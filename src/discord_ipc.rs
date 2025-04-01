#[cfg(target_os = "windows")]
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    thread,
    time::Duration,
};

#[cfg(target_os = "windows")]
use serde_json::Value;

#[cfg(target_os = "windows")]
use uuid::Uuid;

#[cfg(target_os = "windows")]
const PIPE_PATH: &str = r"\\.\pipe\discord-ipc-0";

#[cfg(target_os = "windows")]
pub fn connect() -> std::io::Result<File> {
    for attempt in 1..=10 {
        match OpenOptions::new().read(true).write(true).open(PIPE_PATH) {
            Ok(file) => {
                println!("Connected to Discord IPC pipe.");
                return Ok(file);
            }
            Err(e) => {
                if attempt == 10 {

                    eprintln!("Failed to connect after 10 attempts: {:?}", e);
                    return Err(e);
                }
                eprintln!(
                    "Attempt {} failed to connect to Discord IPC: {:?}. Retrying...",
                    attempt, e
                );
                thread::sleep(Duration::from_millis(500));
            }
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Failed to connect after retries",
    ))
}

#[cfg(target_os = "windows")]
fn write_message(file: &mut File, opcode: u32, payload: &str) -> std::io::Result<()> {
    let payload_bytes = payload.as_bytes();
    let length = payload_bytes.len() as u32;

    let mut header = Vec::with_capacity(8);
    header.extend_from_slice(&opcode.to_le_bytes());
    header.extend_from_slice(&length.to_le_bytes());

    file.write_all(&header)?;
    file.write_all(payload_bytes)?;
    file.flush()?; 
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn read_message(file: &mut File) -> std::io::Result<(u32, String)> {
    let mut header = [0u8; 8];

    file.read_exact(&mut header)?; 

    let opcode = u32::from_le_bytes(header[0..4].try_into().unwrap());
    let length = u32::from_le_bytes(header[4..8].try_into().unwrap());

    if length > 0 {
        let mut payload_bytes = vec![0u8; length as usize];
        file.read_exact(&mut payload_bytes)?; 

        let payload = String::from_utf8(payload_bytes).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e)
        })?;

        Ok((opcode, payload))
    } else {

        Ok((opcode, String::new()))
    }
}

#[cfg(target_os = "windows")]
pub fn send_handshake(file: &mut File, client_id: &str) -> std::io::Result<()> {
    let handshake_payload = serde_json::json!({
        "v": 1,
        "client_id": client_id
    });
    let handshake_str = serde_json::to_string(&handshake_payload)?;
    write_message(file, 0, &handshake_str) 
}

#[cfg(target_os = "windows")]
pub fn set_activity(
    file: &mut File,
    pid: u32,
    activity_json_str: &str,
) -> std::io::Result<()> {

    let activity_value: Value = serde_json::from_str(activity_json_str).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Invalid activity JSON passed to set_activity: {}", e),
        )
    })?;

    let nonce = Uuid::new_v4().to_string();

    let command_payload = serde_json::json!({
        "cmd": "SET_ACTIVITY",
        "args": {
            "pid": pid,
            "activity": activity_value
        },
        "nonce": nonce
    });

    let payload_string = serde_json::to_string(&command_payload)?;
    write_message(file, 1, &payload_string) 
}