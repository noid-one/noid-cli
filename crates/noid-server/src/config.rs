use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    pub kernel: String,
    pub rootfs: String,
    #[serde(default = "default_max_ws_sessions")]
    pub max_ws_sessions: usize,
    #[serde(default)]
    pub trust_forwarded_for: bool,
    #[serde(default = "default_exec_timeout_secs")]
    pub exec_timeout_secs: u64,
    #[serde(default = "default_console_timeout_secs")]
    pub console_timeout_secs: u64,
}

fn default_listen() -> String {
    "127.0.0.1:7654".to_string()
}

fn default_max_ws_sessions() -> usize {
    32
}

fn default_exec_timeout_secs() -> u64 {
    30
}

fn default_console_timeout_secs() -> u64 {
    3600
}

impl ServerConfig {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read config file '{path}': {e}"))?;
        let config: Self = toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse config file '{path}': {e}"))?;
        Ok(config)
    }
}
