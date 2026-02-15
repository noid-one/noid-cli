use anyhow::{Context, Result};
// PermissionsExt is available on both Linux and macOS (both are Unix-like)
use std::os::unix::fs::PermissionsExt;

const BINARY_NAME: &str = "noid";
const REPO: &str = "noid-one/noid-cli";

/// Returns the platform-specific asset name for the current binary.
/// Release assets use the naming convention: `noid-{os}-{arch}`.
fn platform_asset_name() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "noid-linux-x86_64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "noid-darwin-x86_64"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "noid-darwin-aarch64"
    }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    )))]
    {
        compile_error!("noid client is only supported on Linux x86_64, macOS x86_64, and macOS aarch64")
    }
}

pub fn self_update() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");

    let resp: serde_json::Value = ureq::get(&url)
        .set("User-Agent", &format!("noid/{current}"))
        .set("Accept", "application/vnd.github+json")
        .call()
        .context("failed to fetch latest release")?
        .into_json()
        .context("failed to parse release JSON")?;

    let tag = resp["tag_name"]
        .as_str()
        .context("missing tag_name in release")?;
    let latest = tag.strip_prefix('v').unwrap_or(tag);

    if latest == current {
        println!("{BINARY_NAME} is already up to date (v{current})");
        return Ok(());
    }

    let asset_name = platform_asset_name();
    let asset = resp["assets"]
        .as_array()
        .context("missing assets array")?
        .iter()
        .find(|a| a["name"].as_str() == Some(asset_name))
        .context(format!("no '{asset_name}' asset in release"))?;

    let download_url = asset["browser_download_url"]
        .as_str()
        .context("missing download URL")?;

    let mut bytes: Vec<u8> = Vec::new();
    ureq::get(download_url)
        .set("User-Agent", &format!("noid/{current}"))
        .call()
        .context("failed to download binary")?
        .into_reader()
        .read_to_end(&mut bytes)
        .context("failed to read binary")?;

    let home = std::env::var("HOME").context("HOME not set")?;
    let bin_dir = std::path::PathBuf::from(&home).join(".local/bin");
    std::fs::create_dir_all(&bin_dir)?;

    let dest = bin_dir.join(BINARY_NAME);
    let tmp = bin_dir.join(format!("{BINARY_NAME}.tmp"));
    std::fs::write(&tmp, &bytes)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, &dest).context("failed to replace binary")?;

    println!("Updated {BINARY_NAME} v{current} -> v{latest}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_asset_name_has_prefix() {
        let name = platform_asset_name();
        assert!(
            name.starts_with("noid-"),
            "expected platform asset name to start with 'noid-', got '{name}'"
        );
    }

    #[test]
    fn platform_asset_name_contains_os_and_arch() {
        let name = platform_asset_name();
        let parts: Vec<&str> = name.split('-').collect();
        assert_eq!(parts.len(), 3, "expected 'noid-os-arch', got '{name}'");
        assert_eq!(parts[0], "noid");
        assert!(
            ["linux", "darwin"].contains(&parts[1]),
            "unexpected OS in asset name: {}",
            parts[1]
        );
        assert!(
            ["x86_64", "aarch64"].contains(&parts[2]),
            "unexpected arch in asset name: {}",
            parts[2]
        );
    }
}
