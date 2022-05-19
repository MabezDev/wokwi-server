
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct SimulationPacket {
    pub r#type: String,
    pub elf: String, // string because we base64 encode the binary data
    #[serde(rename = "espBin")]
    pub esp_bin: Vec<Vec<Value>>,
}

#[derive(Debug)]
pub enum GdbInstruction {
    Command(String),
    Break,
}