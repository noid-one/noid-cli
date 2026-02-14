use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::config::Config;

pub struct Db {
    conn: Connection,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct VmRecord {
    pub id: i64,
    pub name: String,
    pub pid: Option<i64>,
    pub socket_path: String,
    pub kernel: String,
    pub rootfs: String,
    pub cpus: u32,
    pub mem_mib: u32,
    pub state: String,
    pub created_at: String,
}

#[derive(Debug)]
pub struct CheckpointRecord {
    pub id: String,
    pub vm_name: String,
    pub label: Option<String>,
    pub snapshot_path: String,
    pub created_at: String,
}

pub struct VmInsertData {
    pub pid: u32,
    pub socket_path: String,
    pub kernel: String,
    pub rootfs: String,
    pub cpus: u32,
    pub mem_mib: u32,
}

impl Db {
    pub fn open() -> Result<Self> {
        let dir = Config::noid_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("noid.db");
        let conn = Connection::open(&path)
            .with_context(|| format!("failed to open database at {}", path.display()))?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vms (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                pid INTEGER,
                socket_path TEXT NOT NULL,
                kernel TEXT NOT NULL,
                rootfs TEXT NOT NULL,
                cpus INTEGER NOT NULL DEFAULT 1,
                mem_mib INTEGER NOT NULL DEFAULT 2048,
                state TEXT NOT NULL DEFAULT 'running',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS checkpoints (
                id TEXT PRIMARY KEY,
                vm_name TEXT NOT NULL,
                label TEXT,
                snapshot_path TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (vm_name) REFERENCES vms(name)
            );",
        )?;
        Ok(())
    }

    pub fn insert_vm(&self, name: &str, vm_data: VmInsertData) -> Result<()> {
        self.conn.execute(
            "INSERT INTO vms (name, pid, socket_path, kernel, rootfs, cpus, mem_mib, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running')",
            params![
                name,
                vm_data.pid,
                vm_data.socket_path,
                vm_data.kernel,
                vm_data.rootfs,
                vm_data.cpus,
                vm_data.mem_mib
            ],
        )?;
        Ok(())
    }

    pub fn get_vm(&self, name: &str) -> Result<Option<VmRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, pid, socket_path, kernel, rootfs, cpus, mem_mib, state, created_at
             FROM vms WHERE name = ?1",
        )?;
        let mut rows = stmt.query_map(params![name], |row| {
            Ok(VmRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                pid: row.get(2)?,
                socket_path: row.get(3)?,
                kernel: row.get(4)?,
                rootfs: row.get(5)?,
                cpus: row.get(6)?,
                mem_mib: row.get(7)?,
                state: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn list_vms(&self) -> Result<Vec<VmRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, pid, socket_path, kernel, rootfs, cpus, mem_mib, state, created_at
             FROM vms ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(VmRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                pid: row.get(2)?,
                socket_path: row.get(3)?,
                kernel: row.get(4)?,
                rootfs: row.get(5)?,
                cpus: row.get(6)?,
                mem_mib: row.get(7)?,
                state: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn delete_vm(&self, name: &str) -> Result<()> {
        // Delete associated checkpoints first (FK constraint)
        self.conn
            .execute("DELETE FROM checkpoints WHERE vm_name = ?1", params![name])?;
        self.conn
            .execute("DELETE FROM vms WHERE name = ?1", params![name])?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn update_vm_state(&self, name: &str, state: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE vms SET state = ?1 WHERE name = ?2",
            params![state, name],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn update_vm_pid(&self, name: &str, pid: u32) -> Result<()> {
        self.conn.execute(
            "UPDATE vms SET pid = ?1 WHERE name = ?2",
            params![pid, name],
        )?;
        Ok(())
    }

    pub fn insert_checkpoint(
        &self,
        id: &str,
        vm_name: &str,
        label: Option<&str>,
        snapshot_path: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO checkpoints (id, vm_name, label, snapshot_path) VALUES (?1, ?2, ?3, ?4)",
            params![id, vm_name, label, snapshot_path],
        )?;
        Ok(())
    }

    pub fn get_checkpoint(&self, id: &str) -> Result<Option<CheckpointRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, vm_name, label, snapshot_path, created_at
             FROM checkpoints WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(CheckpointRecord {
                id: row.get(0)?,
                vm_name: row.get(1)?,
                label: row.get(2)?,
                snapshot_path: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn list_checkpoints(&self, vm_name: &str) -> Result<Vec<CheckpointRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, vm_name, label, snapshot_path, created_at
             FROM checkpoints WHERE vm_name = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![vm_name], |row| {
            Ok(CheckpointRecord {
                id: row.get(0)?,
                vm_name: row.get(1)?,
                label: row.get(2)?,
                snapshot_path: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
