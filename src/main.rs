use anyhow::Context;
use anyhow::Result;
use bytes::{Buf, BytesMut};
use esp_idf_part::PartitionTable;
use espflash::elf::ElfFirmwareImage;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::signal;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::task::JoinSet;
use tokio_tungstenite::accept_async;
use wokwi_server::{GdbInstruction, SimulationPacket};

use espflash::targets::Chip;

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
async fn main() -> Result<(), anyhow::Error> {
    #[cfg(feature = "tokio-console")]
    console_subscriber::init();

    let opts = Args::parse();
    if !matches!(
        opts.chip,
        Chip::Esp32 | Chip::Esp32c3 | Chip::Esp32s2 | Chip::Esp32s3
    ) {
        anyhow::bail!("Chip not supported in Wokwi. See available chips and features at https://docs.wokwi.com/guides/esp32#simulation-features");
    }

    if !opts.elf.exists() {
        anyhow::bail!("Path to elf does not exist");
    }

    if let Some(bt) = &opts.bootloader {
        if !bt.exists() {
            anyhow::bail!("Path to bootloader does not exist");
        } 
    }

    if let Some(pt) = &opts.partition_table {
        if !pt.exists() {
            anyhow::bail!("Path to partition table does not exist");
        } 
    }

    let (wsend, wrecv) = tokio::sync::mpsc::channel(1);
    let (gsend, grecv) = tokio::sync::mpsc::channel(1);

    let mut set = JoinSet::new();
    set.spawn(wokwi_task(opts, gsend, wrecv));
    set.spawn(gdb_task(wsend, grecv));

    loop {
        tokio::select! {
            _ = signal::ctrl_c() => {
                set.shutdown().await;
                break;
            },
            task = set.join_next() => {
                match task {
                    Some(Err(join_error)) => {
                        println!("Task failed: {:?}", join_error);
                        set.shutdown().await;
                        break;
                    }
                    Some(Ok(Err(task_error))) => {
                        println!("Task failed: {:?}", task_error);
                        set.shutdown().await;
                        break;
                    }
                    Some(Ok(_)) => {} /* Task gracefully shutdown */
                    None => break, /* All tasks completed */
                }
            }
        }
    }
    Ok(())
}

async fn wokwi_task(
    opts: Args,
    mut send: Sender<String>,
    mut recv: Receiver<GdbInstruction>,
) -> Result<()> {
    let server = TcpListener::bind(("127.0.0.1", PORT))
        .await
        .with_context(|| format!("Failed to listen on 127.0.0.1:{}", PORT))?;

    let project_id = match opts.id.clone() {
        Some(id) => id,
        None => match opts.chip {
            Chip::Esp32 => "338154815612781140".to_string(),
            Chip::Esp32s2 => "338154940543271506".to_string(),
            Chip::Esp32c3 => "338322025101656660".to_string(),
            Chip::Esp32s3 => "345144250522927698".to_string(),
            _ => unreachable!(),
        },
    };

    let mut url = format!(
        "https://wokwi.com/_alpha/wembed/{}?partner=espressif&port={}&data=demo",
        project_id, PORT
    );

    if let Some(h) = opts.host.as_ref() {
        url.push_str(&format!("&_host={}", h))
    }

    println!(
        "Open the following link in the browser\r\n\r\n{}\r\n\r\n",
        url
    );
    opener::open_browser(url).ok(); // we don't care if this fails

    loop {
        let (stream, _) = server.accept().await?;
        process(opts.clone(), stream, (&mut send, &mut recv)).await?;
    }
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

    let bytes = tokio::fs::read(&opts.elf).await?;
    let elf = xmas_elf::ElfFile::new(&bytes).expect("Invalid elf file");
    let firmware = ElfFirmwareImage::new(elf);

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

    // TODO allow setting flash params, or take from bootloader?
    let image = opts
        .chip
        .into_target()
        .get_flash_image(&firmware, b, p, None, None, None, None, None)?;
    let parts: Vec<_> = image.flash_segments().collect();

    let bootloader = &parts[0];
    let partition_table = &parts[1];
    let app = &parts[2];

    let simdata = SimulationPacket {
        r#type: "start".to_owned(),
        elf: base64::encode(&bytes),
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
    loop {
        let (stream, _) = server.accept().await?;
        println!("GDB client connected.");
        match handle_gdb_client(stream, &mut send, &mut recv).await {
            Ok(_) => println!("GDB Session ended cleanly."),
            Err(e) => println!("GDB Session ended with error: {:?}", e),
        }
    }
}

async fn handle_gdb_client(
    mut stream: TcpStream,
    send: &mut Sender<GdbInstruction>,
    recv: &mut Receiver<String>,
) -> Result<()> {
    stream.write_all(b"+").await?;

    let mut buffer = BytesMut::with_capacity(1024);
    loop {
        tokio::select! {
            r = stream.read_buf(&mut buffer) => {
                let n = r?;

                if n == 0 {
                    anyhow::bail!("GDB End of stream");
                }

                loop {
                    let raw_command = String::from_utf8_lossy(buffer.as_ref());
                    let start = raw_command.find('$').map(|i| i + 1); // we want everything after the $
                    let end = raw_command.find('#');

                    match (start, end) {
                        (Some(start), Some(end)) => {
                            let command = &raw_command[start..end];
                            let end = end + 1; // move past #
                            let checksum = &raw_command[end..];
                            // println!("Command: {}, checksum: {}", command, checksum);
                            let len = if gdb_checksum(command, checksum).is_err() {
                                stream.write_all(b"-").await?;
                                end
                            } else {
                                stream.write_all(b"+").await?;
                                send.send(GdbInstruction::Command(command.to_owned()))
                                    .await?;
                                end + 2
                            };
                            buffer.advance(len);
                        }
                        (None, Some(end)) => buffer.advance(end), /* partial command, discard */
                        (Some(_), None) => break,                 /* incomplete, need more data */
                        (None, None) => {
                            if let Some(_index) = buffer.iter().position(|&x| x == 0x03) {
                                // println!("GDB BREAK detected in packet at index {}", index);
                                send.send(GdbInstruction::Break).await?;
                            }
                            buffer.advance(buffer.remaining()); /* garbage */
                            break;
                        }
                    }
                }
            }
            resp = recv.recv() => {
                let resp = resp.ok_or_else(|| anyhow::anyhow!("Channel closed unexpectedly"))?;
                stream.write_all(resp.as_bytes()).await?;
            }
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
