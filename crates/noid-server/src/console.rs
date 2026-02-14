use noid_core::backend;
use noid_core::db::UserRecord;
use noid_types::{CHANNEL_STDIN, CHANNEL_STDOUT};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tungstenite::protocol::Message;

use crate::ServerState;

pub fn handle_console_ws<S: Read + Write>(
    stream: S,
    state: &Arc<ServerState>,
    user: &UserRecord,
    vm_name: &str,
    remote_addr: Option<SocketAddr>,
) {
    let handle = match state.backend.console_attach(&user.id, vm_name) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("console attach failed: {e}");
            return;
        }
    };

    let mut ws =
        tungstenite::WebSocket::from_raw_socket(stream, tungstenite::protocol::Role::Server, None);

    // Set the underlying socket to non-blocking so ws.read() returns WouldBlock
    // instead of blocking forever. The stream from tiny_http's upgrade() is
    // Box<dyn ReadWrite + Send> with no way to call set_nonblocking() directly,
    // so we find the socket fd by matching the peer address.
    if let Some(peer) = remote_addr {
        if let Some(fd) = find_socket_fd(&peer) {
            set_fd_nonblocking(fd);
        } else {
            eprintln!("[console] warning: could not find socket fd for {peer}, reads will block");
        }
    }

    // Open serial log for reading
    let mut log_file = match backend::console_open_log(&handle) {
        Ok(f) => f,
        Err(e) => {
            let _ = ws.close(None);
            eprintln!("failed to open serial log: {e}");
            return;
        }
    };

    // Set up a reader thread to tail serial.log → WS
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let running_r = running.clone();

    // We can't share the WS between threads with tungstenite easily.
    // Instead, use a channel to send data from the reader thread to the main loop.
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();

    let reader_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut leftover: Vec<u8> = Vec::new();
        let mut empty_reads: u8 = 0;
        const MAX_LEFTOVER: usize = 8192;

        while running_r.load(std::sync::atomic::Ordering::Relaxed) {
            match log_file.read(&mut buf) {
                Ok(0) => {
                    // Flush leftover after 2+ consecutive empty reads (~100ms)
                    // to keep interactive output (keystrokes, progress bars) responsive.
                    // Marker lines always end with \r\n so they're processed as complete lines.
                    empty_reads = empty_reads.saturating_add(1);
                    if !leftover.is_empty() && (empty_reads >= 2 || leftover.len() > MAX_LEFTOVER)
                    {
                        let mut frame = Vec::with_capacity(1 + leftover.len());
                        frame.push(CHANNEL_STDOUT);
                        frame.append(&mut leftover);
                        if tx.send(frame).is_err() {
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(n) => {
                    empty_reads = 0;
                    leftover.extend_from_slice(&buf[..n]);

                    // Find the last newline to split complete lines from partial data
                    let last_nl = leftover.iter().rposition(|&b| b == b'\n');
                    let (complete, remainder) = match last_nl {
                        Some(pos) => {
                            let rest = leftover[pos + 1..].to_vec();
                            leftover.truncate(pos + 1);
                            let complete = std::mem::take(&mut leftover);
                            (complete, rest)
                        }
                        None => {
                            // No newline yet — flush if over limit, otherwise wait
                            if leftover.len() > MAX_LEFTOVER {
                                let data = std::mem::take(&mut leftover);
                                // Still filter oversized buffers to prevent marker leakage
                                if is_exec_marker_line(&data) {
                                    (Vec::new(), Vec::new())
                                } else {
                                    (data, Vec::new())
                                }
                            } else {
                                continue;
                            }
                        }
                    };

                    // Filter complete lines, dropping exec marker lines
                    let mut output = Vec::new();
                    for line in complete.split_inclusive(|&b| b == b'\n') {
                        if !is_exec_marker_line(line) {
                            output.extend_from_slice(line);
                        }
                    }

                    leftover = remainder;

                    if !output.is_empty() {
                        let mut frame = Vec::with_capacity(1 + output.len());
                        frame.push(CHANNEL_STDOUT);
                        frame.extend_from_slice(&output);
                        if tx.send(frame).is_err() {
                            break;
                        }
                    }
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }

        // Flush any remaining partial data before thread exit
        if !leftover.is_empty() {
            let mut frame = Vec::with_capacity(1 + leftover.len());
            frame.push(CHANNEL_STDOUT);
            frame.extend_from_slice(&leftover);
            let _ = tx.send(frame); // ignore errors, we're shutting down
        }
    });

    let timeout = Duration::from_secs(state.config.console_timeout_secs);
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            let _ = ws.close(None);
            break;
        }

        // Check for data from reader thread
        while let Ok(data) = rx.try_recv() {
            if ws.send(Message::Binary(data)).is_err() {
                running.store(false, std::sync::atomic::Ordering::Relaxed);
                let _ = reader_thread.join();
                return;
            }
        }

        // Check for incoming WS messages (non-blocking via O_NONBLOCK on socket)
        match ws.read() {
            Ok(Message::Binary(data)) => {
                if data.is_empty() {
                    continue;
                }
                if data[0] == CHANNEL_STDIN {
                    if let Err(e) = backend::console_write(&handle, &data[1..]) {
                        eprintln!("serial write error: {e}");
                        break;
                    }
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(data)) => {
                let _ = ws.send(Message::Pong(data));
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }

    running.store(false, std::sync::atomic::Ordering::Relaxed);
    let _ = ws.close(None);
    let _ = reader_thread.join();
}

/// Find the socket file descriptor for a given peer address by scanning open fds.
/// This is needed because tiny_http's upgrade() returns Box<dyn ReadWrite + Send>
/// which doesn't expose the raw fd for set_nonblocking().
fn find_socket_fd(peer: &SocketAddr) -> Option<i32> {
    for fd in 3..1024 {
        unsafe {
            let mut addr: libc::sockaddr_storage = std::mem::zeroed();
            let mut len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
            if libc::getpeername(fd, &mut addr as *mut _ as *mut libc::sockaddr, &mut len) != 0 {
                continue;
            }
            let matches = match peer {
                SocketAddr::V4(v4) => {
                    if addr.ss_family as i32 != libc::AF_INET {
                        false
                    } else {
                        let sa = &*(&addr as *const _ as *const libc::sockaddr_in);
                        let port = u16::from_be(sa.sin_port);
                        let ip = std::net::Ipv4Addr::from(u32::from_be(sa.sin_addr.s_addr));
                        port == v4.port() && ip == *v4.ip()
                    }
                }
                SocketAddr::V6(v6) => {
                    if addr.ss_family as i32 != libc::AF_INET6 {
                        false
                    } else {
                        let sa = &*(&addr as *const _ as *const libc::sockaddr_in6);
                        let port = u16::from_be(sa.sin6_port);
                        let ip = std::net::Ipv6Addr::from(sa.sin6_addr.s6_addr);
                        port == v6.port() && ip == *v6.ip()
                    }
                }
            };
            if matches {
                return Some(fd);
            }
        }
    }
    None
}

fn set_fd_nonblocking(fd: i32) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags != -1 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

/// Returns true if `line` is an exec marker token that should be hidden from console.
///
/// After stripping ANSI escapes and trimming whitespace, matches exactly:
/// - `NOID_EXEC_<8 hex>` (start marker)
/// - `NOID_EXEC_<8 hex>_EXIT<digits>` (exit code marker)
/// - `NOID_EXEC_<8 hex>_END` (end marker)
fn is_exec_marker_line(line: &[u8]) -> bool {
    let as_str = String::from_utf8_lossy(line);
    let cleaned = noid_core::exec::strip_ansi(&as_str);
    let trimmed = cleaned.trim();

    let rest = match trimmed.strip_prefix(noid_core::exec::EXEC_MARKER_PREFIX) {
        Some(r) => r,
        None => return false,
    };

    // Need at least 8 hex chars after the prefix
    if rest.len() < 8 || !rest[..8].chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    let after_id = &rest[8..];

    // Exact: just the ID (start marker)
    if after_id.is_empty() {
        return true;
    }
    // _END
    if after_id == "_END" {
        return true;
    }
    // _EXIT followed by one or more digits (max 4 for exit codes 0-255)
    if let Some(digits) = after_id.strip_prefix("_EXIT") {
        return !digits.is_empty()
            && digits.len() <= 4
            && digits.chars().all(|c| c.is_ascii_digit());
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_start_detected() {
        assert!(is_exec_marker_line(b"NOID_EXEC_abcd1234\r\n"));
    }

    #[test]
    fn marker_exit0_detected() {
        assert!(is_exec_marker_line(b"NOID_EXEC_abcd1234_EXIT0\r\n"));
    }

    #[test]
    fn marker_exit255_detected() {
        assert!(is_exec_marker_line(b"NOID_EXEC_abcd1234_EXIT255\r\n"));
    }

    #[test]
    fn marker_end_detected() {
        assert!(is_exec_marker_line(b"NOID_EXEC_abcd1234_END\r\n"));
    }

    #[test]
    fn ansi_wrapped_start_marker_detected() {
        assert!(is_exec_marker_line(
            b"\x1b[32mNOID_EXEC_abcd1234\x1b[0m\r\n"
        ));
    }

    #[test]
    fn ansi_bracketed_paste_marker_detected() {
        assert!(is_exec_marker_line(
            b"\x1b[?2004hNOID_EXEC_abcd1234_END\r\n"
        ));
    }

    #[test]
    fn normal_output_passes_through() {
        assert!(!is_exec_marker_line(b"hello world\r\n"));
    }

    #[test]
    fn command_echo_passes_through() {
        assert!(!is_exec_marker_line(b"echo 'NOID_EXEC_abcd'; ls\r\n"));
    }

    #[test]
    fn embedded_marker_in_output_passes_through() {
        assert!(!is_exec_marker_line(
            b"user printed NOID_EXEC_abcd1234 in output\r\n"
        ));
    }

    #[test]
    fn prompt_passes_through() {
        assert!(!is_exec_marker_line(b"noid@noid:~$ "));
    }

    #[test]
    fn single_keystroke_passes_through() {
        assert!(!is_exec_marker_line(b"h"));
    }

    #[test]
    fn marker_with_short_id_rejected() {
        // Only 4 hex chars — not a valid marker
        assert!(!is_exec_marker_line(b"NOID_EXEC_abcd\r\n"));
    }

    #[test]
    fn marker_exit_no_digits_rejected() {
        assert!(!is_exec_marker_line(b"NOID_EXEC_abcd1234_EXIT\r\n"));
    }

    #[test]
    fn marker_with_trailing_text_rejected() {
        assert!(!is_exec_marker_line(
            b"NOID_EXEC_abcd1234_extra_stuff\r\n"
        ));
    }

    #[test]
    fn marker_exit_excessive_digits_rejected() {
        // Protect against DoS via extremely long exit code sequences
        assert!(!is_exec_marker_line(
            b"NOID_EXEC_abcd1234_EXIT99999\r\n"
        ));
    }
}
