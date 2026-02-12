use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ClientConfig {
    pub server: Option<ServerSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerSection {
    pub url: String,
    pub token: String,
}

impl ClientConfig {
    fn dir() -> PathBuf {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/root"))
            .join(".noid")
    }

    fn path() -> PathBuf {
        Self::dir().join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        toml::from_str(&content).with_context(|| "failed to parse config.toml")
    }

    pub fn save(&self) -> Result<()> {
        let dir = Self::dir();
        std::fs::create_dir_all(&dir)?;
        let content = toml::to_string_pretty(self)?;
        std::fs::write(Self::path(), content)?;
        Ok(())
    }

    pub fn server(&self) -> Result<&ServerSection> {
        self.server
            .as_ref()
            .context("not configured. Run: noid auth setup --url <url> --token <token>")
    }
}

/// Read the active VM name from .noid file in CWD.
pub fn read_active_vm() -> Option<String> {
    std::fs::read_to_string(".noid")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Write the active VM name to .noid file in CWD.
pub fn write_active_vm(name: &str) -> Result<()> {
    std::fs::write(".noid", format!("{name}\n"))?;
    Ok(())
}

/// Resolve VM name: explicit arg > .noid file > error.
pub fn resolve_vm_name(name: Option<&str>) -> Result<String> {
    if let Some(n) = name {
        return Ok(n.to_string());
    }
    read_active_vm().context("no VM specified. Pass a name or run: noid use <name>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_vm_name_explicit_takes_priority() {
        assert_eq!(resolve_vm_name(Some("myvm")).unwrap(), "myvm");
    }

    #[test]
    fn client_config_parses_toml() {
        let content = r#"
            [server]
            url = "http://localhost"
            token = "noid_tok_abc"
        "#;
        let config: ClientConfig = toml::from_str(content).unwrap();
        let server = config.server.unwrap();
        assert_eq!(server.url, "http://localhost");
        assert_eq!(server.token, "noid_tok_abc");
    }

    #[test]
    fn client_config_default_has_no_server() {
        let config = ClientConfig::default();
        assert!(config.server.is_none());
        assert!(config.server().is_err());
    }

    #[test]
    fn client_config_server_returns_ref() {
        let config = ClientConfig {
            server: Some(ServerSection {
                url: "http://localhost".into(),
                token: "tok".into(),
            }),
        };
        assert_eq!(config.server().unwrap().url, "http://localhost");
    }
}
