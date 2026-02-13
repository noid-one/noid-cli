use anyhow::{bail, Result};
use noid_types::{CheckpointInfo, ExecResult, VmInfo};
use std::collections::HashMap;
use std::io::Seek;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Per-VM lock map: keyed by (user_id, vm_name), value is a shared mutex.
type VmLockMap = Mutex<HashMap<(String, String), Arc<Mutex<()>>>>;

use crate::{db, exec, network, storage, vm};

/// Handle for an attached console session.
pub struct ConsoleHandle {
    pub serial_log: PathBuf,
    pub vm_dir: PathBuf,
}

/// Trait abstracting VM operations.
pub trait VmBackend: Send + Sync {
    fn create(&self, user_id: &str, name: &str, cpus: u32, mem_mib: u32) -> Result<VmInfo>;
    fn destroy(&self, user_id: &str, name: &str) -> Result<()>;
    fn get(&self, user_id: &str, name: &str) -> Result<Option<VmInfo>>;
    fn list(&self, user_id: &str) -> Result<Vec<VmInfo>>;
    fn exec_full(
        &self,
        user_id: &str,
        name: &str,
        command: &[String],
    ) -> Result<(String, ExecResult)>;
    fn checkpoint(&self, user_id: &str, name: &str, label: Option<&str>) -> Result<CheckpointInfo>;
    fn list_checkpoints(&self, user_id: &str, name: &str) -> Result<Vec<CheckpointInfo>>;
    fn restore(
        &self,
        user_id: &str,
        name: &str,
        checkpoint_id: &str,
        new_name: Option<&str>,
    ) -> Result<VmInfo>;
    fn console_attach(&self, user_id: &str, name: &str) -> Result<ConsoleHandle>;
}

pub struct FirecrackerBackend {
    db: Mutex<db::Db>,
    kernel: String,
    rootfs: String,
    exec_timeout_secs: u64,
    vm_locks: VmLockMap,
    golden_dir: PathBuf,
}

impl FirecrackerBackend {
    pub fn new(db: db::Db, kernel: String, rootfs: String, exec_timeout_secs: u64) -> Self {
        let golden_dir = storage::golden_dir();
        Self {
            db: Mutex::new(db),
            kernel,
            rootfs,
            exec_timeout_secs,
            vm_locks: Mutex::new(HashMap::new()),
            golden_dir,
        }
    }

    fn db(&self) -> std::sync::MutexGuard<'_, db::Db> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn vm_lock(&self, user_id: &str, name: &str) -> Arc<Mutex<()>> {
        let mut locks = self.vm_locks.lock().unwrap_or_else(|e| e.into_inner());
        locks
            .entry((user_id.to_string(), name.to_string()))
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn remove_vm_lock(&self, user_id: &str, name: &str) {
        let mut locks = self.vm_locks.lock().unwrap_or_else(|e| e.into_inner());
        locks.remove(&(user_id.to_string(), name.to_string()));
    }

    /// Cold-boot create: configure + start a fresh VM from kernel/rootfs.
    fn create_cold_boot(
        &self,
        user_id: &str,
        name: &str,
        cpus: u32,
        mem_mib: u32,
    ) -> Result<VmInfo> {
        if !std::path::Path::new(&self.kernel).exists() {
            bail!("kernel not found: {}", self.kernel);
        }
        if !std::path::Path::new(&self.rootfs).exists() {
            bail!("rootfs not found: {}", self.rootfs);
        }

        let net_config = match (|| -> Result<_> {
            let used = self.db().list_used_net_indices()?;
            let index = network::allocate_index(&used)?;
            network::setup_vm_network(index)
        })() {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                eprintln!("warning: VM networking unavailable: {e:#}");
                None
            }
        };

        let subvol = storage::create_vm_subvolume(user_id, name)?;
        let vm_rootfs = match storage::reflink_rootfs(user_id, name, &self.rootfs) {
            Ok(r) => r,
            Err(e) => {
                if let Some(ref nc) = net_config {
                    let _ = network::teardown_vm_network(&nc.tap_name);
                }
                let _ = storage::delete_subvolume(user_id, name);
                return Err(e);
            }
        };

        let (pid, sock) = match vm::spawn_fc(&subvol) {
            Ok(r) => r,
            Err(e) => {
                if let Some(ref nc) = net_config {
                    let _ = network::teardown_vm_network(&nc.tap_name);
                }
                let _ = storage::delete_subvolume(user_id, name);
                return Err(e);
            }
        };

