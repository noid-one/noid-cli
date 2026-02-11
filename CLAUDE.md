# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

```bash
cargo build --release          # release binary at target/release/noid
cargo build                    # debug build
cargo check                    # type-check without codegen (fastest feedback)
cargo clippy                   # lint
```

Run tests with `cargo test`. No Makefile or build script.

## Architecture

noid is a synchronous Rust CLI (~1400 lines across 7 files) that manages Firecracker microVMs. It spawns Firecracker processes, configures them via hand-rolled HTTP/1.1 over Unix sockets, and tracks state in SQLite.

### Module responsibilities

- **main.rs** (249 lines) — Command dispatch (match on `Command` enum) and the `exec_via_serial()` implementation. Each command handler is inline in main, not factored into separate functions per module.
- **vm.rs** (413 lines) — Firecracker process lifecycle and HTTP API client. `spawn_fc()` creates the FC process with named FIFO for stdin + file for stdout. `create_vm()` orchestrates the full creation flow (storage → spawn → configure via API → boot → DB insert). Also has `write_to_serial()` and `serial_log_path()` helpers used by console and exec. The HTTP client (`fc_request`/`fc_put`/`fc_patch`) is a raw HTTP/1.1-over-Unix-socket implementation — no HTTP library dependency.
- **db.rs** (214 lines) — SQLite via rusqlite (bundled). Schema auto-created on first `Db::open()`. Two tables: `vms` and `checkpoints`. All queries are inline SQL strings. `delete_vm()` cascades to checkpoints in application code (not via SQL CASCADE).
- **storage.rs** (204 lines) — Filesystem operations with btrfs/ext4 auto-detection. Shells out to `btrfs`, `cp`, `stat`, `truncate`, `mkfs.btrfs`, `mount`. Key pattern: every operation has a btrfs fast-path (subvolume/snapshot/reflink) and an ext4 fallback (mkdir/`cp -a`/`rm -rf`).
- **console.rs** (119 lines) — Bidirectional serial console bridge. Reader thread tails `serial.log`; main thread reads crossterm key events and writes to `serial.in` FIFO via `vm::write_to_serial()`. Ctrl+Q detaches. `key_to_bytes()` translates crossterm `KeyEvent` to terminal escape sequences.
- **cli.rs** (88 lines) — clap `#[derive(Parser)]` structs defining 9 subcommands. All CLI surface area lives here.
- **config.rs** (69 lines) — TOML config at `~/.noid/config.toml` with `kernel` and `rootfs` paths. Resolution order: CLI flag → config file → error.

### Key design decisions

- **No fctools** — We initially planned to use the fctools SDK but it has a complex, poorly-documented API. Direct HTTP over Unix socket is simpler and gives us full control.
- **No async runtime** — Despite the tokio dependency (reserved for future use), all I/O is blocking. Firecracker API calls use synchronous Unix socket reads/writes.
- **No HTTP library** — The Firecracker API client hand-builds HTTP/1.1 requests and parses responses. Response parsing reads until Content-Length is satisfied or connection closes.
- **Process model** — Firecracker processes are spawned and orphaned (`mem::forget()` on the `Child` handle). Each `noid` invocation is stateless; it reads the DB and probes process liveness with `kill(pid, 0)`. Destroy sends SIGTERM, waits 500ms, then SIGKILL.
- **Serial I/O via named FIFO** — FC's stdin is connected to a named FIFO (`serial.in`), FC's stdout goes to a regular file (`serial.log`). This lets any later `noid` process write to or read from the serial console without needing the original process handle.
- **Sentinel writer pattern** — A write-end fd for the FIFO is opened before `Command::spawn()` so FC inherits it. This keeps the FIFO alive (>=1 writer) even after the parent noid process exits, preventing FC from seeing EOF on stdin. Without this, only the first `exec`/`console` works — subsequent ones timeout because FC stopped reading.

### Firecracker API configuration order

`create_vm()` configures FC via PUT requests in this exact order (order matters):
1. `PUT /machine-config` — vcpu_count, mem_size_mib
2. `PUT /boot-source` — kernel_image_path, boot_args (`console=ttyS0 reboot=k panic=1 pci=off`)
3. `PUT /drives/rootfs` — drive_id, path_on_host, is_root_device, is_read_only
4. `PUT /actions` — InstanceStart

