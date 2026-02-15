# Golden Snapshots Guide

Golden snapshots are pre-booted VM images that make `noid create` fast. Without one, every `noid create` cold-boots a VM from scratch (30-60 seconds). With a golden snapshot, the server restores from the pre-booted image instead (5-10 seconds).

The `scripts/provision-golden.sh` script lets you create and update golden snapshots with your own tools pre-installed.

## How it works

A golden snapshot is a frozen-in-time copy of a fully booted VM stored at `~/.noid/golden/`:

```
~/.noid/golden/
  rootfs.ext4      # Root filesystem (with any tools you installed)
  memory.snap      # Full RAM contents at snapshot time
  vmstate.snap     # CPU registers and device state
  config.json      # Template config (cpus, mem_mib)
```

When a user runs `noid create myvm`, the server checks:

1. Does `~/.noid/golden/memory.snap` exist?
2. Does the requested config (cpus, mem) match `config.json`?

If both are true, it clones the golden files and resumes the snapshot instead of booting from scratch. The new VM wakes up with all the tools that were installed when the snapshot was taken.

On btrfs, the rootfs clone is a reflink (instant, zero disk usage until writes diverge). On ext4 or other filesystems, it falls back to a full file copy. Memory and vmstate snapshots are always full copies regardless of filesystem.

Golden snapshots freeze the guest clock at the moment they were taken. When a VM is restored, the server automatically syncs the guest clock from the host (`sudo date -s @<epoch>`) and reconfigures the network interface with a fresh IP. This happens transparently during `noid create`.

## Initial setup

`install-server.sh` creates a minimal golden snapshot automatically during server provisioning. This snapshot has the base OS but no extra tools.

```bash
sudo bash scripts/install-server.sh
```

