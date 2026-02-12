use anyhow::{Context, Result};
use std::os::unix::fs::PermissionsExt;

const BINARY_NAME: &str = "noid";
const REPO: &str = "noid-one/noid-cli";

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

    let asset = resp["assets"]
        .as_array()
        .context("missing assets array")?
        .iter()
        .find(|a| a["name"].as_str() == Some(BINARY_NAME))
        .context(format!("no '{BINARY_NAME}' asset in release"))?;

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
