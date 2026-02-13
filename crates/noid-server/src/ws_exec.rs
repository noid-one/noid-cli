use noid_core::db::UserRecord;
use noid_types::{ExecRequest, CHANNEL_STDOUT};
use std::io::{Read, Write};
use std::sync::Arc;
use tungstenite::protocol::Message;

use crate::ServerState;

pub fn handle_exec_ws<S: Read + Write>(
    stream: S,
    state: &Arc<ServerState>,
    user: &UserRecord,
    vm_name: &str,
) {
    let mut ws =
        tungstenite::WebSocket::from_raw_socket(stream, tungstenite::protocol::Role::Server, None);

    // Read the ExecRequest (first text frame)
    let exec_req: ExecRequest = match ws.read() {
        Ok(Message::Text(text)) => match serde_json::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                let _ = ws.send(Message::Text(
                    serde_json::to_string(&noid_types::ErrorResponse {
                        error: format!("invalid exec request: {e}"),
                    })
                    .unwrap(),
                ));
                let _ = ws.close(None);
                return;
            }
        },
        _ => {
            let _ = ws.close(None);
            return;
        }
    };

    if exec_req.command.is_empty() {
        let _ = ws.send(Message::Text(
            serde_json::to_string(&noid_types::ErrorResponse {
                error: "command cannot be empty".into(),
            })
            .unwrap(),
        ));
        let _ = ws.close(None);
        return;
    }

    // Execute and stream results
    // For now, use the synchronous exec_full and send the output as a single chunk.
    // A true streaming implementation would require refactoring exec_via_serial.
    match state
        .backend
        .exec_full(&user.id, vm_name, &exec_req.command)
    {
        Ok((stdout, result)) => {
            // Send output as binary frame with CHANNEL_STDOUT prefix
            if !stdout.is_empty() {
                let mut frame = Vec::with_capacity(1 + stdout.len());
                frame.push(CHANNEL_STDOUT);
                frame.extend_from_slice(stdout.as_bytes());
                let _ = ws.send(Message::Binary(frame));
            }

            // Send ExecResult as text frame
            let result_json = serde_json::to_string(&result).unwrap();
            let _ = ws.send(Message::Text(result_json));
        }
        Err(e) => {
            let _ = ws.send(Message::Text(
                serde_json::to_string(&noid_types::ErrorResponse {
                    error: e.to_string(),
                })
                .unwrap(),
            ));
        }
    }

    let _ = ws.close(None);
}
