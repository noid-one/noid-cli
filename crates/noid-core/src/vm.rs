use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const FIRECRACKER_BIN: &str = "/usr/local/bin/firecracker";

/// Spawn a Firecracker process with serial console I/O via files.
///
/// stdin  = named FIFO at serial.in  (any process can write to it later)
/// stdout = regular file at serial.log (any process can tail it)
///
/// Returns (pid, socket_path).
pub fn spawn_fc(subvol: &Path) -> Result<(u32, String)> {
    let socket_path = subvol.join("firecracker.sock");
    let log_path = subvol.join("firecracker.log");
    let serial_out = subvol.join("serial.log");
    let serial_in = subvol.join("serial.in");

    // Remove stale socket
    let _ = std::fs::remove_file(&socket_path);

    // Create serial output file
    let serial_file =
        std::fs::File::create(&serial_out).context("failed to create serial.log")?;

    // Create named FIFO for serial input (if not already there)
    let _ = std::fs::remove_file(&serial_in);
    nix::unistd::mkfifo(&serial_in, nix::sys::stat::Mode::from_bits_truncate(0o666))
        .context("failed to create serial.in FIFO")?;

    // Open FIFO read-end in non-blocking mode so the open doesn't hang
    // (no writer yet). We pass this as FC's stdin.
    use std::os::unix::io::FromRawFd;

    let read_fd = nix::fcntl::open(
        &serial_in,
        nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_NONBLOCK,
        nix::sys::stat::Mode::empty(),
    )
    .context("failed to open serial.in FIFO for reading")?;

    // Clear O_NONBLOCK so FC reads block normally
    nix::fcntl::fcntl(
        read_fd,
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::empty()),
    )?;

    // Open a sentinel writer BEFORE spawning FC. FC inherits this fd,
    // so the FIFO always has >=1 writer even after the parent exits.
    // This prevents FC from seeing EOF when a real writer closes.
    let _sentinel_fd = nix::fcntl::open(
        &serial_in,
        nix::fcntl::OFlag::O_WRONLY | nix::fcntl::OFlag::O_NONBLOCK,
        nix::sys::stat::Mode::empty(),
    )
    .context("failed to open sentinel writer for FIFO")?;

    let stdin_file = unsafe { std::fs::File::from_raw_fd(read_fd) };

    let child = Command::new(FIRECRACKER_BIN)
        .arg("--api-sock")
        .arg(&socket_path)
        .arg("--log-path")
        .arg(&log_path)
        .arg("--level")
        .arg("Warning")
        .stdin(stdin_file)
        .stdout(serial_file)
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn firecracker")?;

    let pid = child.id();
    // Detach: let FC run independently. FC inherits the sentinel writer fd,
    // keeping the FIFO alive indefinitely.
    std::mem::forget(child);

    wait_for_socket(&socket_path, Duration::from_secs(5))?;

    Ok((pid, socket_path.to_string_lossy().to_string()))
}

/// Get the path to a VM's serial output log
pub fn serial_log_path(vm_dir: &Path) -> std::path::PathBuf {
    vm_dir.join("serial.log")
}

/// Write bytes to a running VM's serial console input via the named FIFO
pub fn write_to_serial(vm_dir: &Path, data: &[u8]) -> Result<()> {
    let fifo_path = vm_dir.join("serial.in");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(&fifo_path)
        .with_context(|| format!("cannot open {} — is VM running?", fifo_path.display()))?;
    f.write_all(data)?;
    f.flush()?;
    Ok(())
}

/// Kill a VM process (SIGTERM then SIGKILL)
pub fn kill_vm_process(pid: i64) {
    let pid = nix::unistd::Pid::from_raw(pid as i32);
    let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
    std::thread::sleep(Duration::from_millis(500));
    let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
}

