
use anyhow::Result;
use bytes::{Buf, BytesMut};
use esp_wokwi_server::{SimulationPacket, GdbInstruction};
use futures_util::future::try_join_all;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::{io::AsyncWriteExt, spawn};
use tokio_tungstenite::accept_async;

use structopt::StructOpt;

const PORT: u16 = 9012;
const GDB_PORT: u16 = 9333;

#[derive(StructOpt, Debug, Clone)]
#[structopt(name = "ESP Wokwi server")]
struct Opts {
    /// Files to process
    #[structopt(name = "FILES", parse(from_os_str))]
    files: Vec<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let (wsend, wrecv) = tokio::sync::mpsc::channel(1);
    let (gsend, grecv) = tokio::sync::mpsc::channel(1);
    let main_wss = spawn(wokwi_task(gsend, wrecv));
    let gdb = spawn(gdb_task(wsend, grecv));

    try_join_all([main_wss, gdb]).await?;

    Ok(())
}

async fn wokwi_task(mut send: Sender<String>, mut recv: Receiver<GdbInstruction>) -> Result<()> {
    let server = TcpListener::bind(("127.0.0.1", PORT)).await?;
    // TODO can we change the target in this URL?
    println!("Open the following link in the browser\r\n\r\nhttps://wokwi.com/_alpha/wembed/327866241856307794?partner=espressif&port={}&data=demo", PORT);

    while let Ok((stream, _)) = server.accept().await {
        process(stream, (&mut send, &mut recv)).await?; // only one connection at atime
    }
    Ok(())
}

async fn process(
    stream: TcpStream,
    (send, recv): (&mut Sender<String>, &mut Receiver<GdbInstruction>),
) -> Result<()> {
    let opts = Opts::from_args();
    let websocket = accept_async(stream).await?;
    let (mut outgoing, mut incoming) = websocket.split();
    let msg = incoming.next().await; // await for hello message
    println!("Client connected: {:?}", msg);

    let simdata = SimulationPacket {
        r#type: "start".to_owned(),
        elf: base64::encode(tokio::fs::read(&opts.files[0]).await?),
        esp_bin: vec![
            vec![
                Value::Number(0x1000.into()),
                Value::String(base64::encode(tokio::fs::read(&opts.files[1]).await?)),
            ],
            vec![
                Value::Number(0x8000.into()),
                Value::String(base64::encode(tokio::fs::read(&opts.files[2]).await?)),
            ],
            vec![
                Value::Number(0x10000.into()),
                Value::String(base64::encode(tokio::fs::read(&opts.files[3]).await?)),
            ],
        ],
    };

    // send the simulation data
    outgoing
        .send(tungstenite::Message::Text(serde_json::to_string(&simdata)?))
        .await?;

    loop {
        tokio::select! {
            Some(msg) = incoming.next() => {
                let msg = msg?;
                if msg.is_text() {
                    let v: Value = serde_json::from_str(msg.to_text()?)?;
                    match &v["type"] {
                        Value::String(s) if s == "uartData" => {
                            if let Value::Array(bytes) = &v["bytes"] {
                                let bytes: Vec<u8> =
                                    bytes.iter().map(|v| v.as_u64().unwrap() as u8).collect();
                                tokio::io::stdout().write_all(&bytes).await?;
                            }
                        }
                        Value::String(s) if s == "gdbResponse" => {
                            let s = v["response"].as_str().unwrap();
                            send.send(s.to_owned()).await?;
                        }
                        _ => unreachable!(),
                    }
                }
            },
            Some(command) = recv.recv() => {
                match command {
                    GdbInstruction::Command(s) => {
                        outgoing
                            .send(tungstenite::Message::Text(serde_json::to_string(
                                &json!({
                                    "type": "gdb",
                                    "message": s
                                }))?
                            )).await?;
                    },
                    GdbInstruction::Break => {
                        outgoing
                            .send(tungstenite::Message::Text(serde_json::to_string(
                                &json!({
                                    "type": "gdbBreak"
                                }))?
                            )).await?;
                    },
                }
            }
        }
    }
}

async fn gdb_task(mut send: Sender<GdbInstruction>, mut recv: Receiver<String>) -> Result<()> {
    let server = TcpListener::bind(("127.0.0.1", GDB_PORT)).await?;
    println!("GDB SERVER LISTENING");

    while let Ok((stream, _)) = server.accept().await {
        println!("GDB client connected");
        match handle_gdb_client(stream, &mut send, &mut recv).await {
            Ok(_) => println!("GDB Session ended cleanly."),
            Err(e) => println!("GDB Session ended with error: {:?}", e),
        }
    }
    Ok(())
}

async fn handle_gdb_client(
    mut stream: TcpStream,
    send: &mut Sender<GdbInstruction>,
    recv: &mut Receiver<String>,
) -> Result<()> {
    stream.write_all(b"+").await?;

    let mut buffer = BytesMut::with_capacity(1024);
    loop {
        let n = stream.read_buf(&mut buffer).await?; // TODO timeout on disconnect?

        let mut bytes = buffer.clone().take(n);
        buffer.advance(n);
        let bytes = bytes.get_mut();

        if bytes.len() == 0 {
            anyhow::bail!("GDB End of stream");
        }

        if bytes[0] == 3 {
            println!("GDB BREAK");
            send.send(GdbInstruction::Break).await?;
            bytes.advance(1);
        }
        let raw_command = String::from_utf8_lossy(bytes);
        let start = raw_command.find("$").map(|i| i + 1); // we want everything after the $
        let end = raw_command.find("#");

        match (start, end) {
            (Some(start), Some(end)) => {
                let command = &raw_command[start..end];
                let checksum = &raw_command[end + 1..];
                if gdb_checksum(command, checksum).is_err() {
                    stream.write_all(b"-").await?;
                    continue;
                } else {
                    stream.write_all(b"+").await?;
                    send.send(GdbInstruction::Command(command.to_owned()))
                        .await?;

                    let resp = recv
                        .recv()
                        .await
                        .ok_or_else(|| anyhow::anyhow!("Channel closed unexpectedly"))?;
                    stream.write_all(resp.as_bytes()).await?;
                }
            }
            _ => continue,
        }
    }
}

fn gdb_checksum(cmd: &str, checksum: &str) -> Result<()> {
    let cs = cmd.as_bytes().iter().map(|&n| n as u16).sum::<u16>() & 0xff;
    let cs = format!("{:02x}", cs);
    if cs != checksum {
        println!("Invalid checksum, expected {}, calculated {}", checksum, cs);
        anyhow::bail!("Invalid checksum, expected {}, calculated {}", checksum, cs);
    }
    Ok(())
}
