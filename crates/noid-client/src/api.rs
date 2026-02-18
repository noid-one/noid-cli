use anyhow::{Context, Result};
use noid_types::*;

use crate::config::ServerSection;

const API_VERSION: u32 = 1;
const HTTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const WS_CONNECT_ATTEMPT_CAP: std::time::Duration = std::time::Duration::from_secs(2);

/// Sort socket addresses so IPv4 comes before IPv6.
/// Avoids timeouts on networks with broken IPv6 transit.
fn sort_ipv4_first(addrs: &mut [std::net::SocketAddr]) {
    addrs.sort_by_key(|a| match a {
        std::net::SocketAddr::V4(_) => 0u8,
        std::net::SocketAddr::V6(_) => 1,
    });
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

pub fn using_system_proxy() -> bool {
    env_truthy("NOID_USE_SYSTEM_PROXY")
}

pub fn proxy_env_vars_present() -> bool {
    [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some())
}

pub fn normalize_server_url(url: &str) -> Result<String> {
    let normalized = url.trim().trim_end_matches('/').to_string();
    if normalized.is_empty() {
        anyhow::bail!("server URL cannot be empty");
    }

    let uri: tungstenite::http::Uri = normalized
        .parse()
        .with_context(|| format!("invalid server URL: '{normalized}'"))?;

    let scheme = uri
        .scheme_str()
        .ok_or_else(|| anyhow::anyhow!("server URL must include scheme (http:// or https://)"))?;
    if scheme != "http" && scheme != "https" {
        anyhow::bail!("unsupported URL scheme '{scheme}' (expected http:// or https://)");
    }

    if uri.authority().is_none() {
        anyhow::bail!("server URL must include host");
    }

    Ok(normalized)
}

pub struct ApiClient {
    base_url: String,
    auth_header: String,
    agent: ureq::Agent,
}

impl ApiClient {
    pub fn new(server: &ServerSection) -> Self {
        let mut builder = ureq::AgentBuilder::new()
            .user_agent(&format!("noid/{}", env!("CARGO_PKG_VERSION")))
            .timeout_connect(HTTP_CONNECT_TIMEOUT)
            .timeout_read(std::time::Duration::from_secs(30))
            .resolver(|netloc: &str| -> std::io::Result<Vec<std::net::SocketAddr>> {
                use std::net::ToSocketAddrs;
                let mut addrs: Vec<_> = netloc.to_socket_addrs()?.collect();
                sort_ipv4_first(&mut addrs);
                Ok(addrs)
            });
        if !using_system_proxy() {
            builder = builder.try_proxy_from_env(false);
        }
        let agent = builder.build();
        Self {
            base_url: server.url.trim_end_matches('/').to_string(),
            auth_header: format!("Bearer {}", server.token),
            agent,
        }
    }

    fn validate_name(name: &str) -> Result<&str> {
        anyhow::ensure!(!name.is_empty(), "VM name cannot be empty");
        anyhow::ensure!(name.len() <= 64, "VM name too long (max 64 characters)");
        anyhow::ensure!(
            !name.starts_with('.') && !name.starts_with('-'),
            "VM name cannot start with . or -"
        );
        anyhow::ensure!(
            !name.contains(".."),
            "VM name cannot contain '..'"
        );
        anyhow::ensure!(
            name.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'),
            "VM name contains invalid characters"
        );
        Ok(name)
    }

    fn get(&self, path: &str) -> Result<ureq::Response> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .agent
            .get(&url)
            .set("Authorization", &self.auth_header)
            .call()
            .map_err(|e| self.handle_error(e))?;
        self.check_api_version(&resp)?;
        Ok(resp)
    }

    fn post(&self, path: &str, body: &impl serde::Serialize) -> Result<ureq::Response> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .agent
            .post(&url)
            .set("Authorization", &self.auth_header)
            .send_json(body)
            .map_err(|e| self.handle_error(e))?;
        self.check_api_version(&resp)?;
        Ok(resp)
    }

    fn delete(&self, path: &str) -> Result<ureq::Response> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .agent
            .delete(&url)
            .set("Authorization", &self.auth_header)
            .call()
            .map_err(|e| self.handle_error(e))?;
        self.check_api_version(&resp)?;
        Ok(resp)
    }

    fn handle_error(&self, err: ureq::Error) -> anyhow::Error {
        match err {
            ureq::Error::Status(status, resp) => {
                let body = resp.into_string().unwrap_or_default();
                if let Ok(err_resp) = serde_json::from_str::<ErrorResponse>(&body) {
                    anyhow::anyhow!("server error ({}): {}", status, err_resp.error)
                } else {
                    anyhow::anyhow!("server error ({}): {}", status, body)
                }
            }
            ureq::Error::Transport(t) => {
                anyhow::anyhow!("connection error: {t}")
            }
        }
    }

    fn check_api_version(&self, resp: &ureq::Response) -> Result<()> {
        if let Some(version_str) = resp.header("X-Noid-Api-Version") {
            let version = version_str.parse::<u32>().with_context(|| {
                format!("server sent unrecognized API version header: '{version_str}'")
            })?;
            anyhow::ensure!(
                version == API_VERSION,
                "server API version ({version}) is incompatible with client ({API_VERSION}); \
                 upgrade noid or noid-server"
            );
        }
        Ok(())
    }

    // --- Public API methods ---

    pub fn whoami(&self) -> Result<WhoamiResponse> {
        let resp = self.get("/v1/whoami")?;
        resp.into_json().context("failed to parse whoami response")
    }

    pub fn create_vm(&self, name: &str, cpus: u32, mem_mib: u32) -> Result<VmInfo> {
        let name = Self::validate_name(name)?;
        let req = CreateVmRequest {
            name: name.to_string(),
            cpus,
            mem_mib,
        };
        let resp = self.post("/v1/vms", &req)?;
        resp.into_json().context("failed to parse create response")
    }

    pub fn list_vms(&self) -> Result<Vec<VmInfo>> {
        let resp = self.get("/v1/vms")?;
        resp.into_json().context("failed to parse list response")
    }

    pub fn get_vm(&self, name: &str) -> Result<VmInfo> {
        let name = Self::validate_name(name)?;
        let resp = self.get(&format!("/v1/vms/{name}"))?;
        resp.into_json().context("failed to parse VM info")
    }

    pub fn destroy_vm(&self, name: &str) -> Result<()> {
        let name = Self::validate_name(name)?;
        self.delete(&format!("/v1/vms/{name}"))?;
        Ok(())
    }

    pub fn exec_vm(&self, name: &str, command: &[String], env: &[String]) -> Result<ExecResponse> {
        let name = Self::validate_name(name)?;
        let req = ExecRequest {
            command: command.to_vec(),
            tty: false,
            env: env.to_vec(),
        };
        let resp = self.post(&format!("/v1/vms/{name}/exec"), &req)?;
        resp.into_json().context("failed to parse exec response")
    }

    pub fn create_checkpoint(&self, name: &str, label: Option<&str>) -> Result<CheckpointInfo> {
        let name = Self::validate_name(name)?;
        let req = CheckpointRequest {
            label: label.map(|s| s.to_string()),
        };
        let resp = self.post(&format!("/v1/vms/{name}/checkpoints"), &req)?;
        resp.into_json()
            .context("failed to parse checkpoint response")
    }

    pub fn list_checkpoints(&self, name: &str) -> Result<Vec<CheckpointInfo>> {
        let name = Self::validate_name(name)?;
        let resp = self.get(&format!("/v1/vms/{name}/checkpoints"))?;
        resp.into_json()
            .context("failed to parse checkpoints response")
    }

    pub fn restore_vm(
        &self,
        name: &str,
        checkpoint_id: &str,
        new_name: Option<&str>,
    ) -> Result<VmInfo> {
        let name = Self::validate_name(name)?;
        if let Some(n) = new_name {
            Self::validate_name(n)?;
        }
        let req = RestoreRequest {
            checkpoint_id: checkpoint_id.to_string(),
            new_name: new_name.map(|s| s.to_string()),
        };
        let resp = self.post(&format!("/v1/vms/{name}/restore"), &req)?;
        resp.into_json().context("failed to parse restore response")
    }

    /// Return the WebSocket URL for a given path (replaces http(s) with ws(s)).
    pub fn ws_url(&self, path: &str) -> String {
        let base = self
            .base_url
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        format!("{base}{path}")
    }

    /// Connect a WebSocket with a connect/handshake timeout.
    ///
    /// WebSocket connections are established independently of `self.agent`
    /// because ureq does not support HTTP Upgrade. The agent is used only
    /// for REST calls (get/post/delete).
    ///
    /// Addresses are sorted IPv4-first to avoid timeouts when IPv6 transit
    /// is broken. The full TCP+TLS+WS pipeline is retried per address so
    /// that a TLS/handshake failure on one path falls back to the next.
    pub fn ws_connect(
        &self,
        path: &str,
        timeout: std::time::Duration,
    ) -> Result<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>>
    {
        use std::net::{TcpStream, ToSocketAddrs};

        let ws_url = self.ws_url(path);
        let uri: tungstenite::http::Uri = ws_url.parse().context("invalid WebSocket URL")?;
        let authority = uri.authority().context("missing authority in URL")?;
        let host = authority.host();
        let port = authority
            .port_u16()
            .unwrap_or(if uri.scheme_str() == Some("wss") {
                443
            } else {
                80
            });

        let addr_str = format!("{host}:{port}");
        let mut addrs: Vec<_> = addr_str
            .to_socket_addrs()
            .context("failed to resolve server address")?
            .collect();
        if addrs.is_empty() {
            anyhow::bail!("no addresses found for server");
        }

        sort_ipv4_first(&mut addrs);

        let verbose = env_truthy("NOID_VERBOSE");
        if verbose {
            eprintln!(
                "[ws] connecting to {addr_str} ({} address{})",
                addrs.len(),
                if addrs.len() == 1 { "" } else { "es" }
            );
            for a in &addrs {
                eprintln!("[ws]   {a}");
            }
        }

        let deadline = std::time::Instant::now() + timeout;
        let mut errors: Vec<String> = Vec::new();

        for (i, addr) in addrs.iter().enumerate() {
            let now = std::time::Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline.saturating_duration_since(now);
            let attempt_timeout = if i + 1 == addrs.len() {
                remaining
            } else {
                remaining.min(WS_CONNECT_ATTEMPT_CAP)
            };

            if verbose {
                eprintln!("[ws] trying {addr} (timeout {attempt_timeout:.1?})...");
            }

            // --- TCP connect ---
            let stream = match TcpStream::connect_timeout(addr, attempt_timeout) {
                Ok(s) => s,
                Err(e) => {
                    let msg = format!("{addr}: TCP connect failed: {e}");
                    if verbose {
                        eprintln!("[ws]   {msg}");
                    }
                    errors.push(msg);
                    continue;
                }
            };

            // Set a read timeout covering TLS + WS handshake.
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                errors.push(format!("{addr}: deadline expired after TCP connect"));
                continue;
            }
            let _ = stream.set_read_timeout(Some(remaining));
            // connect_timeout may leave the socket non-blocking — force blocking.
            if let Err(e) = stream.set_nonblocking(false) {
                errors.push(format!("{addr}: set_nonblocking failed: {e}"));
                continue;
            }

            // --- TLS + WebSocket handshake ---
            let request = tungstenite::http::Request::builder()
                .uri(&ws_url)
                .header("Host", authority.as_str())
                .header("Authorization", &self.auth_header)
                .header("Connection", "Upgrade")
                .header("Upgrade", "websocket")
                .header("Sec-WebSocket-Version", "13")
                .header(
                    "Sec-WebSocket-Key",
                    tungstenite::handshake::client::generate_key(),
                )
                .body(())
                .context("failed to build WS request")?;

            let ws = match tungstenite::client_tls(request, stream) {
                Ok((ws, _)) => ws,
                Err(e) => {
                    let detail = match &e {
                        tungstenite::HandshakeError::Interrupted(_) => {
                            "handshake interrupted (WouldBlock)".to_string()
                        }
                        tungstenite::HandshakeError::Failure(inner) => {
                            format!("handshake failed: {inner}")
                        }
                    };
                    let msg = format!("{addr}: {detail}");
                    if verbose {
                        eprintln!("[ws]   {msg}");
                    }
                    errors.push(msg);
                    continue;
                }
            };

            if verbose {
                eprintln!("[ws] connected via {addr}");
            }

            // Clear the handshake read timeout; callers manage their own blocking.
            match ws.get_ref() {
                tungstenite::stream::MaybeTlsStream::Plain(s) => {
                    let _ = s.set_read_timeout(None);
                }
                tungstenite::stream::MaybeTlsStream::Rustls(s) => {
                    let _ = s.get_ref().set_read_timeout(None);
                }
                _ => {}
            }

            return Ok(ws);
        }

        // All addresses exhausted (or deadline expired before any could be tried).
        if errors.is_empty() {
            anyhow::bail!("connection timed out to {addr_str}");
        } else if errors.len() == 1 {
            anyhow::bail!("connection failed ({})", errors[0]);
        } else {
            anyhow::bail!(
                "{}/{} addresses failed:\n  {}",
                errors.len(),
                addrs.len(),
                errors.join("\n  ")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerSection;

    #[test]
    fn normalize_server_url_trims_and_strips_trailing_slash() {
        let normalized = normalize_server_url("  https://noid.example.com/ ").unwrap();
        assert_eq!(normalized, "https://noid.example.com");
    }

    #[test]
    fn normalize_server_url_rejects_missing_scheme() {
        let err = normalize_server_url("noid.example.com").unwrap_err();
        assert!(err.to_string().contains("server URL must include scheme"));
    }

    #[test]
    fn normalize_server_url_rejects_unsupported_scheme() {
        let err = normalize_server_url("ftp://noid.example.com").unwrap_err();
        assert!(err.to_string().contains("unsupported URL scheme"));
    }

    #[test]
    fn ws_url_converts_http_to_ws() {
        let api = ApiClient::new(&ServerSection {
            url: "http://localhost".into(),
            token: "noid_tok_test".into(),
        });
        assert_eq!(
            api.ws_url("/v1/vms/test/console"),
            "ws://localhost/v1/vms/test/console"
        );
    }

    #[test]
    fn ws_url_converts_https_to_wss() {
        let api = ApiClient::new(&ServerSection {
            url: "https://noid.example.com".into(),
            token: "noid_tok_test".into(),
        });
        assert_eq!(
            api.ws_url("/v1/vms/test/exec"),
            "wss://noid.example.com/v1/vms/test/exec"
        );
    }

    #[test]
    fn base_url_strips_trailing_slash() {
        let api = ApiClient::new(&ServerSection {
            url: "http://localhost/".into(),
            token: "noid_tok_test".into(),
        });
        assert_eq!(
            api.ws_url("/v1/vms/test/console"),
            "ws://localhost/v1/vms/test/console"
        );
    }

    #[test]
    fn validate_name_accepts_valid_names() {
        assert!(ApiClient::validate_name("myvm").is_ok());
        assert!(ApiClient::validate_name("test_vm_01").is_ok());
        assert!(ApiClient::validate_name("a").is_ok());
        assert!(ApiClient::validate_name("my.vm").is_ok());
        assert!(ApiClient::validate_name("VM-123").is_ok());
    }

    #[test]
    fn validate_name_rejects_empty() {
        let err = ApiClient::validate_name("").unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn validate_name_rejects_too_long() {
        let long = "a".repeat(65);
        assert!(ApiClient::validate_name(&long).is_err());
        assert!(ApiClient::validate_name(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn validate_name_rejects_path_traversal() {
        assert!(ApiClient::validate_name("../etc").is_err());
        assert!(ApiClient::validate_name("foo/bar").is_err());
        assert!(ApiClient::validate_name("a\\b").is_err());
        assert!(ApiClient::validate_name("foo..bar").is_err());
    }

    #[test]
    fn validate_name_rejects_leading_dot_or_dash() {
        assert!(ApiClient::validate_name(".hidden").is_err());
        assert!(ApiClient::validate_name("-flag").is_err());
    }

    #[test]
    fn validate_name_rejects_special_chars() {
        assert!(ApiClient::validate_name("my vm").is_err());
        assert!(ApiClient::validate_name("vm?name").is_err());
        assert!(ApiClient::validate_name("vm#1").is_err());
    }

    #[test]
    fn sort_ipv4_first_orders_v4_before_v6() {
        use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

        let mut addrs = vec![
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 443, 0, 0)),
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 443)),
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 443, 0, 0)),
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 1), 443)),
        ];
        sort_ipv4_first(&mut addrs);
        assert!(matches!(addrs[0], SocketAddr::V4(_)));
        assert!(matches!(addrs[1], SocketAddr::V4(_)));
        assert!(matches!(addrs[2], SocketAddr::V6(_)));
        assert!(matches!(addrs[3], SocketAddr::V6(_)));
    }
}
