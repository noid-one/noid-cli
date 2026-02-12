use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::config;

pub struct Db {
    conn: Connection,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct VmRecord {
    pub id: i64,
    pub user_id: String,
    pub name: String,
    pub pid: Option<i64>,
    pub socket_path: String,
    pub kernel: String,
    pub rootfs: String,
    pub cpus: u32,
    pub mem_mib: u32,
    pub state: String,
    pub created_at: String,
    pub net_index: Option<u32>,
    pub tap_name: Option<String>,
    pub guest_ip: Option<String>,
}

#[derive(Debug)]
pub struct CheckpointRecord {
    pub id: String,
    pub vm_name: String,
    pub user_id: String,
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
    pub net_index: Option<u32>,
    pub tap_name: Option<String>,
    pub guest_ip: Option<String>,
}

#[derive(Debug)]
pub struct UserRecord {
    pub id: String,
    pub name: String,
    pub token_hash: String,
    pub created_at: String,
}

impl Db {
    pub fn open() -> Result<Self> {
        let dir = config::noid_dir();
        std::fs::create_dir_all(&dir)?;
        let path = config::db_path();
        let conn = Connection::open(&path)
            .with_context(|| format!("failed to open database at {}", path.display()))?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                token_hash TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS vms (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id TEXT NOT NULL REFERENCES users(id),
                name TEXT NOT NULL,
                pid INTEGER,
                socket_path TEXT NOT NULL,
                kernel TEXT NOT NULL,
                rootfs TEXT NOT NULL,
                cpus INTEGER NOT NULL DEFAULT 1,
                mem_mib INTEGER NOT NULL DEFAULT 128,
                state TEXT NOT NULL DEFAULT 'running',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                net_index INTEGER,
                tap_name TEXT,
                guest_ip TEXT,
                UNIQUE(user_id, name)
            );
            CREATE TABLE IF NOT EXISTS checkpoints (
                id TEXT PRIMARY KEY,
                vm_name TEXT NOT NULL,
                user_id TEXT NOT NULL,
                label TEXT,
                snapshot_path TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (user_id, vm_name) REFERENCES vms(user_id, name)
            );",
        )?;
        Ok(())
    }

    // --- User methods ---

