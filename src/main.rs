use anyhow::Result;
use esp_wokwi_server::SimulationPacket;
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::spawn;
use tungstenite::accept;

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

fn main() -> Result<()> {
    let opts = Opts::from_args();
    println!("{:?}", opts);

    let server = TcpListener::bind(("127.0.0.1", PORT))?;
    // TODO can we change the target in this URL
    println!("Open the following link in the browser\r\n\r\nhttps://wokwi.com/_alpha/wembed/327866241856307794?partner=espressif&port={}&data=demo", PORT);

    for stream in server.incoming() {
        spawn(|| {
            let handler = move || -> Result<()> {
                let opts = Opts::from_args();
                let mut websocket = accept(stream?)?;
                let msg = websocket.read_message()?; // await for hello message
                println!("Client connected: {:?}", msg);

                let simdata = SimulationPacket {
                    r#type: "start".to_owned(),
                    elf: base64::encode(read_to_end(&opts.files[0])?),
                    esp_bin: vec![
                        vec![
                            Value::Number(0x1000.into()),
                            Value::String(base64::encode(read_to_end(&opts.files[1])?)),
                        ],
                        vec![
                            Value::Number(0x8000.into()),
                            Value::String(base64::encode(read_to_end(&opts.files[2])?)),
                        ],
                        vec![
                            Value::Number(0x10000.into()),
                            Value::String(base64::encode(read_to_end(&opts.files[3])?)),
                        ],
                    ],
                };

                // send the simulation data
                websocket
                    .write_message(tungstenite::Message::Text(serde_json::to_string(&simdata)?))?;

                loop {
                    let msg = websocket.read_message()?;

                    if msg.is_text() {
                        let v: Value = serde_json::from_str(msg.to_text()?)?;
                        match &v["type"] {
                            Value::String(s) if s == "uartData" => {
                                if let Value::Array(bytes) = &v["bytes"] {
                                    let bytes: Vec<u8> = bytes.iter().map(|v| v.as_u64().unwrap() as u8).collect();
                                    std::io::stdout().write_all(&bytes)?;
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
            };

            handler().unwrap(); // panic with the thread message
        });
    }

    Ok(())
}

fn read_to_end<P: AsRef<std::path::Path>>(path: P) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut vec = Vec::new();
    file.read_to_end(&mut vec)?;

    Ok(vec)
}