If golden creation fails during install (e.g. the template VM can't boot), the server still works -- VMs will just use the slower cold-boot path. You can re-run the installer or use `provision-golden.sh` later.

To customize the golden snapshot with your own tools, use `provision-golden.sh`.

## Mode 1: Full provisioning (recommended first time)

Run with no arguments to create a temporary VM, install tools, and promote the result to golden:

```bash
sudo bash scripts/provision-golden.sh
```

This runs through the following steps automatically:

1. Creates a temporary VM named `_provision`
2. Waits for it to boot
3. Installs Claude Code and opencode (hardcoded in the script)
4. Takes a checkpoint
5. Promotes the checkpoint to golden
6. Destroys the temporary VM

The server and noid-netd must be running, and the CLI must be configured with a valid token.

### Customizing what gets installed

Edit the provisioning section of `provision-golden.sh` (between the "Waiting for VM to boot" and "Taking checkpoint" steps) to install your own tools before the checkpoint is taken:

```bash
# --- Install Claude Code ---
step "Installing Claude Code"
sudo -u "$NOID_USER" noid exec _provision -- sh -c 'curl -fsSL https://claude.ai/install.sh | sh'
info "Claude Code installed"

# --- Install opencode ---
step "Installing opencode"
sudo -u "$NOID_USER" noid exec _provision -- sh -c 'curl -fsSL https://opencode.ai/install | sh'
info "opencode installed"

# Add your own tools here:
# sudo -u "$NOID_USER" noid exec _provision -- apt-get update
# sudo -u "$NOID_USER" noid exec _provision -- apt-get install -y git curl build-essential
# sudo -u "$NOID_USER" noid exec _provision -- pip install pytest
```

Every tool you install here will be available instantly in every new VM created from this golden snapshot.

## Mode 2: Promote an existing checkpoint

If you already have a VM configured exactly how you want it, take a checkpoint and promote it directly:

```bash
# 1. Set up your VM however you like
noid create my-base
noid exec my-base -- apt-get update
noid exec my-base -- apt-get install -y python3 nodejs git
noid exec -e ANTHROPIC_API_KEY=sk-... -- sh -c 'curl -fsSL https://claude.ai/install.sh | sh'

# 2. Take a checkpoint
noid checkpoint my-base --label golden-ready
```

```
Checkpoint 'a1b2c3d4e5f67890' created (label: golden-ready)
```

```bash
# 3. Promote that checkpoint to golden
sudo bash scripts/provision-golden.sh --from-checkpoint a1b2c3d4e5f67890
```

The script:

1. Finds the checkpoint files on disk
2. Reads the VM's cpus/mem_mib config from the database
3. Extracts the snapshot rootfs path from `vmstate.snap`
4. Backs up any existing golden snapshot (timestamped `.bak`)
5. Copies the checkpoint files to `~/.noid/golden/`
6. Writes `config.json` with the template metadata
7. Fixes file ownership

Your original VM and checkpoint are left untouched.

## Verifying the golden snapshot

After provisioning, confirm it's working:

```bash
# Check golden files exist
ls -lh ~/.noid/golden/
```

```
-rw-r--r-- 1 firecracker firecracker 1.1G rootfs.ext4
-rw-r--r-- 1 firecracker firecracker 2.1G memory.snap
-rw-r--r-- 1 firecracker firecracker  53K vmstate.snap
-rw-r--r-- 1 firecracker firecracker   62 config.json
```

```bash
# Check config
cat ~/.noid/golden/config.json
```

```json
{"cpus": 1, "mem_mib": 2048, "snapshot_rootfs_path": "/home/firecracker/.noid/storage/users/.../rootfs.ext4"}
```

The `snapshot_rootfs_path` is the rootfs path that was baked into `vmstate.snap` at snapshot time. The server uses it internally to create a temporary path alias during restore, since the original file no longer exists after the template VM is destroyed.

```bash
# Create a VM and verify tools are present
noid create test-golden
noid exec test-golden -- claude --version    # if Claude Code was installed
noid exec test-golden -- python3 --version   # if python3 was installed
noid destroy test-golden
```

If the golden snapshot has the right config, `noid create` will use the fast path. If you see cold-boot times (30+ seconds), the config doesn't match -- see troubleshooting below.

## When to rebuild

The golden snapshot must be rebuilt when any of these change:

| What changed | Why | Action |
|---|---|---|
| Kernel (`vmlinux.bin`) | Snapshot carries old kernel's device state | `install-server.sh` handles this automatically |
| Base rootfs (`rootfs.ext4`) | Snapshot has old filesystem | `rm -rf ~/.noid/golden` then re-provision |
| Firecracker version | Snapshot format may be incompatible | `rm -rf ~/.noid/golden` then re-provision |
| Default cpus or mem_mib | Config mismatch skips golden | Re-provision with matching config |
| You want new tools pre-installed | Tools added after snapshot aren't in golden | Re-provision |

To force a clean rebuild:

```bash
rm -rf ~/.noid/golden
sudo bash scripts/provision-golden.sh
```

The script backs up the existing golden directory before replacing it, so you can roll back if needed:

```bash
ls ~/.noid/golden.bak.*
# golden.bak.1739500000  ← timestamped backup
```

## Config matching

The golden snapshot is only used when the requested VM's `cpus` and `mem_mib` exactly match the golden config. No other fields are checked -- the snapshot is assumed to be compatible with the server's current kernel and Firecracker version. The default config is 1 vCPU and 2048 MiB RAM.

```bash
noid create myvm                         # matches default → uses golden (fast)
noid create myvm --cpus 1 --mem 2048     # matches default → uses golden (fast)
noid create myvm --cpus 2 --mem 512      # doesn't match  → cold boot (slow)
```

If you need a different default config, create a VM with those settings, checkpoint it, and promote:

```bash
noid create my-base --cpus 2 --mem 4096
# ... wait for boot, install tools ...
noid checkpoint my-base --label golden-2cpu
sudo bash scripts/provision-golden.sh --from-checkpoint <id>
```

The golden `config.json` will now contain `"cpus": 2, "mem_mib": 4096`, and VMs created with `--cpus 2 --mem 4096` will use the fast path.

Only one golden snapshot config is supported at a time. VMs with different configs always cold-boot.

## Troubleshooting

### "noid CLI not found on PATH"

The script looks for `noid` in `~/.cargo/bin/`, `/usr/local/bin/`, and `~/.local/bin/`. Ensure the binary exists at one of these locations or add it to `PATH`.

### "Server not responding at .../healthz"

The noid-server must be running. Start it:

```bash
noid-server serve --config server.toml &
```

### "VM failed to boot after 60 retries"

The temporary VM couldn't boot. Check:
- Is the kernel image valid? `strings ~/vmlinux.bin | grep 'Linux version'`
- Is the rootfs valid? `file ~/rootfs.ext4`
- Is `/dev/kvm` accessible? `ls -la /dev/kvm`
- Check serial output: look for `Kernel panic` in server logs

### VMs still cold-booting after provisioning

The golden config doesn't match the requested config. Check:

```bash
cat ~/.noid/golden/config.json
```

If it says `"cpus": 1, "mem_mib": 2048` but you're creating VMs with `--cpus 2`, that's a mismatch. Re-provision with the config you want.

### HTTPS/TLS hangs in VMs created from golden

The snapshot may have been taken before the kernel's CRNG (cryptographic random number generator) was initialized. This causes `getrandom()` to block, which hangs TLS.

Fix: ensure the VM ran for at least 5 seconds before checkpointing (the kernel needs time to seed the CRNG via `virtio-rng`). The provisioning script waits for the VM to respond to `echo ready`, which is sufficient.

Validate:

```bash
bash scripts/test-golden-entropy.sh
```

### Clock drift in VMs

Golden snapshots freeze the guest clock at snapshot time. The server automatically syncs the guest clock with the host during `noid create`, so this is handled transparently. If you see stale timestamps inside a VM, the clock sync may have failed -- check server logs for "failed to reconfigure guest network".