Pause/resume uses `PATCH /vm` with `{"state": "Paused"}` / `{"state": "Resumed"}`.
Snapshots use `PUT /snapshot/create` (Full type) and `PUT /snapshot/load` (with `resume_vm: true`).

### Serial exec protocol

`exec_via_serial()` in main.rs wraps commands with unique UUID markers:
```
echo 'NOID_EXEC_<8-char-uuid>'; <command>; echo 'NOID_EXEC_<8-char-uuid>_END'
```
It then polls `serial.log` for the markers on their own lines using `\r\n` delimiters (serial console uses CR+LF). The content between markers is extracted via `find()` on `"\r\n<marker>\r\n"` needles, trimmed, and printed. 30-second timeout, 100ms polling interval. The `\r\n`-delimited search is critical: matching bare markers would match inside the echoed command line, not the actual echo output.

### Data layout

All state lives under `~/.noid/`:
- `config.toml` — user defaults (kernel, rootfs)
- `noid.db` — SQLite (VM metadata, checkpoint records)
- `storage/vms/{name}/` — per-VM directory:
  - `rootfs.ext4` — copy of base rootfs (reflink on btrfs, regular copy on ext4)
  - `serial.log` — VM serial console output (FC stdout redirected here)
  - `serial.in` — named FIFO for serial console input (FC stdin reads from here)
  - `firecracker.sock` — FC API Unix socket
  - `firecracker.log` — FC's own log (not serial output)
- `storage/checkpoints/{vm-name}/{checkpoint-id}/` — snapshot of the VM directory:
  - `rootfs.ext4`, `serial.log`, `firecracker.log` (copied from VM dir)
  - `memory.snap` — Firecracker memory snapshot
  - `vmstate.snap` — Firecracker CPU/device state snapshot

### DB schema

Two tables:
- `vms` — name (UNIQUE), pid, socket_path, kernel, rootfs, cpus, mem_mib, state, created_at
- `checkpoints` — id (PK, TEXT 8-char UUID), vm_name (FK → vms.name), label, snapshot_path, created_at

Schema is created inline in `Db::init_schema()` via `CREATE TABLE IF NOT EXISTS`, not via migrations.

## Known pitfalls

- **FIFO EOF** — If the sentinel writer fd is not inherited by FC, the FIFO loses all writers when the first exec/console closes. FC sees EOF and stops reading stdin. Second exec will timeout. Fix: the sentinel fd must be opened BEFORE `Command::spawn()` so FC inherits it.
- **Serial line endings** — The serial console uses `\r\n`, not `\n`. Exec marker matching must use `\r\n` delimiters or markers won't be found.
- **Serial echo** — The terminal echoes back typed characters. Exec markers appear twice in serial.log: once in the echoed command line, once as echo output. The parser must find markers on their own lines (preceded by `\r\n`) to avoid matching the echoed command.
- **VM boot time** — The VM needs a few seconds to boot before exec/console work. The quickstart Ubuntu 18.04 rootfs auto-logs in as root on ttyS0.
- **nix crate features** — The `fs` feature is required for `mkfifo`, `fcntl::open`, `OFlag`, `FcntlArg`. Missing it causes confusing "could not find `stat` in `sys`" errors.
- **FK constraints** — Destroying a VM must delete its checkpoints first (done in `Db::delete_vm`), or the DELETE will fail with a foreign key violation. rusqlite doesn't enforce FK by default, but the explicit DELETE order in `delete_vm` handles this.
- **FIFO open blocking** — Opening a FIFO for reading blocks until a writer opens it (and vice versa). `spawn_fc` opens the read end with `O_NONBLOCK` to avoid hanging, then clears the flag so FC blocks normally on reads.
- **`unsafe` in vm.rs** — `File::from_raw_fd(read_fd)` is unsafe because ownership of the fd is transferred. This is correct here because `nix::fcntl::open` returns an owned fd.

## Test images

Quickstart images that work with noid:
- Kernel: `https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/x86_64/kernels/vmlinux.bin` (21 MiB)
- Rootfs: `https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/x86_64/rootfs/bionic.rootfs.ext4` (300 MiB, Ubuntu 18.04, auto-login root)

These are downloaded to `/home/firecracker/vmlinux.bin` and `/home/firecracker/rootfs.ext4` on the current host.
