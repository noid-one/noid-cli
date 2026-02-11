use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub kernel: Option<String>,
    pub rootfs: Option<String>,
}

impl Config {
    pub fn noid_dir() -> PathBuf {
        dirs_home().join(".noid")
    }

    pub fn path() -> PathBuf {
        Self::noid_dir().join("config.toml")
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
        let dir = Self::noid_dir();
        std::fs::create_dir_all(&dir)?;
        let content = toml::to_string_pretty(self)?;
        std::fs::write(Self::path(), content)?;
        Ok(())
    }

    pub fn set(key: &str, value: &str) -> Result<()> {
        let mut config = Self::load()?;
        match key {
            "kernel" => config.kernel = Some(value.to_string()),
            "rootfs" => config.rootfs = Some(value.to_string()),
            _ => anyhow::bail!("unknown config key: {key}. Valid keys: kernel, rootfs"),
        }
        config.save()?;
        println!("Set {key} = {value}");
        Ok(())
    }

    /// Resolve kernel path: CLI flag > config > error
    pub fn resolve_kernel(&self, flag: Option<&str>) -> Result<String> {
        flag.map(|s| s.to_string())
            .or(self.kernel.clone())
            .context("no kernel specified. Use --kernel or `noid config set kernel <path>`")
    }

    /// Resolve rootfs path: CLI flag > config > error
    pub fn resolve_rootfs(&self, flag: Option<&str>) -> Result<String> {
        flag.map(|s| s.to_string())
            .or(self.rootfs.clone())
            .context("no rootfs specified. Use --rootfs or `noid config set rootfs <path>`")
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/root"))
}
