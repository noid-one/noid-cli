use anyhow::{Context, Result};
use noid_types::*;

use crate::config::ServerSection;

const API_VERSION: u32 = 1;

pub struct ApiClient {
    base_url: String,
    token: String,
}

impl ApiClient {
    pub fn new(server: &ServerSection) -> Self {
        Self {
            base_url: server.url.trim_end_matches('/').to_string(),
            token: server.token.clone(),
        }
    }

    fn get(&self, path: &str) -> Result<ureq::Response> {
        let url = format!("{}{path}", self.base_url);
        let resp = ureq::get(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .call()
            .map_err(|e| self.handle_error(e))?;
        self.check_api_version(&resp);
        Ok(resp)
    }

    fn post(&self, path: &str, body: &impl serde::Serialize) -> Result<ureq::Response> {
        let url = format!("{}{path}", self.base_url);
        let resp = ureq::post(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .send_json(serde_json::to_value(body)?)
            .map_err(|e| self.handle_error(e))?;
        self.check_api_version(&resp);
        Ok(resp)
    }

    fn delete(&self, path: &str) -> Result<ureq::Response> {
        let url = format!("{}{path}", self.base_url);
        let resp = ureq::delete(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .call()
            .map_err(|e| self.handle_error(e))?;
        self.check_api_version(&resp);
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

    fn check_api_version(&self, resp: &ureq::Response) {
        if let Some(version_str) = resp.header("X-Noid-Api-Version") {
            if let Ok(version) = version_str.parse::<u32>() {
                if version != API_VERSION {
                    eprintln!(
                        "warning: server API version ({version}) differs from client ({API_VERSION})"
                    );
                }
            }
        }
    }

    // --- Public API methods ---

    pub fn whoami(&self) -> Result<WhoamiResponse> {
        let resp = self.get("/v1/whoami")?;
        resp.into_json().context("failed to parse whoami response")
    }

    pub fn create_vm(&self, name: &str, cpus: u32, mem_mib: u32) -> Result<VmInfo> {
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
        let resp = self.get(&format!("/v1/vms/{name}"))?;
        resp.into_json().context("failed to parse VM info")
    }

    pub fn destroy_vm(&self, name: &str) -> Result<()> {
        self.delete(&format!("/v1/vms/{name}"))?;
        Ok(())
    }

    pub fn exec_vm(&self, name: &str, command: &[String]) -> Result<ExecResponse> {
        let req = ExecRequest {
            command: command.to_vec(),
            tty: false,
        };
        let resp = self.post(&format!("/v1/vms/{name}/exec"), &req)?;
        resp.into_json().context("failed to parse exec response")
    }

    pub fn create_checkpoint(
        &self,
        name: &str,
        label: Option<&str>,
    ) -> Result<CheckpointInfo> {
        let req = CheckpointRequest {
            label: label.map(|s| s.to_string()),
        };
        let resp = self.post(&format!("/v1/vms/{name}/checkpoints"), &req)?;
        resp.into_json()
            .context("failed to parse checkpoint response")
    }

    pub fn list_checkpoints(&self, name: &str) -> Result<Vec<CheckpointInfo>> {
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
    pub fn ws_connect(
        &self,
        path: &str,
        timeout: std::time::Duration,
    ) -> Result<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>>
    {
        use std::net::{TcpStream, ToSocketAddrs};
        use tungstenite::stream::MaybeTlsStream;

        let ws_url = self.ws_url(path);
        let uri: tungstenite::http::Uri = ws_url.parse().context("invalid WebSocket URL")?;
        let authority = uri.authority().context("missing authority in URL")?;
        let host = authority.host();
        let port = authority
            .port_u16()
            .unwrap_or(if uri.scheme_str() == Some("wss") { 443 } else { 80 });

        let addr_str = format!("{host}:{port}");
        let sock_addr = addr_str
            .to_socket_addrs()
            .context("failed to resolve server address")?
            .next()
            .context("no addresses found for server")?;

        let stream =
            TcpStream::connect_timeout(&sock_addr, timeout).context("connection timed out")?;
        stream.set_read_timeout(Some(timeout))?;

        let request = tungstenite::http::Request::builder()
            .uri(&ws_url)
            .header("Host", authority.as_str())
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .body(())
            .context("failed to build WS request")?;

        let (ws, _) = tungstenite::client::client(request, MaybeTlsStream::Plain(stream))
            .map_err(|e| anyhow::anyhow!("WebSocket handshake failed: {e}"))?;

        // Clear read timeout after successful handshake
        if let MaybeTlsStream::Plain(s) = ws.get_ref() {
            let _ = s.set_read_timeout(None);
        }

        Ok(ws)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerSection;

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

}
