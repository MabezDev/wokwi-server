use anyhow::Result;
use bytes::{Buf, BytesMut};
use futures_util::future::try_join_all;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::{io::AsyncWriteExt, spawn};
use tokio_tungstenite::accept_async;
use wokwi_server::{GdbInstruction, SimulationPacket};

use espflash::elf::FirmwareImageBuilder;
use espflash::{Chip, FlashSize, PartitionTable};

const PORT: u16 = 9012;
const GDB_PORT: u16 = 9333;

use clap::Parser;

/// Wokwi server
#[derive(Parser, Debug, Clone)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long, env = "WOKWI_HOST")]
    host: Option<String>,

    /// chip name
    #[clap(short, long)]
    chip: Chip,

    /// path to bootloader
    #[clap(short, long)]
    bootloader: Option<PathBuf>,

    /// path to partition table csv
    #[clap(short, long)]
    partition_table: Option<PathBuf>,

    /// wokwi project id
    #[clap(short, long)]
    id: Option<String>,

    elf: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let opts = Args::parse();

    let (wsend, wrecv) = tokio::sync::mpsc::channel(1);
    let (gsend, grecv) = tokio::sync::mpsc::channel(1);

    let main_wss = spawn(wokwi_task(opts, gsend, wrecv));
    let gdb = spawn(gdb_task(wsend, grecv));

    try_join_all([main_wss, gdb]).await?;

    Ok(())
}

async fn wokwi_task(
    opts: Args,
    mut send: Sender<String>,
    mut recv: Receiver<GdbInstruction>,
) -> Result<()> {
    let server = TcpListener::bind(("127.0.0.1", PORT)).await?;

    let project_id = match opts.id.clone() {
        Some(id) => id,
        None => {
            match opts.chip {
                Chip::Esp32 => "331362827438654036".to_string(),
                Chip::Esp32s2 => "332188085821375060".to_string(),
                Chip::Esp32c3 => "332188235906155092".to_string(),
                _ => anyhow::bail!("Chip not supported in Wokwi. Refer to https://docs.wokwi.com/guides/esp32#simulation-features"),
            }
        }
    };

    let mut url = format!(
        "https://wokwi.com/_alpha/wembed/{}?partner=espressif&port={}&data=demo",
        project_id,
        PORT
    );

    if let Some(h) = opts.host.as_ref() { url.push_str(&format!("&_host={}",h)) }

    println!(
        "Open the following link in the browser\r\n\r\n{}\r\n\r\n",
        url
    );
    opener::open_browser(url).ok(); // we don't care if this fails

    while let Ok((stream, _)) = server.accept().await {
        if let Err(e) = process(opts.clone(), stream, (&mut send, &mut recv)).await {
            // only one connection at a time
            println!("Woki websocket closed, error: {:?}", e);
        }
    }
    Ok(())
}

async fn process(
    opts: Args,
    stream: TcpStream,
    (send, recv): (&mut Sender<String>, &mut Receiver<GdbInstruction>),
) -> Result<()> {
    let websocket = accept_async(stream).await?;
    let (mut outgoing, mut incoming) = websocket.split();
    let msg = incoming.next().await; // await for hello message
    println!("Client connected: {:?}", msg);

    let elf = tokio::fs::read(&opts.elf).await?;
    let firmware = FirmwareImageBuilder::new(&elf)
        .flash_size(Some(FlashSize::Flash4Mb)) // TODO make configurable
        .build()?;

    let p = if let Some(p) = &opts.partition_table {
        Some(PartitionTable::try_from_str(String::from_utf8_lossy(
            &tokio::fs::read(p).await?,
        ))?)
    } else {
        None
    };

    let b = if let Some(b) = &opts.bootloader {
        Some(tokio::fs::read(b).await?)
    } else {
        None
    };

    let image = opts.chip.get_flash_image(&firmware, b, p, None, None)?;
    let parts: Vec<_> = image.flash_segments().collect();

    let bootloader = &parts[0];
    let partition_table = &parts[1];
    let app = &parts[2];

    let simdata = SimulationPacket {
        r#type: "start".to_owned(),
        elf: base64::encode(elf.clone()),
        esp_bin: vec![
            vec![
                Value::Number(bootloader.addr.into()),
                Value::String(base64::encode(&bootloader.data)),
            ],
            vec![
                Value::Number(partition_table.addr.into()),
                Value::String(base64::encode(&partition_table.data)),
            ],
            vec![
                Value::Number(app.addr.into()),
                Value::String(base64::encode(&app.data)),
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

        if bytes.is_empty() {
            anyhow::bail!("GDB End of stream");
        }

        if bytes[0] == 3 {
            println!("GDB BREAK");
            send.send(GdbInstruction::Break).await?;
            bytes.advance(1);
        }
        let raw_command = String::from_utf8_lossy(bytes);
        let start = raw_command.find('$').map(|i| i + 1); // we want everything after the $
        let end = raw_command.find('#');

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
