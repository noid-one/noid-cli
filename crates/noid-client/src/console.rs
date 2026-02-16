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

pub fn attach_console(api: &ApiClient, vm_name: &str, env: &[String]) -> Result<()> {
    let mut ws = api
        .ws_connect(
            &format!("/v1/vms/{vm_name}/console"),
            Duration::from_secs(10),
        )
        .context("failed to connect to console WebSocket")?;

    println!("Attached to '{vm_name}' serial console.");
    println!("Type 'exit' to detach.");

    terminal::enable_raw_mode().context("failed to enable raw terminal mode")?;

    let mut stdout = std::io::stdout();

    // Enable bracketed paste so multi-char pastes arrive as a single Event::Paste
    let _ = crossterm::execute!(stdout, crossterm::event::EnableBracketedPaste);

    // Inject env vars before entering the main loop
    if !env.is_empty() {
        // Temporarily set blocking for reliable sends
        set_ws_nonblocking(&mut ws, false);
        for env_str in env {
            if let Some((key, value)) = env_str.split_once('=') {
                // Defensive: validate env name (should already be validated by caller)
                if !noid_types::validate_env_name(key) {
                    continue;
                }
                let escaped = value.replace('\'', "'\\''");
                // Leading space prevents command from appearing in shell history
                let cmd = format!(" export {key}='{escaped}'\r");
                send_stdin(&mut ws, cmd.as_bytes());
            }
        }
        // Wait for a sync marker to ensure all export commands are processed
        // before user input begins. Without this, rapid typing can interleave
        // with the exports, causing missing env vars or corrupted shell state.
        // Uses a timestamped marker to avoid false matches from shell output.
        let sync_marker = format!(
            "__NOID_ENV_SYNC_{:x}__",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        send_stdin(
            &mut ws,
            format!(" echo {sync_marker}\r").as_bytes(),
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut sync_buf = Vec::new();
        let mut synced = false;
        while std::time::Instant::now() < deadline {
            match ws.read() {
                Ok(Message::Binary(data)) => {
                    if !data.is_empty() && data[0] == CHANNEL_STDOUT {
                        sync_buf.extend_from_slice(&data[1..]);
                        if sync_buf
                            .windows(sync_marker.len())
                            .any(|w| w == sync_marker.as_bytes())
                        {
                            synced = true;
                            break;
                        }
                    }
                }
                Ok(Message::Ping(data)) => {
                    let _ = ws.send(Message::Pong(data));
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(ref e))
                    if e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        if !synced {
            // Raw mode is active, so use \r\n for correct terminal output
            let _ = stdout.write_all(b"\r\nWarning: env var sync timed out; vars may not be set yet.\r\n");
            let _ = stdout.flush();
        }
    }

    // Line buffer for "exit" detection
    let mut line_buffer = String::new();

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
                    // Bracketed paste: send entire pasted text as one frame.
                    // Translate newlines to CR (what real terminals send for Enter).
                    // Normalize \r\n first to avoid double-CR.
                    if !text.is_empty() {
                        let translated = text.replace("\r\n", "\r").replace('\n', "\r");

                        // Check if any line in the pasted text is "exit"
                        let lines: Vec<&str> = translated.split('\r').collect();
                        for (i, line) in lines.iter().enumerate() {
                            let is_last = i == lines.len() - 1;
                            if !is_last && line.trim() == "exit" {
                                // Send Ctrl+U to clear the VM's input line, then detach
                                let _ = send_stdin(&mut ws, b"\x15");
                                set_ws_nonblocking(&mut ws, false);
                                let _ = ws.send(Message::Close(None));
                                let _ = ws.close(None);
                                let _ = crossterm::execute!(
                                    stdout,
                                    crossterm::event::DisableBracketedPaste
                                );
                                terminal::disable_raw_mode()?;
                                println!("\r\n--- Detached ---");
                                return Ok(());
                            }
                        }

                        if !send_stdin(&mut ws, translated.as_bytes()) {
                            break;
                        }
                        // Update line_buffer with the last incomplete line
                        if let Some(last) = lines.last() {
                            if translated.ends_with('\r') {
                                line_buffer.clear();
                            } else {
                                line_buffer = last.to_string();
                            }
                        }
                    }
                }
                Event::Key(key) => {
                    // Normal key handling
                    if let Some(bytes) = key_to_bytes(&key) {
                        // Track line buffer for "exit" detection
                        match key.code {
                            KeyCode::Char(c)
                                if !key
                                    .modifiers
                                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                            {
                                line_buffer.push(c);
                            }
                            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                // Ctrl+U (clear line) and Ctrl+C (interrupt) both abandon the current line
                                if c == 'u' || c == 'c' {
                                    line_buffer.clear();
                                }
                            }
                            KeyCode::Backspace => {
                                line_buffer.pop();
                            }
                            KeyCode::Enter => {
                                if line_buffer.trim() == "exit" {
                                    // Send Ctrl+U to clear any buffered input in the VM's shell
                                    // before we detach, preventing the "exit" from executing
                                    let _ = send_stdin(&mut ws, b"\x15");
                                    break;
                                }
                                line_buffer.clear();
                            }
                            _ => {
                                // Arrows, Tab, etc. break simple line assumption
                                line_buffer.clear();
                            }
                        }

                        if !send_stdin(&mut ws, &bytes) {
                            break;
                        }
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
    match ws.get_mut() {
        MaybeTlsStream::Plain(stream) => {
            let _ = stream.set_nonblocking(nonblocking);
        }
        MaybeTlsStream::Rustls(tls_stream) => {
            let _ = tls_stream.get_mut().set_nonblocking(nonblocking);
        }
        _ => {
            // NativeTls or __Unsupported variants.
            // Since we only enable rustls-tls-webpki-roots, we should only see Plain/Rustls.
            // Log a warning in debug builds if we encounter an unexpected variant.
            #[cfg(debug_assertions)]
            eprintln!("Warning: set_ws_nonblocking called on unsupported stream type");
        }
    }
}

fn key_to_bytes(key: &KeyEvent) -> Option<Vec<u8>> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char(c) => {
                // Ctrl+A = 0x01, Ctrl+B = 0x02, ... Ctrl+Z = 0x1A
                // Handle both upper and lowercase
                let lower = c.to_ascii_lowercase();
                if lower.is_ascii_lowercase() {
                    let ctrl = (lower as u8) - b'a' + 1;
                    Some(vec![ctrl])
                } else {
                    None
                }
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
            KeyCode::Enter => Some(b"\r".to_vec()),
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
