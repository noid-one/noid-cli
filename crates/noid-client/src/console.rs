use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;
use noid_types::{CHANNEL_STDIN, CHANNEL_STDOUT};
use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;
use tungstenite::protocol::Message;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::WebSocket;

use crate::api::ApiClient;

pub fn attach_console(api: &ApiClient, vm_name: &str) -> Result<()> {
    let url = api.ws_url(&format!("/v1/vms/{vm_name}/console"));

    let request = tungstenite::http::Request::builder()
        .uri(&url)
        .header("Authorization", format!("Bearer {}", api.token()))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .context("failed to build WS request")?;

    let (mut ws, _) =
        tungstenite::connect(request).context("failed to connect to console WebSocket")?;

    println!("Attached to '{vm_name}' serial console. Press Ctrl+Q to detach.");

    terminal::enable_raw_mode().context("failed to enable raw terminal mode")?;

    // We can't easily share the WS across threads, so we use a single-threaded
    // approach with non-blocking polling.

    // Set the underlying stream to non-blocking if it's a TCP stream
    set_ws_nonblocking(&mut ws, true);

    let mut stdout = std::io::stdout();

    loop {
        // Check for incoming WS messages
        match ws.read() {
            Ok(Message::Binary(data)) => {
                if !data.is_empty() && data[0] == CHANNEL_STDOUT {
                    let _ = stdout.write_all(&data[1..]);
                    let _ = stdout.flush();
                }
            }
            Ok(Message::Ping(data)) => {
                let _ = ws.send(Message::Pong(data));
            }
            Ok(Message::Close(_)) => {
                break;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref e))
                if e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                // No data available, continue
            }
            Err(_) => break,
        }

        // Check for keyboard input (non-blocking)
        if event::poll(Duration::from_millis(10))? {
            if let Event::Key(key) = event::read()? {
                // Ctrl+Q to detach
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.code == KeyCode::Char('q')
                {
                    break;
                }

                if let Some(bytes) = key_to_bytes(&key) {
                    let mut frame = Vec::with_capacity(1 + bytes.len());
                    frame.push(CHANNEL_STDIN);
                    frame.extend_from_slice(&bytes);

                    // Set blocking for sends
                    set_ws_nonblocking(&mut ws, false);
                    if ws.send(Message::Binary(frame)).is_err() {
                        break;
                    }
                    set_ws_nonblocking(&mut ws, true);
                }
            }
        }
    }

    set_ws_nonblocking(&mut ws, false);
    let _ = ws.close(None);
    let _ = ws.send(Message::Close(None));
    terminal::disable_raw_mode()?;
    println!("\r\n--- Detached ---");
    Ok(())
}

fn set_ws_nonblocking(
    ws: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    nonblocking: bool,
) {
    if let MaybeTlsStream::Plain(stream) = ws.get_mut() {
        let _ = stream.set_nonblocking(nonblocking);
    }
}

fn key_to_bytes(key: &KeyEvent) -> Option<Vec<u8>> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char(c) => {
                let ctrl = (c as u8).wrapping_sub(b'a').wrapping_add(1);
                Some(vec![ctrl])
            }
            _ => None,
        }
    } else {
        match key.code {
            KeyCode::Char(c) => {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                Some(s.as_bytes().to_vec())
            }
            KeyCode::Enter => Some(b"\n".to_vec()),
            KeyCode::Backspace => Some(vec![0x7f]),
            KeyCode::Tab => Some(b"\t".to_vec()),
            KeyCode::Esc => Some(vec![0x1b]),
            KeyCode::Up => Some(b"\x1b[A".to_vec()),
            KeyCode::Down => Some(b"\x1b[B".to_vec()),
            KeyCode::Right => Some(b"\x1b[C".to_vec()),
            KeyCode::Left => Some(b"\x1b[D".to_vec()),
            KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
            KeyCode::Home => Some(b"\x1b[H".to_vec()),
            KeyCode::End => Some(b"\x1b[F".to_vec()),
            _ => None,
        }
    }
}
