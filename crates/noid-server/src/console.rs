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
        while running_r.load(std::sync::atomic::Ordering::Relaxed) {
            match log_file.read(&mut buf) {
                Ok(0) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(n) => {
                    // Prefix with CHANNEL_STDOUT
                    let mut frame = Vec::with_capacity(1 + n);
                    frame.push(CHANNEL_STDOUT);
                    frame.extend_from_slice(&buf[..n]);
                    if tx.send(frame).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
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
