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

This produces three server-side binaries:
- `target/release/noid-server` -- the HTTP/WS server
- `target/release/noid-netd` -- the privileged network daemon
- `target/release/noid` -- the CLI client

Install them:

```bash
mkdir -p $HOME/.local/bin
cp target/release/noid-server $HOME/.local/bin/
cp target/release/noid-netd $HOME/.local/bin/
cp target/release/noid $HOME/.local/bin/
```

## Step 2: Get a kernel and rootfs

The easiest way is to run the install script, which builds the kernel from source and creates an Ubuntu 25.04 rootfs automatically:

```bash
sudo bash scripts/install-server.sh
```

The installer builds kernel **6.12.71** from [kernel.org](https://www.kernel.org/) source using Firecracker's recommended config as a base. This takes 5-15 minutes on first run.

### Kernel version verification

The installer validates the existing kernel on every run. If `~/vmlinux.bin` exists but is the wrong version, it is replaced automatically:

```
[skip] kernel at /home/firecracker/vmlinux.bin is version 4.14, expected 6.12 — replacing
```

When the kernel is replaced, the **golden snapshot is also invalidated** (deleted and rebuilt), since a snapshot captured under the old kernel carries stale device state.

To check your kernel version manually:

```bash
strings ~/vmlinux.bin | grep -oP 'Linux version \K[0-9]+\.[0-9]+\.[0-9]+' | head -1
# Expected: 6.12.71
```

If you need to force a kernel rebuild (e.g., after a patch version bump in `install-server.sh`):

```bash
rm ~/vmlinux.bin
sudo bash scripts/install-server.sh
```

The rootfs is built by `install-server.sh` using debootstrap (Ubuntu 25.04 / plucky). See the script for details.

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
# listen = "0.0.0.0:7654"
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
| `listen` | No | `0.0.0.0:7654` | Address and port to bind |
| `max_ws_sessions` | No | `32` | Max concurrent WebSocket connections (console + exec) |
| `trust_forwarded_for` | No | `false` | Trust `X-Forwarded-For` header for client IP (set `true` behind a reverse proxy) |
| `exec_timeout_secs` | No | `30` | Max seconds a `noid exec` command can run |
| `console_timeout_secs` | No | `3600` | Max seconds an idle console session stays open |

## Step 4: Set up networking

VMs need NAT to reach the internet. This requires two things:

1. **IP forwarding** enabled on the host
2. **iptables rules** for MASQUERADE and FORWARD

### IP forwarding

```bash
sudo sysctl -w net.ipv4.ip_forward=1
echo "net.ipv4.ip_forward=1" | sudo tee /etc/sysctl.d/99-noid.conf
```

### iptables rules (handled by noid-netd)

You do **not** need to set up iptables rules manually or install `iptables-persistent`. The `noid-netd` daemon applies them automatically on startup:

- **MASQUERADE** for 172.16.0.0/16 via the default network interface
- **FORWARD** rules allowing VM traffic out and return traffic back

Rules are applied idempotently (checked before adding), so they coexist safely with `install-server.sh` or manual setup.

### noid-netd (network daemon)

`noid-netd` is a privileged daemon that manages TAP devices and iptables rules. It runs as root and communicates with the unprivileged `noid-server` via a Unix socket at `/run/noid/netd.sock`.

On startup, noid-netd:
1. Cleans up orphaned TAP devices from previous runs
2. Ensures iptables NAT/FORWARD rules are in place (auto-detects default interface)
3. Listens for TAP setup/teardown requests from noid-server

Install the systemd service:

```bash
# Install with path substitution for your home directory
sed "s|/home/firecracker/.local/bin|$HOME/.local/bin|g" scripts/noid-netd.service | \
  sudo tee /etc/systemd/system/noid-netd.service > /dev/null
sudo systemctl daemon-reload
sudo systemctl enable --now noid-netd
```

Verify it's running:

```bash
sudo systemctl status noid-netd
sudo journalctl -u noid-netd --no-pager -n 10
```

You should see: `iptables: NAT 172.16.0.0/16 via <interface>`

### Verify networking

```bash
# Check iptables rules
sudo iptables -t nat -L POSTROUTING -v -n   # MASQUERADE for 172.16.0.0/16
sudo iptables -L FORWARD -v -n              # ACCEPT for noid+ interfaces

# Check IP forwarding
sysctl net.ipv4.ip_forward                  # should be 1
```

### What survives reboot

| Component | Persistent? | How |
|---|---|---|
| IP forwarding | Yes | `/etc/sysctl.d/99-noid.conf` |
| iptables rules | Yes | noid-netd re-applies on startup |
| TAP devices | No | Created per-VM on demand |

## Step 5: Start the server

```bash
noid-server serve --config server.toml
```

The server will:
1. Load the config
2. Initialize the SQLite database at `~/.noid/noid.db`
3. Start listening on the configured address

You should see output like:

```
noid-server listening on 0.0.0.0:7654
```

To run in the background:

```bash
noid-server serve --config server.toml &
```

## Step 6: Add users

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

## Step 7: Manage users

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

### VM create fails with snapshot/rootfs `os error 2`

If `POST /v1/vms` fails with a Firecracker message like:

`Failed to restore devices ... backing file ... No such file or directory ... /vms/_golden/rootfs.ext4`

your golden snapshot references a template rootfs path that was deleted after snapshot creation.

Fix:

1. Rebuild `noid-server` with the current `v0.2.5`+ restore compatibility code.
2. Recreate the golden snapshot so `~/.noid/golden/config.json` includes `snapshot_rootfs_path`.

```bash
rm -rf ~/.noid/golden
sudo bash scripts/install-server.sh
```

### Schema migration errors

If upgrading from an older version of noid, the database schema may be incompatible. Delete the database and recreate users:

```bash
rm ~/.noid/noid.db
noid-server add-user alice   # re-add users
```

### VMs have no internet access

Check networking in this order:

1. **noid-netd running?** `sudo systemctl status noid-netd`
2. **iptables rules applied?** `sudo iptables -t nat -L POSTROUTING -v -n` should show MASQUERADE for 172.16.0.0/16
3. **IP forwarding enabled?** `sysctl net.ipv4.ip_forward` should be 1
4. **Default interface detected?** `sudo journalctl -u noid-netd --no-pager -n 10` should show `iptables: NAT 172.16.0.0/16 via <interface>`

If noid-netd failed to detect the default interface, the network may not have been ready when the service started. Restart it: `sudo systemctl restart noid-netd`

### HTTPS/TLS hangs inside VMs

TLS requires a seeded CRNG (cryptographic random number generator). Three things can cause TLS hangs:

**1. Stale kernel (most common)**

Old kernels (e.g. 4.14) take minutes to initialize the CRNG without `virtio-rng`. The installer now uses kernel 6.12 which initializes CRNG within seconds via the `virtio-rng` device.

Check your kernel version:

```bash
strings ~/vmlinux.bin | grep -oP 'Linux version \K[0-9]+\.[0-9]+' | head -1
# Should be: 6.12
```

If it shows an old version (4.14, 5.10, etc.), re-run the installer:

```bash
sudo bash scripts/install-server.sh
```

**2. Stale golden snapshot**

If the golden snapshot was captured under an old kernel, restored VMs inherit the old kernel's uninitialized CRNG state. The installer invalidates the golden snapshot when replacing the kernel, but if you replaced the kernel manually:

```bash
rm -rf ~/.noid/golden
sudo bash scripts/install-server.sh
```

**3. MTU/MSS mismatch**

If the upstream network has a smaller MTU than the VM's default 1500, large TLS packets get silently dropped. The installer and `noid-netd` both add a TCP MSS clamping rule to handle this. Verify:

```bash
sudo iptables -t mangle -L FORWARD -v -n
# Should show: TCPMSS ... --clamp-mss-to-pmtu
```

**Validation commands:**

```bash
# Quick TLS test from inside a VM (should complete in <5s)
noid exec --name myvm -- curl -sS -o /dev/null -w '%{http_code}' https://example.com

# Check entropy inside VM
noid exec --name myvm -- cat /proc/sys/kernel/random/entropy_avail

# Run the automated tests
bash scripts/test-e2e-tls.sh
bash scripts/test-golden-entropy.sh
```

### Database locked errors

The server uses `Mutex<Db>` internally. If you see locking errors, ensure only one `noid-server` instance is running against the same `~/.noid/` directory.

## Updating after code changes

When you rebuild noid from source, deploy the new binaries:

```bash
cd ~/noid
cargo build --release --workspace
cargo test --workspace

# Stop services (binaries are locked while running)
sudo systemctl stop noid-server
sudo systemctl stop noid-netd

# Copy new binaries
cp target/release/noid-server $HOME/.local/bin/
cp target/release/noid-netd $HOME/.local/bin/
cp target/release/noid $HOME/.local/bin/

# Update systemd units if changed
sed "s|/home/firecracker/.local/bin|$HOME/.local/bin|g" scripts/noid-netd.service | \
  sudo tee /etc/systemd/system/noid-netd.service > /dev/null
sudo systemctl daemon-reload

# Restart services
sudo systemctl start noid-netd
sudo systemctl start noid-server
```

Start noid-netd before noid-server -- the server needs noid-netd for VM networking.

### When to rebuild the golden snapshot

The golden snapshot must be rebuilt when any of these change:

| Changed | Action |
|---|---|
| Kernel image (`vmlinux.bin`) | `rm -rf ~/.noid/golden && sudo bash scripts/install-server.sh` |
| Base rootfs (`rootfs.ext4`) | `rm -rf ~/.noid/golden && sudo bash scripts/install-server.sh` |
| VM default config (cpus, mem) | `rm -rf ~/.noid/golden && sudo bash scripts/install-server.sh` |
| Firecracker version | `rm -rf ~/.noid/golden && sudo bash scripts/install-server.sh` |
| noid-server binary only | No rebuild needed (golden is independent of server code) |

The installer automatically invalidates the golden snapshot when it replaces the kernel. For other changes, delete it manually before re-running the installer.

Running `sudo bash scripts/install-server.sh` is always safe — it skips components that are already up to date and only rebuilds what changed.
