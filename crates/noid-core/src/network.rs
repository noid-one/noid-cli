//! Network client — talks to noid-netd over Unix socket.
//!
//! No privileged operations here. All privilege lives in noid-netd.

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

const NETD_SOCKET: &str = "/run/noid/netd.sock";

#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub tap_name: String,
    pub host_ip: String,
    pub guest_ip: String,
    pub guest_mac: String,
    pub index: u32,
}

/// Ask noid-netd to set up a TAP device for the given index.
pub fn setup_vm_network(index: u32) -> Result<NetworkConfig> {
    let request = serde_json::json!({ "op": "setup", "index": index });
    let response = netd_request(&request).context("failed to setup VM network via noid-netd")?;

    if response.get("ok") != Some(&serde_json::Value::Bool(true)) {
        let err = response["error"]
            .as_str()
            .unwrap_or("unknown error from noid-netd");
        bail!("noid-netd setup failed: {err}");
    }

    Ok(NetworkConfig {
        tap_name: response["tap_name"]
            .as_str()
            .context("missing tap_name in response")?
            .to_string(),
        host_ip: response["host_ip"]
            .as_str()
            .context("missing host_ip in response")?
            .to_string(),
        guest_ip: response["guest_ip"]
            .as_str()
            .context("missing guest_ip in response")?
            .to_string(),
        guest_mac: response["guest_mac"]
            .as_str()
            .context("missing guest_mac in response")?
            .to_string(),
        index,
    })
}

/// Ask noid-netd to tear down a TAP device.
pub fn teardown_vm_network(tap_name: &str) -> Result<()> {
    let request = serde_json::json!({ "op": "teardown", "tap_name": tap_name });
    let response =
        netd_request(&request).context("failed to teardown VM network via noid-netd")?;

    if response.get("ok") != Some(&serde_json::Value::Bool(true)) {
        let err = response["error"]
            .as_str()
            .unwrap_or("unknown error from noid-netd");
        bail!("noid-netd teardown failed: {err}");
    }

    Ok(())
}

/// Find the lowest unused network index.
/// Max 16384 VMs (172.16.0.0/16 divided into /30 subnets).
const MAX_NET_INDEX: u32 = 16383;

pub fn allocate_index(used: &[u32]) -> Result<u32> {
    for i in 0..=MAX_NET_INDEX {
        if !used.contains(&i) {
            return Ok(i);
        }
    }
    bail!("no available network indices (all {} /30 subnets in 172.16.0.0/16 exhausted)", MAX_NET_INDEX + 1)
}

/// Build the kernel `ip=` boot parameter for the guest.
pub fn kernel_ip_param(config: &NetworkConfig) -> String {
    format!(
        "ip={}::{}:255.255.255.252::eth0:off",
        config.guest_ip, config.host_ip
    )
}

fn netd_request(request: &serde_json::Value) -> Result<serde_json::Value> {
    let mut stream = UnixStream::connect(NETD_SOCKET).with_context(|| {
        format!(
            "cannot connect to noid-netd at {NETD_SOCKET} — is noid-netd running? \
             Start it with: sudo systemctl start noid-netd"
        )
    })?;

    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .context("failed to write to noid-netd")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    reader
        .read_line(&mut response_line)
        .context("failed to read response from noid-netd")?;

    serde_json::from_str(&response_line).context("failed to parse noid-netd response")
}
