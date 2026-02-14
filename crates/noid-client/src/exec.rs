use anyhow::{Context, Result};
use noid_types::{ErrorResponse, ExecRequest, ExecResult, CHANNEL_STDERR, CHANNEL_STDOUT};
use std::io::Write;
use std::time::Duration;
use tungstenite::protocol::Message;

use crate::api::ApiClient;

pub fn exec_ws(api: &ApiClient, vm_name: &str, command: &[String], env: &[String]) -> Result<i32> {
    let mut ws = api
        .ws_connect(&format!("/v1/vms/{vm_name}/exec"), Duration::from_secs(10))
        .context("failed to connect to exec WebSocket")?;

    // Send the exec request
    let exec_req = ExecRequest {
        command: command.to_vec(),
        tty: false,
        env: env.to_vec(),
    };
    ws.send(Message::Text(serde_json::to_string(&exec_req)?))?;

    let mut exit_code = 0i32;
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();

    loop {
        match ws.read() {
            Ok(Message::Binary(data)) => {
                if data.is_empty() {
                    continue;
                }
                match data[0] {
                    CHANNEL_STDOUT => {
                        let _ = stdout.write_all(&data[1..]);
                        let _ = stdout.flush();
                    }
                    CHANNEL_STDERR => {
                        let _ = stderr.write_all(&data[1..]);
                        let _ = stderr.flush();
                    }
                    _ => {}
                }
            }
            Ok(Message::Text(text)) => {
                // Could be ExecResult or ErrorResponse
                if let Ok(result) = serde_json::from_str::<ExecResult>(&text) {
                    if result.timed_out {
                        eprintln!("exec timed out");
                        exit_code = 124;
                    } else if let Some(code) = result.exit_code {
                        exit_code = code;
                    }
                    if result.truncated {
                        eprintln!("warning: output was truncated (exceeded 1MB limit)");
                    }
                } else if let Ok(err) = serde_json::from_str::<ErrorResponse>(&text) {
                    eprintln!("error: {}", err.error);
                    exit_code = 1;
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(data)) => {
                let _ = ws.send(Message::Pong(data));
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }

    Ok(exit_code)
}
