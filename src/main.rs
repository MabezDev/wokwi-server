use anyhow::Result;
use esp_wokwi_server::SimulationPacket;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::fs::File;
use std::io::Read;
use tokio::net::{TcpListener, TcpStream};
use tokio::{spawn, io::AsyncWriteExt};
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
    let opts = Opts::from_args();
    println!("{:?}", opts);

    let main_wss = spawn(wokwi_main());

    
    main_wss.await??;
    // TODO GDB thread

    Ok(())
}

async fn wokwi_main() -> Result<()> {
    let server = TcpListener::bind(("127.0.0.1", PORT)).await?;
    // TODO can we change the target in this URL
    println!("Open the following link in the browser\r\n\r\nhttps://wokwi.com/_alpha/wembed/327866241856307794?partner=espressif&port={}&data=demo", PORT);

    while let Ok((stream, _)) = server.accept().await {
        spawn(process(stream));
    }
    Ok(())
}

async fn process(stream: TcpStream) -> Result<()> {
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
        if let Some(msg) = incoming.next().await {
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
                        println!("{:?}", msg);
                        todo!();
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
}

async fn gdb_main() -> Result<()> {

    Ok(())
}