    pub fn insert_user(&self, id: &str, name: &str, token_hash: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO users (id, name, token_hash) VALUES (?1, ?2, ?3)",
            params![id, name, token_hash],
        )?;
        Ok(())
    }

    pub fn get_user_by_name(&self, name: &str) -> Result<Option<UserRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, token_hash, created_at FROM users WHERE name = ?1",
        )?;
        let mut rows = stmt.query_map(params![name], |row| {
            Ok(UserRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                token_hash: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn get_user_by_id(&self, id: &str) -> Result<Option<UserRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, token_hash, created_at FROM users WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(UserRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                token_hash: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Find user by hashing the token and looking up the hash directly.
    /// SHA-256 is deterministic, so we can do an O(1) lookup by hash.
    pub fn authenticate_user(&self, token: &str) -> Result<Option<UserRecord>> {
        let token_hash = crate::auth::hash_token(token);
        let mut stmt = self.conn.prepare(
            "SELECT id, name, token_hash, created_at FROM users WHERE token_hash = ?1",
        )?;
        let mut rows = stmt.query_map(params![token_hash], |row| {
            Ok(UserRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                token_hash: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn list_users(&self) -> Result<Vec<UserRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, token_hash, created_at FROM users ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(UserRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                token_hash: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn update_user_token(&self, name: &str, token_hash: &str) -> Result<bool> {
        let count = self.conn.execute(
            "UPDATE users SET token_hash = ?1 WHERE name = ?2",
            params![token_hash, name],
        )?;
        Ok(count > 0)
    }

    pub fn delete_user(&self, name: &str) -> Result<Option<String>> {
        // Return user_id so caller can clean up storage
        let user = self.get_user_by_name(name)?;
        let user_id = match user {
            Some(u) => u.id,
            None => return Ok(None),
        };
        // Delete checkpoints, then VMs, then user
        self.conn.execute(
            "DELETE FROM checkpoints WHERE user_id = ?1",
            params![user_id],
        )?;
        self.conn
            .execute("DELETE FROM vms WHERE user_id = ?1", params![user_id])?;
        self.conn
            .execute("DELETE FROM users WHERE id = ?1", params![user_id])?;
        Ok(Some(user_id))
    }

    // --- VM methods (user-scoped) ---

    pub fn insert_vm(&self, user_id: &str, name: &str, data: VmInsertData) -> Result<()> {
        self.conn.execute(
            "INSERT INTO vms (user_id, name, pid, socket_path, kernel, rootfs, cpus, mem_mib, state, net_index, tap_name, guest_ip)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'running', ?9, ?10, ?11)",
            params![
                user_id,
                name,
                data.pid,
                data.socket_path,
                data.kernel,
                data.rootfs,
                data.cpus,
                data.mem_mib,
                data.net_index,
                data.tap_name,
                data.guest_ip
            ],
        )?;
        Ok(())
    }

    pub fn get_vm(&self, user_id: &str, name: &str) -> Result<Option<VmRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, user_id, name, pid, socket_path, kernel, rootfs, cpus, mem_mib, state, created_at, net_index, tap_name, guest_ip
             FROM vms WHERE user_id = ?1 AND name = ?2",
        )?;
        let mut rows = stmt.query_map(params![user_id, name], |row| {
            Ok(VmRecord {
                id: row.get(0)?,
                user_id: row.get(1)?,
                name: row.get(2)?,
                pid: row.get(3)?,
                socket_path: row.get(4)?,
                kernel: row.get(5)?,
                rootfs: row.get(6)?,
                cpus: row.get(7)?,
                mem_mib: row.get(8)?,
                state: row.get(9)?,
                created_at: row.get(10)?,
                net_index: row.get(11)?,
                tap_name: row.get(12)?,
                guest_ip: row.get(13)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn list_vms(&self, user_id: &str) -> Result<Vec<VmRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, user_id, name, pid, socket_path, kernel, rootfs, cpus, mem_mib, state, created_at, net_index, tap_name, guest_ip
             FROM vms WHERE user_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![user_id], |row| {
            Ok(VmRecord {
                id: row.get(0)?,
                user_id: row.get(1)?,
                name: row.get(2)?,
                pid: row.get(3)?,
                socket_path: row.get(4)?,
                kernel: row.get(5)?,
                rootfs: row.get(6)?,
                cpus: row.get(7)?,
                mem_mib: row.get(8)?,
                state: row.get(9)?,
                created_at: row.get(10)?,
                net_index: row.get(11)?,
                tap_name: row.get(12)?,
                guest_ip: row.get(13)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_used_net_indices(&self) -> Result<Vec<u32>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT net_index FROM vms WHERE net_index IS NOT NULL")?;
        let rows = stmt.query_map([], |row| row.get::<_, u32>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn delete_vm(&self, user_id: &str, name: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM checkpoints WHERE user_id = ?1 AND vm_name = ?2",
            params![user_id, name],
        )?;
        self.conn.execute(
            "DELETE FROM vms WHERE user_id = ?1 AND name = ?2",
            params![user_id, name],
        )?;
        Ok(())
    }

    // --- Checkpoint methods (user-scoped) ---

    pub fn insert_checkpoint(
        &self,
        id: &str,
        vm_name: &str,
        user_id: &str,
        label: Option<&str>,
        snapshot_path: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO checkpoints (id, vm_name, user_id, label, snapshot_path)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, vm_name, user_id, label, snapshot_path],
        )?;
        Ok(())
    }

    pub fn get_checkpoint(
        &self,
        user_id: &str,
        checkpoint_id: &str,
    ) -> Result<Option<CheckpointRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, vm_name, user_id, label, snapshot_path, created_at
             FROM checkpoints WHERE id = ?1 AND user_id = ?2",
        )?;
        let mut rows = stmt.query_map(params![checkpoint_id, user_id], |row| {
            Ok(CheckpointRecord {
                id: row.get(0)?,
                vm_name: row.get(1)?,
                user_id: row.get(2)?,
                label: row.get(3)?,
                snapshot_path: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn list_checkpoints(
        &self,
        user_id: &str,
        vm_name: &str,
    ) -> Result<Vec<CheckpointRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, vm_name, user_id, label, snapshot_path, created_at
             FROM checkpoints WHERE user_id = ?1 AND vm_name = ?2 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![user_id, vm_name], |row| {
            Ok(CheckpointRecord {
                id: row.get(0)?,
                vm_name: row.get(1)?,
                user_id: row.get(2)?,
                label: row.get(3)?,
                snapshot_path: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