        if let Err(e) = vm::configure_and_start_vm(
            &sock,
            &self.kernel,
            &vm_rootfs.to_string_lossy(),
            cpus,
            mem_mib,
            net_config.as_ref(),
        ) {
            vm::kill_vm_process(pid as i64);
            if let Some(ref nc) = net_config {
                let _ = network::teardown_vm_network(&nc.tap_name);
            }
            let _ = storage::delete_subvolume(user_id, name);
            return Err(e);
        }

        self.insert_vm_record(user_id, name, pid, sock, cpus, mem_mib, net_config.as_ref())
    }

    /// Fast create: restore from golden snapshot, reconfigure network.
    fn create_from_golden(
        &self,
        user_id: &str,
        name: &str,
        cpus: u32,
        mem_mib: u32,
    ) -> Result<VmInfo> {
        // Clone golden snapshot files into VM dir
        let subvol = storage::clone_golden(user_id, name)?;

        // Allocate network
        let net_config = match (|| -> Result<_> {
            let used = self.db().list_used_net_indices()?;
            let index = network::allocate_index(&used)?;
            network::setup_vm_network(index)
        })() {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                eprintln!("warning: VM networking unavailable: {e:#}");
                None
            }
        };

        // Spawn FC process (creates new FIFO + serial.log)
        let (pid, sock) = match vm::spawn_fc(&subvol) {
            Ok(r) => r,
            Err(e) => {
                if let Some(ref nc) = net_config {
                    let _ = network::teardown_vm_network(&nc.tap_name);
                }
                let _ = storage::delete_subvolume(user_id, name);
                return Err(e);
            }
        };

        // Load snapshot, patch drive/network to point at new resources, resume
        let rootfs_path = subvol.join("rootfs.ext4");
        let snapshot_rootfs_hint = match storage::golden_snapshot_rootfs_path() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("warning: failed to read golden snapshot rootfs path: {e:#}");
                None
            }
        }
        .or_else(|| vm::extract_rootfs_path_from_vmstate(&subvol));
        let rootfs_alias = snapshot_rootfs_hint
            .as_deref()
            .and_then(|p| {
                match vm::ensure_snapshot_rootfs_path(p, &rootfs_path.to_string_lossy()) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("warning: failed to create snapshot rootfs alias: {e:#}");
                        None
                    }
                }
            });
        if let Err(e) = vm::load_and_restore_snapshot(
            &sock,
            &subvol,
            &rootfs_path.to_string_lossy(),
            net_config.as_ref(),
        ) {
            if let Some(alias) = rootfs_alias.as_ref() {
                let _ = std::fs::remove_file(alias);
            }
            vm::kill_vm_process(pid as i64);
            if let Some(ref nc) = net_config {
                let _ = network::teardown_vm_network(&nc.tap_name);
            }
            let _ = storage::delete_subvolume(user_id, name);
            return Err(e);
        }
        if let Some(alias) = rootfs_alias.as_ref() {
            let _ = std::fs::remove_file(alias);
        }

        // Reconfigure guest network (snapshot has old template IP).
        // Brief delay lets the guest kernel stabilize after resume.
        if let Some(ref nc) = net_config {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if let Err(e) = self.reconfigure_guest_network(&subvol, nc) {
                eprintln!("warning: failed to reconfigure guest network: {e:#}");
                // Send Ctrl+C only when the command appears hung.
                if e.to_string().contains("timed out") {
                    let _ = vm::write_to_serial(&subvol, b"\x03\n");
                }
            }
        }

        self.insert_vm_record(user_id, name, pid, sock, cpus, mem_mib, net_config.as_ref())
    }

    /// Insert VM record into DB and return VmInfo. Rolls back on failure.
    #[allow(clippy::too_many_arguments)]
    fn insert_vm_record(
        &self,
        user_id: &str,
        name: &str,
        pid: u32,
        socket_path: String,
        cpus: u32,
        mem_mib: u32,
        net_config: Option<&network::NetworkConfig>,
    ) -> Result<VmInfo> {
        let rootfs_path = storage::vm_dir(user_id, name)
            .join("rootfs.ext4")
            .to_string_lossy()
            .to_string();

        if let Err(e) = self.db().insert_vm(
            user_id,
            name,
            db::VmInsertData {
                pid,
                socket_path,
                kernel: self.kernel.clone(),
                rootfs: rootfs_path,
                cpus,
                mem_mib,
                net_index: net_config.map(|c| c.index),
                tap_name: net_config.map(|c| c.tap_name.clone()),
                guest_ip: net_config.map(|c| c.guest_ip.clone()),
            },
        ) {
            vm::kill_vm_process(pid as i64);
            if let Some(nc) = net_config {
                let _ = network::teardown_vm_network(&nc.tap_name);
            }
            let _ = storage::delete_subvolume(user_id, name);
            return Err(e);
        }

        Ok(VmInfo {
            name: name.to_string(),
            state: "running".to_string(),
            cpus,
            mem_mib,
            created_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        })
    }

    /// After restoring from snapshot, reconfigure the guest's network interface.
    /// The snapshot has the template's old IP; we flush and assign the new one.
    /// Uses a bounded timeout since this is best-effort (VM is usable without it).
    fn reconfigure_guest_network(
        &self,
        vm_dir: &std::path::Path,
        net_config: &network::NetworkConfig,
    ) -> Result<()> {
        // Serial console auto-logins as root, so no sudo needed.
        let cmd_str = format!(
            "ip addr flush dev eth0 && \
             ip addr add {}/30 dev eth0 && \
             ip link set eth0 up && \
             ip route replace default via {}",
            net_config.guest_ip, net_config.host_ip
        );
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            cmd_str,
        ];
        let timeout = self.exec_timeout_secs.max(15);
        let (_, exit_code, timed_out, _) =
            exec::exec_via_serial(vm_dir, &cmd, timeout)?;
        if timed_out {
            bail!("network reconfiguration timed out");
        }
        if exit_code != Some(0) {
            bail!(
                "network reconfiguration failed (exit code: {:?})",
                exit_code
            );
        }
        Ok(())
    }

    fn vm_to_info(rec: &db::VmRecord) -> VmInfo {
        let alive = rec.pid.is_some_and(|pid| vm::is_process_alive(pid as i32));
        let state = if alive {
            rec.state.clone()
        } else {
            "dead".to_string()
        };
        VmInfo {
            name: rec.name.clone(),
            state,
            cpus: rec.cpus,
            mem_mib: rec.mem_mib,
            created_at: rec.created_at.clone(),
        }
    }
}

