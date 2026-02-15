use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;
use std::io::{self, Seek, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::vm;

/// Attach to VM serial console — bidirectional pipe bridge.
///
/// Reads serial output from serial.log (FC stdout) and writes
/// keystrokes to FC stdin via serial.in FIFO.
pub fn attach_console(vm_name: &str) -> Result<()> {
    let serial_path = vm::serial_log_path(vm_name);
    if !serial_path.exists() {
        anyhow::bail!("serial.log not found for VM '{vm_name}' — is it running?");
    }

    println!("Attached to '{vm_name}' serial console. Press Ctrl+] to detach (or type 'exit').");

    terminal::enable_raw_mode().context("failed to enable raw terminal mode")?;

    let running = Arc::new(AtomicBool::new(true));

    // Reader thread: tail serial.log and print new output
    let running_r = running.clone();
    let serial_path_clone = serial_path.clone();
    let reader = std::thread::spawn(move || {
        let mut file = match std::fs::File::open(&serial_path_clone) {
            Ok(f) => f,
            Err(_) => return,
        };
        // Seek to end so we only see new output
        let _ = file.seek(io::SeekFrom::End(0));
        let mut buf = [0u8; 4096];
        while running_r.load(Ordering::Relaxed) {
            match std::io::Read::read(&mut file, &mut buf) {
                Ok(0) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(n) => {
                    let mut stdout = io::stdout().lock();
                    let _ = stdout.write_all(&buf[..n]);
                    let _ = stdout.flush();
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    });

    let vm_name_owned = vm_name.to_string();

    // Line buffer for "exit" detection
    let mut line_buffer = String::new();

    // Main loop: read keystrokes, send to VM serial input
    loop {
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                // Ctrl+] to detach
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(']') {
                    break;
                }

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
                                let _ = vm::write_to_serial(&vm_name_owned, b"\x15");
                                break;
                            }
                            line_buffer.clear();
                        }
                        _ => {
                            // Arrows, Tab, etc. break simple line assumption
                            line_buffer.clear();
                        }
                    }

                    if let Err(e) = vm::write_to_serial(&vm_name_owned, &bytes) {
                        let mut stdout = io::stdout().lock();
                        let _ = writeln!(stdout, "\r\n[serial write error: {e}]");
                        break;
                    }
                }
            }
        }
    }

    running.store(false, Ordering::Relaxed);
    terminal::disable_raw_mode()?;
    let _ = reader.join();

    println!("\r\n--- Detached ---");
    Ok(())
}

/// Convert a crossterm KeyEvent into bytes to send to the serial console
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
