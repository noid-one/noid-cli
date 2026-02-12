use noid_core::backend;
use noid_core::db::UserRecord;
use noid_types::{CHANNEL_STDIN, CHANNEL_STDOUT};
use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;
use tungstenite::protocol::Message;

use crate::ServerState;

pub fn handle_console_ws<S: Read + Write>(
    stream: S,
    state: &Arc<ServerState>,
    user: &UserRecord,
    vm_name: &str,
) {
    let handle = match state.backend.console_attach(&user.id, vm_name) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("console attach failed: {e}");
            return;
        }
    };

    let mut ws = tungstenite::WebSocket::from_raw_socket(
        stream,
        tungstenite::protocol::Role::Server,
        None,
    );

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

        // Check for incoming WS messages (non-blocking via peek)
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
            Err(tungstenite::Error::Io(ref e))
                if e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }

    running.store(false, std::sync::atomic::Ordering::Relaxed);
    let _ = ws.close(None);
    let _ = reader_thread.join();
}
