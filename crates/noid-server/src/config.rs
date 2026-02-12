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

    pub fn from_str(content: &str) -> anyhow::Result<Self> {
        toml::from_str(content).map_err(|e| anyhow::anyhow!("failed to parse config: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let cfg = ServerConfig::from_str(
            r#"
            kernel = "/path/to/vmlinux.bin"
            rootfs = "/path/to/rootfs.ext4"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.listen, "127.0.0.1:7654");
        assert_eq!(cfg.kernel, "/path/to/vmlinux.bin");
        assert_eq!(cfg.rootfs, "/path/to/rootfs.ext4");
        assert_eq!(cfg.max_ws_sessions, 32);
        assert!(!cfg.trust_forwarded_for);
        assert_eq!(cfg.exec_timeout_secs, 30);
        assert_eq!(cfg.console_timeout_secs, 3600);
    }

    #[test]
    fn parse_full_config() {
        let cfg = ServerConfig::from_str(
            r#"
            listen = "0.0.0.0:8080"
            kernel = "/k"
            rootfs = "/r"
            max_ws_sessions = 64
            trust_forwarded_for = true
            exec_timeout_secs = 60
            console_timeout_secs = 7200
            "#,
        )
        .unwrap();
        assert_eq!(cfg.listen, "0.0.0.0:8080");
        assert_eq!(cfg.max_ws_sessions, 64);
        assert!(cfg.trust_forwarded_for);
        assert_eq!(cfg.exec_timeout_secs, 60);
        assert_eq!(cfg.console_timeout_secs, 7200);
    }

    #[test]
    fn parse_missing_required_field() {
        // kernel and rootfs are required
        let result = ServerConfig::from_str(r#"listen = "127.0.0.1:7654""#);
        assert!(result.is_err());
    }
}
