use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Config;

const LOOPBACK_SIZE_MB: u64 = 4096;
const LOOPBACK_FILE: &str = "storage.img";

/// Root of storage (btrfs mount or plain directory)
pub fn storage_dir() -> PathBuf {
    Config::noid_dir().join("storage")
}

/// Path to the loopback image file
fn loopback_path() -> PathBuf {
    Config::noid_dir().join(LOOPBACK_FILE)
}

/// Detect if btrfs is available and we have permission to use it
fn btrfs_available() -> bool {
    Command::new("btrfs")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if storage dir is a btrfs mount
fn is_btrfs_mounted(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    // Check filesystem type
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

/// Ensure storage directory exists. Uses btrfs loopback if possible, plain dirs otherwise.
pub fn ensure_storage() -> Result<()> {
    let storage = storage_dir();

    if is_btrfs_mounted(&storage) {
        return Ok(());
    }

    // If btrfs loopback image exists, try to mount it
    let img = loopback_path();
    if img.exists()
        && btrfs_available()
        && run_cmd("mount", &["-o", "loop", &img.to_string_lossy(), &storage.to_string_lossy()])
            .is_ok()
    {
        return Ok(());
    }

    // Try to create btrfs loopback (needs root + btrfs tools)
    if btrfs_available() && !img.exists() {
        std::fs::create_dir_all(&storage)?;
        if run_cmd(
            "truncate",
            &["-s", &format!("{LOOPBACK_SIZE_MB}M"), &img.to_string_lossy()],
        )
        .is_ok()
        {
            if run_cmd("mkfs.btrfs", &["-f", &img.to_string_lossy()]).is_ok()
                && run_cmd(
                    "mount",
                    &["-o", "loop", &img.to_string_lossy(), &storage.to_string_lossy()],
                )
                .is_ok()
            {
                println!("btrfs storage initialized at {}", storage.display());
                return Ok(());
            }
            // Cleanup failed image
            let _ = std::fs::remove_file(&img);
        }
    }

    // Fallback: plain directory (no CoW, but works everywhere)
    std::fs::create_dir_all(&storage)?;
    Ok(())
}

/// Create a directory for a VM (btrfs subvolume if on btrfs, plain mkdir otherwise)
pub fn create_vm_subvolume(vm_name: &str) -> Result<PathBuf> {
    ensure_storage()?;
    let dir = storage_dir().join("vms").join(vm_name);
    if dir.exists() {
        bail!("storage already exists for VM '{vm_name}'");
    }
    std::fs::create_dir_all(dir.parent().unwrap())?;

    if is_btrfs_mounted(&storage_dir()) {
        run_cmd("btrfs", &["subvolume", "create", &dir.to_string_lossy()])?;
    } else {
        std::fs::create_dir_all(&dir)?;
    }
    Ok(dir)
}

/// Copy rootfs into VM's directory using reflink (falls back to regular copy on non-CoW fs)
pub fn reflink_rootfs(vm_name: &str, rootfs_src: &str) -> Result<PathBuf> {
    let dir = storage_dir().join("vms").join(vm_name);
    let dest = dir.join("rootfs.ext4");
    run_cmd(
        "cp",
        &["--reflink=auto", rootfs_src, &dest.to_string_lossy()],
    )?;
    Ok(dest)
}

/// Create a snapshot (checkpoint). btrfs snapshot if available, cp -a otherwise.
pub fn create_snapshot(vm_name: &str, checkpoint_id: &str) -> Result<PathBuf> {
    let src = storage_dir().join("vms").join(vm_name);
    let snap_dir = storage_dir().join("checkpoints").join(vm_name);
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
        // Fallback: recursive copy
        run_cmd("cp", &["-a", &src.to_string_lossy(), &snap.to_string_lossy()])?;
    }
    Ok(snap)
}

/// Create a writable clone from a checkpoint. btrfs snapshot or cp -a.
pub fn clone_snapshot(checkpoint_path: &str, new_vm_name: &str) -> Result<PathBuf> {
    let dest = storage_dir().join("vms").join(new_vm_name);
    if dest.exists() {
        bail!("storage already exists for VM '{new_vm_name}'");
    }
    std::fs::create_dir_all(dest.parent().unwrap())?;

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

/// Delete VM storage (btrfs subvolume delete or rm -rf)
pub fn delete_subvolume(vm_name: &str) -> Result<()> {
    let dir = storage_dir().join("vms").join(vm_name);
    if dir.exists() {
        if is_btrfs_mounted(&storage_dir()) {
            run_cmd(
                "btrfs",
                &["subvolume", "delete", &dir.to_string_lossy()],
            )?;
        } else {
            std::fs::remove_dir_all(&dir)?;
        }
    }
    Ok(())
}

/// Get the VM storage path
pub fn vm_subvolume_path(vm_name: &str) -> PathBuf {
    storage_dir().join("vms").join(vm_name)
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
