use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config;

const LOOPBACK_SIZE_MB: u64 = 4096;
const LOOPBACK_FILE: &str = "storage.img";

/// Validate that a name is safe to use in paths (no path traversal).
pub fn validate_name(name: &str, kind: &str) -> Result<()> {
    if name.is_empty() {
        bail!("{kind} name cannot be empty");
    }
    if name.len() > 64 {
        bail!("{kind} name too long (max 64 characters)");
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        bail!("{kind} name contains invalid characters (/, \\, or ..)");
    }
    if name.starts_with('.') || name.starts_with('-') {
        bail!("{kind} name cannot start with . or -");
    }
    Ok(())
}

/// Root of storage (btrfs mount or plain directory)
pub fn storage_dir() -> PathBuf {
    config::noid_dir().join("storage")
}

/// Path for a user's storage
pub fn user_storage_dir(user_id: &str) -> PathBuf {
    storage_dir().join("users").join(user_id)
}

/// Path to the loopback image file
fn loopback_path() -> PathBuf {
    config::noid_dir().join(LOOPBACK_FILE)
}

fn btrfs_available() -> bool {
    Command::new("btrfs")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn is_btrfs_mounted(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    let output = Command::new("stat")
        .args(["-f", "-c", "%T"])
        .arg(path)
        .output();
    match output {
        Ok(o) => {
            let fstype = String::from_utf8_lossy(&o.stdout).trim().to_string();
            fstype == "btrfs"
        }
        Err(_) => false,
    }
}

pub fn ensure_storage() -> Result<()> {
    let storage = storage_dir();

    if is_btrfs_mounted(&storage) {
        return Ok(());
    }

    let img = loopback_path();
    if img.exists()
        && btrfs_available()
        && run_cmd(
            "mount",
            &[
                "-o",
                "loop",
                &img.to_string_lossy(),
                &storage.to_string_lossy(),
            ],
        )
        .is_ok()
    {
        return Ok(());
    }

    if btrfs_available() && !img.exists() {
        std::fs::create_dir_all(&storage)?;
        if run_cmd(
            "truncate",
            &[
                "-s",
                &format!("{LOOPBACK_SIZE_MB}M"),
                &img.to_string_lossy(),
            ],
        )
        .is_ok()
        {
            if run_cmd("mkfs.btrfs", &["-f", &img.to_string_lossy()]).is_ok()
                && run_cmd(
                    "mount",
                    &[
                        "-o",
                        "loop",
                        &img.to_string_lossy(),
                        &storage.to_string_lossy(),
                    ],
                )
                .is_ok()
            {
                return Ok(());
            }
            let _ = std::fs::remove_file(&img);
        }
    }

    std::fs::create_dir_all(&storage)?;
    Ok(())
}

/// Get the VM storage path (user-namespaced)
pub fn vm_dir(user_id: &str, vm_name: &str) -> PathBuf {
    user_storage_dir(user_id).join("vms").join(vm_name)
}

/// Create a directory for a VM (user-namespaced)
pub fn create_vm_subvolume(user_id: &str, vm_name: &str) -> Result<PathBuf> {
    validate_name(vm_name, "VM")?;
    ensure_storage()?;
    let dir = vm_dir(user_id, vm_name);
    if dir.exists() {
        bail!("storage already exists for VM '{vm_name}'");
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if is_btrfs_mounted(&storage_dir()) {
        run_cmd("btrfs", &["subvolume", "create", &dir.to_string_lossy()])?;
    } else {
        std::fs::create_dir_all(&dir)?;
    }
    Ok(dir)
}

/// Copy rootfs into VM's directory using reflink
pub fn reflink_rootfs(user_id: &str, vm_name: &str, rootfs_src: &str) -> Result<PathBuf> {
    validate_name(vm_name, "VM")?;
    let dir = vm_dir(user_id, vm_name);
    let dest = dir.join("rootfs.ext4");
    run_cmd(
        "cp",
        &["--reflink=auto", rootfs_src, &dest.to_string_lossy()],
    )?;
    Ok(dest)
}

/// Create a snapshot (checkpoint) — user-namespaced
pub fn create_snapshot(user_id: &str, vm_name: &str, checkpoint_id: &str) -> Result<PathBuf> {
    validate_name(vm_name, "VM")?;
    validate_name(checkpoint_id, "Checkpoint")?;
    let src = vm_dir(user_id, vm_name);
    let snap_dir = user_storage_dir(user_id).join("checkpoints").join(vm_name);
    std::fs::create_dir_all(&snap_dir)?;
    let snap = snap_dir.join(checkpoint_id);

    if is_btrfs_mounted(&storage_dir()) {
        run_cmd(
            "btrfs",
            &[
                "subvolume",
                "snapshot",
                "-r",
                &src.to_string_lossy(),
                &snap.to_string_lossy(),
            ],
        )?;
    } else {
        run_cmd(
            "cp",
            &["-a", &src.to_string_lossy(), &snap.to_string_lossy()],
        )?;
    }
    Ok(snap)
}

/// Clone a checkpoint to a new VM — user-namespaced
pub fn clone_snapshot(user_id: &str, checkpoint_path: &str, new_vm_name: &str) -> Result<PathBuf> {
    validate_name(new_vm_name, "VM")?;
    let dest = vm_dir(user_id, new_vm_name);
    if dest.exists() {
        bail!("storage already exists for VM '{new_vm_name}'");
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if is_btrfs_mounted(&storage_dir()) {
        run_cmd(
            "btrfs",
            &[
                "subvolume",
                "snapshot",
                checkpoint_path,
                &dest.to_string_lossy(),
            ],
        )?;
    } else {
        run_cmd("cp", &["-a", checkpoint_path, &dest.to_string_lossy()])?;
    }
    Ok(dest)
}

/// Path to the golden snapshot directory.
pub fn golden_dir() -> PathBuf {
    config::noid_dir().join("golden")
}

/// Check if a golden snapshot exists (has memory.snap).
pub fn golden_snapshot_exists() -> bool {
    golden_dir().join("memory.snap").exists()
}

/// Read the golden snapshot's template config (cpus, mem_mib).
pub fn golden_config() -> Result<(u32, u32)> {
    let config_path = golden_dir().join("config.json");
    let data = std::fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read golden config: {}", config_path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&data).context("failed to parse golden config.json")?;
    let cpus = v["cpus"]
        .as_u64()
        .context("missing cpus in golden config")? as u32;
    let mem_mib = v["mem_mib"]
        .as_u64()
        .context("missing mem_mib in golden config")? as u32;
    Ok((cpus, mem_mib))
}

/// Read optional source rootfs path embedded in golden config.
/// This is the backing file path originally captured in vmstate.snap.
pub fn golden_snapshot_rootfs_path() -> Result<Option<String>> {
    let config_path = golden_dir().join("config.json");
    let data = std::fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read golden config: {}", config_path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&data).context("failed to parse golden config.json")?;
    Ok(v["snapshot_rootfs_path"].as_str().map(|s| s.to_string()))
}

/// Clone golden snapshot files into a new VM directory.
/// Creates the VM dir, copies rootfs.ext4 (reflink), memory.snap, and vmstate.snap.
pub fn clone_golden(user_id: &str, vm_name: &str) -> Result<PathBuf> {
    validate_name(vm_name, "VM")?;
    ensure_storage()?;

    let golden = golden_dir();

    // Validate all required files exist and are non-empty
    for file in &["rootfs.ext4", "memory.snap", "vmstate.snap"] {
        let path = golden.join(file);
        if !path.exists() {
            bail!("golden snapshot incomplete: missing {}", path.display());
        }
        if path.metadata()?.len() == 0 {
            bail!("golden snapshot file is empty: {}", path.display());
        }
    }

    let dest = vm_dir(user_id, vm_name);
    if dest.exists() {
        bail!("storage already exists for VM '{vm_name}'");
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(&dest)?;
    // Copy rootfs with reflink for speed
    run_cmd(
        "cp",
        &[
            "--reflink=auto",
            &golden.join("rootfs.ext4").to_string_lossy(),
            &dest.join("rootfs.ext4").to_string_lossy(),
        ],
    )?;
    // Copy snapshot files
    std::fs::copy(golden.join("memory.snap"), dest.join("memory.snap"))
        .context("failed to copy memory.snap")?;
    std::fs::copy(golden.join("vmstate.snap"), dest.join("vmstate.snap"))
        .context("failed to copy vmstate.snap")?;

    Ok(dest)
}

/// Delete VM storage
pub fn delete_subvolume(user_id: &str, vm_name: &str) -> Result<()> {
    validate_name(vm_name, "VM")?;
    let dir = vm_dir(user_id, vm_name);
    if dir.exists() {
        if is_btrfs_mounted(&storage_dir()) {
            run_cmd("btrfs", &["subvolume", "delete", &dir.to_string_lossy()])?;
        } else {
            std::fs::remove_dir_all(&dir)?;
        }
    }
    Ok(())
}

/// Delete all storage for a user
pub fn delete_user_storage(user_id: &str) -> Result<()> {
    let dir = user_storage_dir(user_id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

fn run_cmd(program: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {program}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{program} failed: {stderr}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_name_accepts_simple_names() {
        assert!(validate_name("myvm", "VM").is_ok());
        assert!(validate_name("test_vm_01", "VM").is_ok());
        assert!(validate_name("a", "VM").is_ok());
        assert!(validate_name("VM123", "Checkpoint").is_ok());
    }

    #[test]
    fn validate_name_rejects_empty() {
        let err = validate_name("", "VM").unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn validate_name_rejects_too_long() {
        let long = "a".repeat(65);
        let err = validate_name(&long, "VM").unwrap_err();
        assert!(err.to_string().contains("too long"));
        let ok = "a".repeat(64);
        assert!(validate_name(&ok, "VM").is_ok());
    }

    #[test]
    fn validate_name_rejects_path_traversal() {
        let cases = ["../etc/passwd", "foo/bar", "a\\b", "foo..bar"];
        for name in cases {
            assert!(validate_name(name, "VM").is_err(), "should reject: {name}");
        }
    }

    #[test]
    fn validate_name_rejects_leading_dot_or_dash() {
        assert!(validate_name(".hidden", "VM").is_err());
        assert!(validate_name("-flag", "VM").is_err());
        assert!(validate_name("..double", "VM").is_err());
    }

    #[test]
    fn validate_name_preserves_kind_in_error() {
        let err = validate_name("", "Checkpoint").unwrap_err();
        assert!(err.to_string().contains("Checkpoint"));
    }
}
