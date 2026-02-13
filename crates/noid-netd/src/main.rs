#[allow(dead_code)]
mod addressing;
mod netlink;
mod tap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;

const SOCKET_DIR: &str = "/run/noid";
const SOCKET_PATH: &str = "/run/noid/netd.sock";

#[derive(Deserialize)]
struct Request {
    op: String,
    #[serde(default)]
    index: Option<u32>,
    #[serde(default)]
    tap_name: Option<String>,
}

#[derive(Serialize)]
struct SetupResponse {
    ok: bool,
    tap_name: String,
    host_ip: String,
    guest_ip: String,
    guest_mac: String,
}

#[derive(Serialize)]
struct OkResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active: Option<Vec<String>>,
}

fn handle_setup(index: u32) -> Result<String> {
    let config = addressing::derive_config(index);

    // Create TAP device
    tap::create_tap(&config.tap_name)
        .with_context(|| format!("failed to create TAP {}", config.tap_name))?;

    // Assign IP to host end
    if let Err(e) = netlink::assign_ip(&config.tap_name, &config.host_ip, 30) {
        // Rollback: destroy TAP
        let _ = tap::destroy_tap(&config.tap_name);
        return Err(e.context("failed to assign IP"));
    }

    // Bring link up
    if let Err(e) = tap::link_up(&config.tap_name) {
        let _ = tap::destroy_tap(&config.tap_name);
        return Err(e.context("failed to bring link up"));
    }

    let resp = SetupResponse {
        ok: true,
        tap_name: config.tap_name,
        host_ip: config.host_ip,
        guest_ip: config.guest_ip,
        guest_mac: config.guest_mac,
    };
    serde_json::to_string(&resp).map_err(Into::into)
}

fn handle_teardown(tap_name: &str) -> Result<String> {
    // Only allow destroying noid-managed interfaces
    if !tap_name.starts_with("noid") {
        anyhow::bail!("invalid tap_name '{}': must start with 'noid'", tap_name);
    }

    tap::destroy_tap(tap_name)?;

    let resp = OkResponse {
        ok: true,
        error: None,
        active: None,
    };
    serde_json::to_string(&resp).map_err(Into::into)
}

fn handle_status() -> Result<String> {
    // List active noid* interfaces by scanning /sys/class/net
    let mut active = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("noid") {
                active.push(name);
            }
        }
    }
    active.sort();

    let resp = OkResponse {
        ok: true,
        error: None,
        active: Some(active),
    };
    serde_json::to_string(&resp).map_err(Into::into)
}

fn handle_request(line: &str) -> String {
    let req: Request = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return serde_json::to_string(&OkResponse {
                ok: false,
                error: Some(format!("invalid request: {e}")),
                active: None,
            })
            .unwrap();
        }
    };

    let result = match req.op.as_str() {
        "setup" => match req.index {
            Some(idx) => handle_setup(idx),
            None => Err(anyhow::anyhow!("setup requires 'index' field")),
        },
        "teardown" => match req.tap_name.as_deref() {
            Some(name) => handle_teardown(name),
            None => Err(anyhow::anyhow!("teardown requires 'tap_name' field")),
        },
        "status" => handle_status(),
        other => Err(anyhow::anyhow!("unknown op: {other}")),
    };

    match result {
        Ok(json) => json,
        Err(e) => serde_json::to_string(&OkResponse {
            ok: false,
            error: Some(format!("{e:#}")),
            active: None,
        })
        .unwrap(),
    }
}

fn cleanup_orphaned_taps() {
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("noid") {
                eprintln!("cleaning up orphaned TAP: {name}");
                let _ = tap::destroy_tap(&name);
            }
        }
    }
}

fn main() -> Result<()> {
    eprintln!("noid-netd starting");

    // Create runtime directory
    std::fs::create_dir_all(SOCKET_DIR)
        .with_context(|| format!("failed to create {SOCKET_DIR}"))?;

    // Remove stale socket
    let _ = std::fs::remove_file(SOCKET_PATH);

    // Clean up orphaned TAPs from previous runs
    cleanup_orphaned_taps();

    // Bind Unix socket
    let listener =
        UnixListener::bind(SOCKET_PATH).with_context(|| format!("failed to bind {SOCKET_PATH}"))?;

    // Set socket permissions: owner=root, group+other can connect
    // The firecracker user needs to be able to connect
    unsafe {
        let path_c = std::ffi::CString::new(SOCKET_PATH).unwrap();
        if libc::chmod(path_c.as_ptr(), 0o666) != 0 {
            anyhow::bail!(
                "failed to set socket permissions on {}: {}",
                SOCKET_PATH,
                std::io::Error::last_os_error()
            );
        }
    }

    eprintln!("listening on {SOCKET_PATH}");

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => continue,
                    Ok(_) => {
                        let response = handle_request(line.trim());
                        let _ = writeln!(stream, "{response}");
                    }
                    Err(e) => {
                        eprintln!("read error: {e}");
                    }
                }
            }
            Err(e) => {
                eprintln!("accept error: {e}");
            }
        }
    }

    Ok(())
}
