# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

```bash
cargo build --workspace          # build all crates
cargo build --release --workspace  # release builds
cargo check --workspace          # type-check
cargo clippy --workspace         # lint
cargo test --workspace           # run all tests (46 total)
```

Three binaries:
- `noid` — CLI client (what end users install)
- `noid-server` — HTTP/WS server managing Firecracker VMs
- `noid-local` — legacy standalone CLI (pre-client-server)

## Workspace Layout

```
noid/
  Cargo.toml                     # workspace root + noid-local package
  src/                           # noid-local (standalone, unchanged)
  crates/
    noid-types/                  # wire types shared between client & server
    noid-core/                   # VM engine: DB, storage, exec, auth, backend
    noid-server/                 # HTTP/WS server on top of noid-core
    noid-client/                 # CLI client (HTTP + WS)
```

### Dependency Graph

```
noid-types          (leaf — serde, serde_json only)
    ↑
noid-core           (depends on noid-types)
    ↑
noid-server         (depends on noid-core + noid-types)

noid-client         (depends on noid-types only)
noid-local          (standalone)
```

## Architecture

### Protocol: REST + WebSocket

Synchronous stack — no async runtime:
- **Server**: `tiny_http` (sync HTTP), `tungstenite` (sync WS)
- **Client**: `ureq` (sync HTTP), `tungstenite` (sync WS)

REST endpoints under `/v1/`:
- `POST /v1/vms` — create VM
- `GET /v1/vms` — list VMs
- `GET /v1/vms/{name}` — get VM info
- `DELETE /v1/vms/{name}` — destroy VM
- `POST /v1/vms/{name}/exec` — HTTP exec (30s timeout, 1MB max)
- `POST /v1/vms/{name}/checkpoints` — create checkpoint
- `GET /v1/vms/{name}/checkpoints` — list checkpoints
- `POST /v1/vms/{name}/restore` — restore from checkpoint
- `GET /v1/vms/{name}/console` — WS upgrade for console
- `GET /v1/vms/{name}/exec` — WS upgrade for streaming exec
- `GET /v1/whoami`, `GET /v1/capabilities` — user/server info
- `GET /healthz`, `GET /version` — no auth required

WS frames use 1-byte channel prefix: `0x01`=stdout, `0x02`=stderr, `0x03`=stdin.

### Authentication

- Token format: `noid_tok_` + 64 hex chars (32 bytes entropy)
- Tokens hashed at rest with SHA-256, constant-time comparison via `subtle`
- Wire: `Authorization: Bearer noid_tok_...`
- Rate limiting by token prefix, not IP
- User management: `noid-server add-user/rotate-token/list-users/remove-user`

### Multi-Tenancy

- `users` table: UUID `id`, `name`, `token_hash`
- `vms` table has `user_id` column, `UNIQUE(user_id, name)`
- Storage: `~/.noid/storage/users/{user_id}/vms/{name}/`
- All queries scoped by `user_id`

### Crate Responsibilities

**noid-types** — Request/response structs with serde derives, WS channel constants.

**noid-core**:
- `vm.rs` — Firecracker process lifecycle + HTTP API (from original vm.rs)
- `db.rs` — SQLite with users/vms/checkpoints tables, user_id scoping
- `storage.rs` — btrfs/ext4 operations, user-namespaced paths
- `exec.rs` — `exec_via_serial()` + `shell_escape()` (from original main.rs)
- `backend.rs` — `VmBackend` trait + `FirecrackerBackend` with per-VM lock map, transactional create
- `auth.rs` — token generation, hashing, verification, rate limiting
- `config.rs` — shared path helpers

**noid-server**:
- `main.rs` — accept loop (thread-per-request), user management subcommands
- `transport.rs` — `RequestContext`/`ResponseBuilder` abstraction over tiny_http
- `router.rs` — path matching, auth middleware, request logging
- `handlers.rs` — REST endpoint handlers (no tiny_http types)
- `console.rs` — WS ↔ serial bridge
- `ws_exec.rs` — WS exec streaming
- `config.rs` — server config (listen, kernel, rootfs, max_ws_sessions)

**noid-client**:
- `main.rs` — clap dispatch → format output
- `cli.rs` — clap definitions (auth, use, current, whoami, create, destroy, list, info, exec, console, checkpoint, checkpoints, restore)
- `api.rs` — ureq HTTP client + Bearer auth, API version check
- `console.rs` — WS + crossterm for interactive console
- `exec.rs` — WS exec with fallback to HTTP POST
- `config.rs` — client config (URL, token), .noid active VM file

### Server Config (server.toml)

```toml
listen = "127.0.0.1:7654"
kernel = "/path/to/vmlinux.bin"
rootfs = "/path/to/rootfs.ext4"
max_ws_sessions = 32
trust_forwarded_for = false
exec_timeout_secs = 30
console_timeout_secs = 3600
```

### Client Config (~/.noid/config.toml)

```toml
[server]
url = "http://127.0.0.1:7654"
token = "noid_tok_..."
```

Set via: `noid auth setup --url <url> --token <token>`

### Active VM (.noid file)

Commands that take `<name>` fall back to `.noid` in CWD:
```
noid use myvm        # writes "myvm" to ./.noid
noid exec -- ls      # targets myvm
```

### DB Schema

Three tables:
- `users` — id (UUID), name (UNIQUE), token_hash, created_at
- `vms` — user_id (FK), name, UNIQUE(user_id, name), pid, socket_path, kernel, rootfs, cpus, mem_mib, state, created_at
- `checkpoints` — id, vm_name, user_id, label, snapshot_path, created_at

### Data Layout (multi-tenant)

```
~/.noid/
  config.toml
  noid.db
  storage/users/{user_id}/
    vms/{name}/        — rootfs.ext4, serial.log, serial.in, firecracker.sock, firecracker.log
    checkpoints/{name}/{id}/  — rootfs.ext4, serial.log, memory.snap, vmstate.snap
```

## Key Design Decisions

- **VmBackend trait** in noid-core allows mocking for tests. `FirecrackerBackend` wraps Db in Mutex for thread safety.
- **Transport abstraction** — handlers never touch tiny_http types. Swapping HTTP lib only changes transport.rs.
- **Per-VM lock map** in backend prevents concurrent serial writes. Console holds lock for duration.
- **Transactional create** — if FC spawn or API config fails, rolls back DB + storage.
- **Destroy** acquires VM lock, kills process, cleans storage + DB.

## Known Pitfalls

- **DB schema migration** — old noid.db lacks `user_id` column. Delete `~/.noid/noid.db` when upgrading.
- **FIFO EOF** — sentinel writer fd must be inherited by FC child.
- **Serial line endings** — `\r\n` not `\n`. Exec markers use `\r\n` delimiters.
- **nix crate features** — `fs` feature needed for mkfifo.
- **rusqlite not Sync** — Db wrapped in Mutex in FirecrackerBackend.

## Test Images

- Kernel: `/home/firecracker/vmlinux.bin`
- Rootfs: `/home/firecracker/rootfs.ext4`