/// Check if a process is alive
pub fn is_process_alive(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

// --- Firecracker API ---

pub fn fc_put(socket_path: &str, path: &str, body: &serde_json::Value) -> Result<()> {
    fc_request("PUT", socket_path, path, body)
}

pub fn fc_patch(socket_path: &str, path: &str, body: &serde_json::Value) -> Result<()> {
    fc_request("PATCH", socket_path, path, body)
}

pub fn pause_vm(socket_path: &str) -> Result<()> {
    fc_patch(
        socket_path,
        "/vm",
        &serde_json::json!({ "state": "Paused" }),
    )
    .context("failed to pause VM")
}

pub fn resume_vm(socket_path: &str) -> Result<()> {
    fc_patch(
        socket_path,
        "/vm",
        &serde_json::json!({ "state": "Resumed" }),
    )
    .context("failed to resume VM")
}

pub fn create_fc_snapshot(socket_path: &str, snap_dir: &Path) -> Result<()> {
    let mem_path = snap_dir.join("memory.snap");
    let state_path = snap_dir.join("vmstate.snap");
    fc_put(
        socket_path,
        "/snapshot/create",
        &serde_json::json!({
            "snapshot_type": "Full",
            "snapshot_path": state_path.to_string_lossy(),
            "mem_file_path": mem_path.to_string_lossy()
        }),
    )
    .context("failed to create FC snapshot")
}

pub fn load_fc_snapshot(socket_path: &str, snap_dir: &Path) -> Result<()> {
    let mem_path = snap_dir.join("memory.snap");
    let state_path = snap_dir.join("vmstate.snap");
    fc_put(
        socket_path,
        "/snapshot/load",
        &serde_json::json!({
            "snapshot_path": state_path.to_string_lossy(),
            "mem_backend": {
                "backend_path": mem_path.to_string_lossy(),
                "backend_type": "File"
            },
            "enable_diff_snapshots": false,
            "resume_vm": true
        }),
    )
    .context("failed to load FC snapshot")
}

pub fn configure_and_start_vm(
    socket_path: &str,
    kernel: &str,
    rootfs_path: &str,
    cpus: u32,
    mem_mib: u32,
) -> Result<()> {
    fc_put(
        socket_path,
        "/machine-config",
        &serde_json::json!({
            "vcpu_count": cpus,
            "mem_size_mib": mem_mib
        }),
    )
    .context("failed to set machine config")?;

    fc_put(
        socket_path,
        "/boot-source",
        &serde_json::json!({
            "kernel_image_path": kernel,
            "boot_args": "console=ttyS0 reboot=k panic=1 pci=off"
        }),
    )
    .context("failed to set boot source")?;

    fc_put(
        socket_path,
        "/drives/rootfs",
        &serde_json::json!({
            "drive_id": "rootfs",
            "path_on_host": rootfs_path,
            "is_root_device": true,
            "is_read_only": false
        }),
    )
    .context("failed to set root drive")?;

    fc_put(
        socket_path,
        "/actions",
        &serde_json::json!({
            "action_type": "InstanceStart"
        }),
    )
    .context("failed to start VM instance")?;

    Ok(())
}

// --- Helpers ---

fn wait_for_socket(path: &Path, timeout: Duration) -> Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("timed out waiting for socket at {}", path.display())
}

fn fc_request(
    method: &str,
    socket_path: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<()> {
    let body_str = serde_json::to_string(body)?;
    let request = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Accept: application/json\r\n\
         \r\n\
         {body_str}",
        body_str.len()
    );

    let mut stream = UnixStream::connect(socket_path)
        .with_context(|| format!("failed to connect to Firecracker socket: {socket_path}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.write_all(request.as_bytes())?;

    let mut response = String::new();
    let mut buf = [0u8; 4096];
    const MAX_RESPONSE_SIZE: usize = 1024 * 1024;
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if response.len() + n > MAX_RESPONSE_SIZE {
                    bail!("Firecracker API response too large (> 1MB)");
                }
                response.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
            Err(e) => return Err(e.into()),
        }
        if response.contains("\r\n\r\n") {
            if let Some(cl_start) = response.to_lowercase().find("content-length: ") {
                let cl_str = &response[cl_start + 16..];
                if let Some(end) = cl_str.find("\r\n") {
                    if let Ok(content_length) = cl_str[..end].parse::<usize>() {
                        if let Some(body_start) = response.find("\r\n\r\n") {
                            let body_received = response.len() - body_start - 4;
                            if body_received >= content_length {
                                break;
                            }
                        }
                    }
                }
            } else {
                break;
            }
        }
    }

    let status_line = response.lines().next().unwrap_or("");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    if (200..300).contains(&status_code) {
        Ok(())
    } else {
        let body = response
            .split("\r\n\r\n")
            .nth(1)
            .unwrap_or("unknown error");
        bail!("Firecracker API error (HTTP {status_code}): {body}")
    }
}
