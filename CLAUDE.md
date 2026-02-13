# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Installation

```bash
sudo bash scripts/install.sh    # complete setup: deps, Firecracker, networking, rootfs
```

## Build & Run

```bash
cargo build --workspace          # build all crates
cargo build --release --workspace  # release builds
cargo check --workspace          # type-check
cargo clippy --workspace         # lint
cargo test --workspace           # run all tests (71 total)
```

Four binaries:
- `noid` — CLI client (what end users install)
- `noid-server` — HTTP/WS server managing Firecracker VMs
- `noid-netd` — privileged network daemon for TAP device management (root)
- `noid-local` — legacy standalone CLI (pre-client-server)

## Workspace Layout

```
noid/
  Cargo.toml                     # workspace root + noid-local package
  src/                           # noid-local (standalone, unchanged)
  crates/
    noid-types/                  # wire types shared between client & server
    noid-core/                   # VM engine: DB, storage, exec, auth, backend, network client
    noid-server/                 # HTTP/WS server on top of noid-core
    noid-client/                 # CLI client (HTTP + WS)
    noid-netd/                   # privileged network daemon (TAP/IP/iptables)
  scripts/
    install.sh                   # complete installer (deps, FC, networking, rootfs)
    noid-netd.service            # systemd unit for noid-netd
```

### Dependency Graph

```
noid-types          (leaf — serde, serde_json only)
    ↑
noid-core           (depends on noid-types)
    ↑
noid-server         (depends on noid-core + noid-types)

noid-client         (depends on noid-types only)
noid-netd           (standalone — libc, serde, serde_json, anyhow)
noid-local          (standalone)
```

## Architecture

### Networking (Privilege Separation)

```
noid-server (unprivileged, firecracker user)
        |
        | Unix socket (/run/noid/netd.sock)
        v
noid-netd (root via systemd, CAP_NET_ADMIN + CAP_NET_RAW)
        |
        v
ioctl (TAP create/destroy) + ioctl (IP assign) + iptables (NAT)
```

- Every VM gets a TAP device, /30 subnet from 172.16.0.0/16, and internet via NAT
- Max 16384 concurrent VMs (16384 /30 subnets)
- Per-VM: host IP `.1`, guest IP `.2`, MAC `AA:FC:00:00:xx:xx`
- TAP name: `noid{index}` (e.g., `noid0`, `noid1`)
- Kernel boot param: `ip=<guest>::<host>:255.255.255.252::eth0:off`
- noid-netd protocol: JSON lines over Unix socket (connect-per-request)

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
- `vm.rs` — Firecracker process lifecycle + HTTP API
- `db.rs` — SQLite with users/vms/checkpoints tables, user_id scoping
- `storage.rs` — btrfs/ext4 operations, user-namespaced paths
- `exec.rs` — `exec_via_serial()` + `shell_escape()`
- `backend.rs` — `VmBackend` trait + `FirecrackerBackend` with per-VM lock map, transactional create with TAP rollback
- `auth.rs` — token generation, hashing, verification, rate limiting
- `network.rs` — client for noid-netd (setup/teardown TAP via Unix socket)
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

**noid-netd**:
- `main.rs` — daemon: Unix socket listener, request dispatch, orphaned TAP cleanup on startup
- `addressing.rs` — index → IP/MAC/subnet derivation, index allocation (bounded to 16384)
- `tap.rs` — TAP create/destroy via ioctl TUNSETIFF/TUNSETPERSIST, link_up via SIOCSIFFLAGS
- `netlink.rs` — IP/netmask assignment via ioctl SIOCSIFADDR/SIOCSIFNETMASK

### Server Config (server.toml)

```toml
listen = "0.0.0.0:7654"
kernel = "/home/firecracker/vmlinux.bin"
rootfs = "/home/firecracker/rootfs.ext4"
max_ws_sessions = 32
trust_forwarded_for = false
exec_timeout_secs = 30
console_timeout_secs = 3600
```

### Client Config (~/.noid/config.toml)

```toml
[server]
url = "http://your-server"
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
- `vms` — user_id (FK), name, UNIQUE(user_id, name), pid, socket_path, kernel, rootfs, cpus, mem_mib, state, created_at, net_index, tap_name, guest_ip
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
- **Transactional create** — if FC spawn, API config, or TAP setup fails, rolls back DB + storage + TAP.
- **Destroy** acquires VM lock, kills process, tears down TAP, cleans storage + DB.
- **Privilege separation** — noid-netd runs as root (CAP_NET_ADMIN), noid-server runs unprivileged. Communication via Unix socket.
- **Graceful degradation** — if noid-netd is not running, VMs are created without networking (warning printed).

## Known Pitfalls

- **DB schema migration** — old noid.db lacks network columns. Delete `~/.noid/noid.db` when upgrading (install.sh does this).
- **noid-netd must be running** — without it, VMs have no network. Check: `systemctl status noid-netd`
- **FIFO EOF** — sentinel writer fd must be inherited by FC child.
- **Serial line endings** — `\r\n` not `\n`. Exec markers use `\r\n` delimiters.
- **nix crate features** — `fs` feature needed for mkfifo.
- **rusqlite not Sync** — Db wrapped in Mutex in FirecrackerBackend.
- **Restore IP mismatch** — restored VMs get a new TAP/IP, but snapshot memory has old IP. Guest network may not work after restore.

## Test Images

- Kernel: `~/vmlinux.bin`
- Rootfs: `~/rootfs.ext4` (Ubuntu 25.04, built by install.sh)