impl VmBackend for FirecrackerBackend {
    fn create(&self, user_id: &str, name: &str, cpus: u32, mem_mib: u32) -> Result<VmInfo> {
        storage::validate_name(name, "VM")?;

        if self.db().get_vm(user_id, name)?.is_some() {
            bail!("VM '{name}' already exists");
        }

        // Check if we can use the golden snapshot (fast path)
        let use_golden = self.golden_dir.join("memory.snap").exists()
            && match storage::golden_config() {
                Ok((gc, gm)) => gc == cpus && gm == mem_mib,
                Err(_) => false,
            };

        if use_golden {
            self.create_from_golden(user_id, name, cpus, mem_mib)
        } else {
            self.create_cold_boot(user_id, name, cpus, mem_mib)
        }
    }

    fn destroy(&self, user_id: &str, name: &str) -> Result<()> {
        let lock = self.vm_lock(user_id, name);
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());

        let vm_rec = self
            .db()
            .get_vm(user_id, name)?
            .ok_or_else(|| anyhow::anyhow!("VM '{name}' not found"))?;

        if let Some(pid) = vm_rec.pid {
            vm::kill_vm_process(pid);
        }

        // Teardown TAP device if networking was configured
        if let Some(ref tap) = vm_rec.tap_name {
            if let Err(e) = network::teardown_vm_network(tap) {
                eprintln!("warning: failed to teardown TAP {tap}: {e:#}");
            }
        }

        storage::delete_subvolume(user_id, name)?;
        self.db().delete_vm(user_id, name)?;

        drop(guard);
        self.remove_vm_lock(user_id, name);

