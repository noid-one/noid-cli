# noid

A CLI for managing Firecracker microVMs with instant checkpointing and restore.

Create VMs in one command. Checkpoint them instantly. Clone and restore from any checkpoint. Inspired by [Fly.io Sprites](https://sprites.dev).

## Prerequisites

- Linux host with KVM support (`/dev/kvm` accessible)
- [Firecracker](https://github.com/firecracker-microvm/firecracker) installed at `/usr/local/bin/firecracker`
- A Linux kernel image (`vmlinux`) and root filesystem (`rootfs.ext4`)
- Rust toolchain (to build from source)

Optional: btrfs-progs for instant copy-on-write snapshots. Without btrfs, noid falls back to regular file copies — everything still works, just slower for large rootfs images.

## Install

```bash
git clone <repo-url> && cd noid
cargo build --release
cp target/release/noid /usr/local/bin/
```

## Quick start

### 1. Get a kernel and rootfs

Download the Firecracker quickstart images:

```bash
# Kernel
curl -fsSL -o vmlinux.bin \
  "https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/x86_64/kernels/vmlinux.bin"

# Root filesystem (Ubuntu 18.04)
curl -fsSL -o rootfs.ext4 \
  "https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/x86_64/rootfs/bionic.rootfs.ext4"
```

### 2. Configure defaults

Set these once. Every `noid create` after this uses them automatically:

```bash
noid config set kernel /path/to/vmlinux.bin
noid config set rootfs /path/to/rootfs.ext4
```

### 3. Create a VM

```bash
noid create my-vm
```

That's it. The VM boots in the background. You can override defaults per-VM:

```bash
noid create beefy-vm --cpus 4 --mem 512
```

### 4. Run commands inside the VM

```bash
noid exec my-vm -- uname -a
# Linux ubuntu-fc-uvm 4.14.174 #2 SMP ... x86_64 GNU/Linux

noid exec my-vm -- cat /etc/os-release
# NAME="Ubuntu"
# VERSION="18.04.5 LTS (Bionic Beaver)"
# ...
```

### 5. Attach to the serial console

```bash
noid console my-vm
```

This gives you a live, interactive terminal session inside the VM. Type commands, see output. Press **Ctrl+Q** to detach (the VM keeps running).

### 6. Checkpoint a VM

Capture the full state of a running VM — memory, CPU registers, disk — in one command:

```bash
noid checkpoint my-vm --label before-deploy
```

This pauses the VM, snapshots everything, and resumes it. Downtime is typically under a second.

### 7. List checkpoints

```bash
noid checkpoints my-vm
# +----------+---------------+---------------------+
# | id       | label         | created             |
# +----------+---------------+---------------------+
# | a1b2c3d4 | before-deploy | 2026-02-10 23:06:54 |
# +----------+---------------+---------------------+
```

### 8. Restore from a checkpoint

Clone a checkpoint into a brand new VM:

```bash
noid restore my-vm a1b2c3d4 --as my-vm-copy
```

The new VM starts from the exact state captured in the checkpoint — same memory contents, same running processes, same filesystem. On btrfs, the clone is instant (zero-copy).

You can also restore in-place, which destroys the current VM and replaces it:

```bash
noid restore my-vm a1b2c3d4
```

### 9. List running VMs

```bash
noid list
# +------------+---------+-------+------+-----------+---------------------+
# | name       | state   | pid   | cpus | mem (MiB) | created             |
# +------------+---------+-------+------+-----------+---------------------+
# | my-vm      | running | 12345 | 1    | 128       | 2026-02-10 23:06:12 |
# +------------+---------+-------+------+-----------+---------------------+
# | my-vm-copy | running | 12390 | 1    | 128       | 2026-02-10 23:07:04 |
# +------------+---------+-------+------+-----------+---------------------+
```

The state column shows `running` if the Firecracker process is alive, or `dead` if it has exited.

### 10. Destroy a VM

```bash
noid destroy my-vm
noid destroy my-vm-copy
```

Kills the Firecracker process, removes the VM's storage directory, and cleans up the database entry.

## Command reference

| Command | Description |
|---------|-------------|
| `noid create <name> [--kernel PATH] [--rootfs PATH] [--cpus N] [--mem MiB]` | Create and boot a new microVM |
| `noid destroy <name>` | Stop and remove a microVM |
| `noid list` | List all microVMs with status |
| `noid exec <name> -- <command...>` | Run a command inside a VM via serial console |
| `noid console <name>` | Attach interactive serial console (Ctrl+Q to detach) |
| `noid checkpoint <name> [--label TEXT]` | Snapshot a running VM |
| `noid checkpoints <name>` | List snapshots for a VM |
| `noid restore <name> <checkpoint-id> [--as NEW_NAME]` | Restore a VM from a snapshot |
| `noid config set <key> <value>` | Set a default (keys: `kernel`, `rootfs`) |

## How it works

### Storage layout

```
~/.noid/
  config.toml          # default kernel/rootfs paths
  noid.db              # SQLite — VM and checkpoint metadata
  storage/
    vms/
      my-vm/
        rootfs.ext4    # copy of base rootfs (reflink on btrfs)
        serial.log     # VM serial console output (FC stdout)
        serial.in      # named FIFO for serial input (FC stdin)
        firecracker.sock
        firecracker.log
    checkpoints/
      my-vm/
        a1b2c3d4/      # snapshot of the VM directory
          rootfs.ext4
          memory.snap   # Firecracker memory snapshot
          vmstate.snap  # Firecracker CPU/device state
          serial.log
```

### VM lifecycle

**Create** spawns a Firecracker process in the background, configures it via the HTTP API over a Unix socket (machine config, boot source, root drive), and starts the instance. The Firecracker process is orphaned — it keeps running after `noid` exits.

**Exec** sends commands through the serial console. It writes to a named FIFO (`serial.in`) connected to Firecracker's stdin, and reads output from `serial.log` (Firecracker's stdout). Unique markers delimit command output from other serial noise.

**Console** is a bidirectional pipe bridge: a reader thread tails `serial.log` to your terminal, while the main thread captures keystrokes and writes them to the FIFO.

### Checkpoint flow

```
noid checkpoint my-vm
  1. Pause the VM (PATCH /vm → Paused)
  2. Create Firecracker snapshot (PUT /snapshot/create → memory.snap + vmstate.snap)
  3. Copy the entire VM directory (btrfs read-only snapshot, or cp -a)
  4. Resume the VM (PATCH /vm → Resumed)
```

### Restore flow

```
noid restore my-vm a1b2c3d4 --as my-vm-copy
  1. Clone the checkpoint directory (btrfs writable snapshot, or cp -a)
  2. Spawn a new Firecracker process
  3. Load the snapshot (PUT /snapshot/load → memory.snap + vmstate.snap)
  4. VM resumes from the exact captured state
```

On btrfs, steps 1 and 3 in the checkpoint flow are instant zero-copy operations. On ext4, they fall back to regular copies.

### btrfs vs ext4

noid auto-detects the filesystem:

| | btrfs | ext4 / other |
|---|---|---|
| Create rootfs | reflink copy (instant, zero disk) | regular copy |
| Checkpoint | read-only snapshot (instant) | `cp -a` (copies everything) |
| Restore clone | writable snapshot (instant) | `cp -a` (copies everything) |
| Delete | `btrfs subvolume delete` | `rm -rf` |

To use btrfs, mount a btrfs filesystem at `~/.noid/storage/` before running `noid create`. If noid has root access, it can auto-create a btrfs loopback image.

## Architecture

```
src/
  main.rs      # CLI dispatch, exec implementation
  cli.rs       # clap derive structs (9 commands)
  config.rs    # ~/.noid/config.toml management
  db.rs        # SQLite schema, VM/checkpoint CRUD
  storage.rs   # btrfs/ext4 storage operations
  vm.rs        # Firecracker process + HTTP API client
  console.rs   # bidirectional serial console bridge
```

No async runtime needed at the binary level — Firecracker is managed via synchronous Unix socket HTTP calls and process spawning. The `tokio` dependency exists for future extensions.

## Troubleshooting

**"kernel not found" or "rootfs not found"**
Run `noid config set kernel /path/to/vmlinux.bin` and `noid config set rootfs /path/to/rootfs.ext4`.

**"failed to spawn firecracker"**
Ensure Firecracker is installed at `/usr/local/bin/firecracker` and is executable.

**"timed out waiting for socket"**
Firecracker failed to start. Check `~/.noid/storage/vms/<name>/firecracker.log` for errors.

**VM shows as "dead" in `noid list`**
The Firecracker process exited. Check the log file. Common causes: KVM not available, bad kernel/rootfs, insufficient memory.

**`noid exec` times out**
The VM may not have finished booting. Wait a few seconds after `noid create` and try again. Also ensure the rootfs has a shell at the default login.

**Checkpoint fails with "not running"**
The VM's Firecracker process must be alive and the VM must be in the `Running` state to create a checkpoint.
