# Server Administration Guide

This guide walks through setting up, configuring, and managing a noid server.

## Prerequisites

- Linux host with KVM support (`/dev/kvm` accessible)
- [Firecracker](https://github.com/firecracker-microvm/firecracker) installed at `/usr/local/bin/firecracker`
- A Linux kernel image (`vmlinux.bin`) and root filesystem (`rootfs.ext4`)
- Rust toolchain (to build from source)

Optional: `btrfs-progs` for instant copy-on-write snapshots. Without btrfs, noid falls back to regular file copies.

## Step 1: Build the server

```bash
cd noid/
cargo build --release --workspace
```

This produces two binaries:
- `target/release/noid-server` -- the server
- `target/release/noid` -- the CLI client

Install them:

```bash
sudo cp target/release/noid-server $HOME/.local/bin/
sudo cp target/release/noid $HOME/.local/bin/
```

## Step 2: Get a kernel and rootfs

Download the Firecracker quickstart images:

```bash
# Kernel
curl -fsSL -o ~/vmlinux.bin \
  "https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/x86_64/kernels/vmlinux.bin"

# Root filesystem (Ubuntu 18.04)
curl -fsSL -o ~/rootfs.ext4 \
  "https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/x86_64/rootfs/bionic.rootfs.ext4"
```

## Step 3: Create a server config

Copy the example and edit it with the paths to your kernel and rootfs:

```bash
cp server.toml.example server.toml
```

```toml
# Required
kernel = "/path/to/vmlinux.bin"
rootfs = "/path/to/rootfs.ext4"

# Optional (these are the defaults)
# listen = "0.0.0.0:80"
# max_ws_sessions = 32
# trust_forwarded_for = false
# exec_timeout_secs = 30
# console_timeout_secs = 3600
```

### Config reference

| Field | Required | Default | Description |
|---|---|---|---|
| `kernel` | Yes | -- | Path to the `vmlinux.bin` kernel image |
| `rootfs` | Yes | -- | Path to the base `rootfs.ext4` filesystem |
| `listen` | No | `0.0.0.0:80` | Address and port to bind |
| `max_ws_sessions` | No | `32` | Max concurrent WebSocket connections (console + exec) |
| `trust_forwarded_for` | No | `false` | Trust `X-Forwarded-For` header for client IP (set `true` behind a reverse proxy) |
| `exec_timeout_secs` | No | `30` | Max seconds a `noid exec` command can run |
| `console_timeout_secs` | No | `3600` | Max seconds an idle console session stays open |

## Step 4: Start the server

```bash
noid-server serve --config server.toml
```

The server will:
1. Load the config
2. Initialize the SQLite database at `~/.noid/noid.db`
3. Start listening on the configured address

You should see output like:

```
noid-server listening on 0.0.0.0:80
```

To run in the background:

```bash
noid-server serve --config server.toml &
```

## Step 5: Add users

Each user gets a unique API token. Create a user with `add-user`:

```bash
noid-server add-user alice
```

This prints the token to stdout:

```
noid_tok_a1b2c3d4e5f6...  (64 hex characters)
```

**Save this token** -- it cannot be retrieved later. Give it to the user so they can configure their client.

Tokens are:
- Prefixed with `noid_tok_`
- 64 hex characters (32 bytes / 256 bits of entropy)
- SHA-256 hashed at rest in the database
- Verified with constant-time comparison (no timing attacks)

## Step 6: Manage users

### List all users

```bash
noid-server list-users
```

Shows user ID, name, and creation time.

### Rotate a user's token

If a token is compromised, rotate it:

```bash
noid-server rotate-token alice
```

Prints the new token. The old token is immediately invalidated.

### Remove a user

```bash
noid-server remove-user alice
```

This deletes the user **and all their VMs, checkpoints, and storage**. Use with caution.

## Storage layout

The server stores all data under `~/.noid/`:

```
~/.noid/
  noid.db                              # SQLite database
  storage/
    users/{user_id}/
      vms/{vm_name}/
        rootfs.ext4                    # VM's root filesystem
        serial.log                     # Serial console output
        serial.in                      # Named FIFO for serial input
        firecracker.sock               # Firecracker API socket
        firecracker.log                # Firecracker internal log
        memory.snap                    # (after checkpoint)
        vmstate.snap                   # (after checkpoint)
      checkpoints/{vm_name}/{id}/
        rootfs.ext4                    # Snapshot of rootfs
        serial.log                     # Snapshot of serial log
        memory.snap                    # Memory snapshot
        vmstate.snap                   # CPU/device state
```

Each user's data is fully isolated under their `user_id` directory.

## btrfs setup (optional, recommended)

With btrfs, VM creation, checkpointing, and restoring become instant zero-copy operations.

If your `~/.noid/storage/` directory is already on a btrfs filesystem, noid detects it automatically. Otherwise, noid can create a loopback btrfs image if it has root access.

To set up btrfs manually:

```bash
# Create a 4GB image
truncate -s 4G ~/.noid/storage.img

# Format as btrfs
mkfs.btrfs ~/.noid/storage.img

# Mount it
sudo mount -o loop ~/.noid/storage.img ~/.noid/storage/
```

### btrfs vs ext4 comparison

| Operation | btrfs | ext4 / other |
|---|---|---|
| Create rootfs | reflink copy (instant, zero disk) | full file copy |
| Checkpoint | read-only snapshot (instant) | `cp -a` (copies everything) |
| Restore/clone | writable snapshot (instant) | `cp -a` (copies everything) |
| Delete VM | `btrfs subvolume delete` | `rm -rf` |

## Security

### Authentication

All API endpoints (except `/healthz` and `/version`) require a Bearer token:

```
Authorization: Bearer noid_tok_...
```

### Rate limiting

Failed authentication attempts are rate-limited per token prefix (first 16 hex characters):
- 10 failures within 60 seconds triggers a block
- Blocked tokens receive `429 Too Many Requests` until the window expires

### Multi-tenancy

- Users cannot see or access each other's VMs
- All database queries are scoped by `user_id`
- VM names are unique per user (different users can have VMs with the same name)
- Storage paths include the `user_id` for filesystem-level isolation

## API endpoints

The server exposes a REST + WebSocket API under `/v1/`:

### Unauthenticated

| Method | Path | Description |
|---|---|---|
| `GET` | `/healthz` | Health check (`{"status": "ok"}`) |
| `GET` | `/version` | Server version and API version |

### Authenticated

| Method | Path | Description |
|---|---|---|
| `GET` | `/v1/whoami` | Current user info |
| `GET` | `/v1/capabilities` | Server defaults and limits |
| `POST` | `/v1/vms` | Create a VM |
| `GET` | `/v1/vms` | List all VMs |
| `GET` | `/v1/vms/{name}` | Get VM info |
| `DELETE` | `/v1/vms/{name}` | Destroy a VM |
| `POST` | `/v1/vms/{name}/exec` | Execute a command (HTTP) |
| `GET` | `/v1/vms/{name}/exec` | Execute a command (WebSocket upgrade) |
| `GET` | `/v1/vms/{name}/console` | Interactive console (WebSocket upgrade) |
| `POST` | `/v1/vms/{name}/checkpoints` | Create a checkpoint |
| `GET` | `/v1/vms/{name}/checkpoints` | List checkpoints |
| `POST` | `/v1/vms/{name}/restore` | Restore from checkpoint |

### Status codes

| Code | Meaning |
|---|---|
| `200` | Success |
| `201` | Created (VM or checkpoint) |
| `204` | Deleted (no content) |
| `400` | Bad request (invalid JSON, missing fields) |
| `401` | Unauthorized (missing or invalid token) |
| `404` | Not found (VM or checkpoint) |
| `409` | Conflict (VM name already exists) |
| `429` | Rate limited (too many auth failures) |
| `500` | Internal server error |
| `503` | Service unavailable (max WebSocket sessions reached) |

## Troubleshooting

### Server won't start

- Ensure `kernel` and `rootfs` paths in `server.toml` exist and are readable
- Check that the `listen` port is not already in use
- Check file permissions on `~/.noid/`

### VMs show as "dead"

The Firecracker process exited. Check `~/.noid/storage/users/{user_id}/vms/{name}/firecracker.log` for errors. Common causes:
- `/dev/kvm` not accessible (missing KVM support or permissions)
- Bad kernel or rootfs image
- Insufficient memory on the host

### Checkpoint fails

- The VM must be in a `running` state (Firecracker process alive)
- Ensure there is enough disk space for the snapshot

### Schema migration errors

If upgrading from an older version of noid, the database schema may be incompatible. Delete the database and recreate users:

```bash
rm ~/.noid/noid.db
noid-server add-user alice   # re-add users
```

### Database locked errors

The server uses `Mutex<Db>` internally. If you see locking errors, ensure only one `noid-server` instance is running against the same `~/.noid/` directory.