        Ok(())
    }

    fn get(&self, user_id: &str, name: &str) -> Result<Option<VmInfo>> {
        let rec = self.db().get_vm(user_id, name)?;
        Ok(rec.as_ref().map(Self::vm_to_info))
    }

    fn list(&self, user_id: &str) -> Result<Vec<VmInfo>> {
        let vms = self.db().list_vms(user_id)?;
        Ok(vms.iter().map(Self::vm_to_info).collect())
    }

    fn exec_full(
        &self,
        user_id: &str,
        name: &str,
        command: &[String],
    ) -> Result<(String, ExecResult)> {
        self.db()
            .get_vm(user_id, name)?
            .ok_or_else(|| anyhow::anyhow!("VM '{name}' not found"))?;

        let lock = self.vm_lock(user_id, name);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

        let dir = storage::vm_dir(user_id, name);
        let (stdout, exit_code, timed_out, truncated) =
            exec::exec_via_serial(&dir, command, self.exec_timeout_secs)?;

        Ok((
            stdout,
            ExecResult {
                exit_code,
                timed_out,
                truncated,
            },
        ))
    }

    fn checkpoint(&self, user_id: &str, name: &str, label: Option<&str>) -> Result<CheckpointInfo> {
        let lock = self.vm_lock(user_id, name);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

        let rec = self
            .db()
            .get_vm(user_id, name)?
            .ok_or_else(|| anyhow::anyhow!("VM '{name}' not found"))?;

        let checkpoint_id = uuid::Uuid::new_v4().to_string().replace('-', "")[..16].to_string();

        vm::pause_vm(&rec.socket_path)?;
        let subvol = storage::vm_dir(user_id, name);
        vm::create_fc_snapshot(&rec.socket_path, &subvol)?;
        let snap_path = storage::create_snapshot(user_id, name, &checkpoint_id)?;
        vm::resume_vm(&rec.socket_path)?;

        self.db().insert_checkpoint(
            &checkpoint_id,
            name,
            user_id,
            label,
            &snap_path.to_string_lossy(),
        )?;

        Ok(CheckpointInfo {
            id: checkpoint_id,
            vm_name: name.to_string(),
            label: label.map(|s| s.to_string()),
            created_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        })
    }

    fn list_checkpoints(&self, user_id: &str, name: &str) -> Result<Vec<CheckpointInfo>> {
        let checkpoints = self.db().list_checkpoints(user_id, name)?;
        Ok(checkpoints
            .into_iter()
            .map(|cp| CheckpointInfo {
                id: cp.id,
                vm_name: cp.vm_name,
                label: cp.label,
                created_at: cp.created_at,
            })
            .collect())
    }

    fn restore(
        &self,
        user_id: &str,
        name: &str,
        checkpoint_id: &str,
        new_name: Option<&str>,
    ) -> Result<VmInfo> {
        let checkpoint = self
            .db()
            .get_checkpoint(user_id, checkpoint_id)?
            .ok_or_else(|| anyhow::anyhow!("checkpoint '{checkpoint_id}' not found"))?;

        let orig_vm = self.db().get_vm(user_id, &checkpoint.vm_name)?;
        let target_name = new_name.unwrap_or(name);
        storage::validate_name(target_name, "VM")?;

        if new_name.is_some() {
            if self.db().get_vm(user_id, target_name)?.is_some() {
                bail!("VM '{target_name}' already exists");
            }
            storage::clone_snapshot(user_id, &checkpoint.snapshot_path, target_name)?;
        } else {
            if let Some(rec) = self.db().get_vm(user_id, name)? {
                if let Some(pid) = rec.pid {
                    vm::kill_vm_process(pid);
                }
                // Teardown old VM's TAP
                if let Some(ref tap) = rec.tap_name {
                    let _ = network::teardown_vm_network(tap);
                }
                storage::delete_subvolume(user_id, name)?;
                self.db().delete_vm(user_id, name)?;
            }
            storage::clone_snapshot(user_id, &checkpoint.snapshot_path, target_name)?;
        }

        // Allocate new TAP for restored VM
        let net_config = match (|| -> Result<_> {
            let used = self.db().list_used_net_indices()?;
            let index = network::allocate_index(&used)?;
            network::setup_vm_network(index)
        })() {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                eprintln!("warning: VM networking unavailable for restore: {e:#}");
                None
            }
        };

        let subvol = storage::vm_dir(user_id, target_name);
        let (pid, socket_path) = match vm::spawn_fc(&subvol) {
            Ok(r) => r,
            Err(e) => {
                if let Some(ref nc) = net_config {
                    let _ = network::teardown_vm_network(&nc.tap_name);
                }
                let _ = storage::delete_subvolume(user_id, target_name);
                return Err(e);
            }
        };

        // Load snapshot, patch drive/network to point at new resources, resume
        let rootfs_path_for_restore = subvol.join("rootfs.ext4");
        let snapshot_rootfs_hint = orig_vm
            .as_ref()
            .map(|rec| rec.rootfs.clone())
            .or_else(|| vm::extract_rootfs_path_from_vmstate(&subvol));
        let rootfs_alias = snapshot_rootfs_hint
            .as_deref()
            .and_then(|p| {
                match vm::ensure_snapshot_rootfs_path(p, &rootfs_path_for_restore.to_string_lossy())
                {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("warning: failed to create snapshot rootfs alias: {e:#}");
                        None
                    }
                }
            });
        if let Err(e) = vm::load_and_restore_snapshot(
            &socket_path,
            &subvol,
            &rootfs_path_for_restore.to_string_lossy(),
            net_config.as_ref(),
        ) {
            if let Some(alias) = rootfs_alias.as_ref() {
                let _ = std::fs::remove_file(alias);
            }
            vm::kill_vm_process(pid as i64);
            if let Some(ref nc) = net_config {
                let _ = network::teardown_vm_network(&nc.tap_name);
            }
            let _ = storage::delete_subvolume(user_id, target_name);
            return Err(e);
        }
        if let Some(alias) = rootfs_alias.as_ref() {
            let _ = std::fs::remove_file(alias);
        }

        // Reconfigure guest network (snapshot has old IP from original VM).
        // Brief delay lets the guest kernel stabilize after resume.
        if let Some(ref nc) = net_config {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if let Err(e) = self.reconfigure_guest_network(&subvol, nc) {
                eprintln!("warning: failed to reconfigure guest network: {e:#}");
                // Send Ctrl+C only when the command appears hung.
                if e.to_string().contains("timed out") {
                    let _ = vm::write_to_serial(&subvol, b"\x03\n");
                }
            }
        }

        let (kernel, rootfs_path, cpus, mem_mib) = if let Some(ref orig) = orig_vm {
            (
                orig.kernel.clone(),
                orig.rootfs.clone(),
                orig.cpus,
                orig.mem_mib,
            )
        } else {
            (
                self.kernel.clone(),
                subvol.join("rootfs.ext4").to_string_lossy().to_string(),
                1,
                128,
            )
        };

        if let Err(e) = self.db().insert_vm(
            user_id,
            target_name,
            db::VmInsertData {
                pid,
                socket_path,
                kernel,
                rootfs: rootfs_path,
                cpus,
                mem_mib,
                net_index: net_config.as_ref().map(|c| c.index),
                tap_name: net_config.as_ref().map(|c| c.tap_name.clone()),
                guest_ip: net_config.as_ref().map(|c| c.guest_ip.clone()),
            },
        ) {
            vm::kill_vm_process(pid as i64);
            if let Some(ref nc) = net_config {
                let _ = network::teardown_vm_network(&nc.tap_name);
            }
            return Err(e);
        }

        Ok(VmInfo {
            name: target_name.to_string(),
            state: "running".to_string(),
            cpus,
            mem_mib,
            created_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        })
    }

    fn console_attach(&self, user_id: &str, name: &str) -> Result<ConsoleHandle> {
        self.db()
            .get_vm(user_id, name)?
            .ok_or_else(|| anyhow::anyhow!("VM '{name}' not found"))?;

        let dir = storage::vm_dir(user_id, name);
        let serial_log = vm::serial_log_path(&dir);
        if !serial_log.exists() {
            bail!("serial.log not found — is VM running?");
        }

        Ok(ConsoleHandle {
            serial_log,
            vm_dir: dir,
        })
    }
}

/// Write bytes to a console handle's serial input.
pub fn console_write(handle: &ConsoleHandle, data: &[u8]) -> Result<()> {
    vm::write_to_serial(&handle.vm_dir, data)
}

/// Open the serial log file for reading, positioned near the end so the
/// user sees recent output (like the login prompt) immediately on attach.
pub fn console_open_log(handle: &ConsoleHandle) -> Result<std::fs::File> {
    let mut f = std::fs::File::open(&handle.serial_log)?;
    // Seek back up to 512 bytes from the end to show recent context
    let len = f.seek(std::io::SeekFrom::End(0))?;
    let rewind = std::cmp::min(len, 512);
    f.seek(std::io::SeekFrom::End(-(rewind as i64)))?;
    Ok(f)
}
