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

/// Send data to the VM's stdin over the WebSocket. Returns false if the send fails.
fn send_stdin(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>, data: &[u8]) -> bool {
    let mut frame = Vec::with_capacity(1 + data.len());
    frame.push(CHANNEL_STDIN);
    frame.extend_from_slice(data);
    set_ws_nonblocking(ws, false);
    let ok = ws.send(Message::Binary(frame)).is_ok();
    set_ws_nonblocking(ws, true);
    ok
}

pub fn attach_console(api: &ApiClient, vm_name: &str) -> Result<()> {
    let mut ws = api
        .ws_connect(
            &format!("/v1/vms/{vm_name}/console"),
            Duration::from_secs(10),
        )
        .context("failed to connect to console WebSocket")?;

    println!("Attached to '{vm_name}' serial console.");
    println!("Use ~. to detach (Enter, then ~, then .)");

    terminal::enable_raw_mode().context("failed to enable raw terminal mode")?;

    let mut stdout = std::io::stdout();

    // Enable bracketed paste so multi-char pastes arrive as a single Event::Paste
    let _ = crossterm::execute!(stdout, crossterm::event::EnableBracketedPaste);

    // SSH-style ~. escape sequence state
    let mut after_newline = true; // starts true so ~. works immediately
    let mut tilde_pending = false;

    // We can't easily share the WS across threads, so we use a single-threaded
    // approach with non-blocking polling.

    // Set the underlying stream to non-blocking if it's a TCP stream
    set_ws_nonblocking(&mut ws, true);

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
            Err(tungstenite::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No data available, continue
            }
            Err(_) => break,
        }

        // Check for keyboard input (non-blocking)
        if event::poll(Duration::from_millis(10))? {
            match event::read()? {
                Event::Paste(text) => {
                    // Bracketed paste: send entire pasted text as one frame
                    if !text.is_empty() {
                        if !send_stdin(&mut ws, text.as_bytes()) {
                            break;
                        }
                        // Update after_newline based on last char of paste
                        after_newline = text.ends_with('\n') || text.ends_with('\r');
                        tilde_pending = false;
                    }
                }
                Event::Key(key) => {
                    // Ctrl+Q to detach (silent fallback, not advertised)
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('q')
                    {
                        break;
                    }

                    // SSH-style ~. escape: only for unmodified keys (or Shift)
                    let has_ctrl_alt = key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);

                    if !has_ctrl_alt {
                        if tilde_pending {
                            tilde_pending = false;
                            match key.code {
                                KeyCode::Char('.') => {
                                    // ~. = detach
                                    break;
                                }
                                KeyCode::Char('~') => {
                                    // ~~ = send one literal ~
                                    if !send_stdin(&mut ws, b"~") {
                                        break;
                                    }
                                    after_newline = false;
                                    continue;
                                }
                                _ => {
                                    // ~<other> = flush buffered ~ then handle char normally
                                    if !send_stdin(&mut ws, b"~") {
                                        break;
                                    }
                                    // Fall through to send the current key below
                                }
                            }
                        } else if after_newline && key.code == KeyCode::Char('~') {
                            // Start of potential escape sequence — buffer the ~
                            tilde_pending = true;
                            continue;
                        }
                    }

                    // Normal key handling
                    if let Some(bytes) = key_to_bytes(&key) {
                        if !send_stdin(&mut ws, &bytes) {
                            break;
                        }
                        // Track newline state
                        after_newline = key.code == KeyCode::Enter;
                        tilde_pending = false;
                    }
                }
                _ => {}
            }
        }
    }

    set_ws_nonblocking(&mut ws, false);
    let _ = ws.close(None);
    let _ = ws.send(Message::Close(None));
    let _ = crossterm::execute!(stdout, crossterm::event::DisableBracketedPaste);
    terminal::disable_raw_mode()?;
    println!("\r\n--- Detached ---");
    Ok(())
}

fn set_ws_nonblocking(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>, nonblocking: bool) {
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
