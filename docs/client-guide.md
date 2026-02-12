# Client & Developer Guide

This guide walks through using the `noid` CLI client to manage Firecracker microVMs against a running noid server.

## Prerequisites

- A running noid server (see [Server Administration Guide](server-guide.md))
- An API token from your server admin
- The `noid` binary installed

## Step 1: Install the client

```bash
cd noid/
cargo build --release -p noid-client
sudo cp target/release/noid $HOME/.local/bin/
```

## Step 2: Configure authentication

Connect the client to your server:

```bash
noid auth setup --url http://127.0.0.1:7654 --token noid_tok_your_token_here
```

This saves the server URL and token to `~/.noid/config.toml` and verifies the connection by calling the server's `whoami` endpoint.

On success:

```
Configuration saved.
Logged in as: alice (id: abc123...)
```

### Verify your identity

```bash
noid whoami
```

```
User: alice
ID:   a1b2c3d4-e5f6-7890-abcd-ef1234567890
```

### Check server capabilities

```bash
noid current
```

Shows the active server URL and the currently selected VM (if any).

## Step 3: Create a VM

```bash
noid create my-vm
```

```
VM 'my-vm' created (state: running)
```

The VM boots with default resources (1 vCPU, 128 MiB RAM). Override with flags:

```bash
noid create beefy-vm --cpus 4 --mem 512
```

The server copies the base rootfs for each new VM, so every VM gets its own independent filesystem.

## Step 4: List your VMs

```bash
noid list
```

```
+----------+---------+------+-----------+---------------------+
| name     | state   | cpus | mem (MiB) | created             |
+----------+---------+------+-----------+---------------------+
| my-vm    | running | 1    | 128       | 2026-02-12 10:30:00 |
| beefy-vm | running | 4    | 512       | 2026-02-12 10:31:00 |
+----------+---------+------+-----------+---------------------+
```

- `running` = Firecracker process is alive
- `dead` = process has exited

### Get details on a single VM

```bash
noid info my-vm
```

## Step 5: Run commands inside a VM

```bash
noid exec --name my-vm -- uname -a
```

```
Linux ubuntu-fc-uvm 4.14.174 #2 SMP ... x86_64 GNU/Linux
```

Commands after `--` are sent to the VM's shell via the serial console. You can run anything:

```bash
noid exec --name my-vm -- ls -la /
noid exec --name my-vm -- cat /etc/os-release
noid exec --name my-vm -- whoami
```

### Commands with special characters

Arguments are shell-escaped automatically, so spaces and special characters work:

```bash
noid exec --name my-vm -- echo "hello world"
noid exec --name my-vm -- sh -c "ls -la | head -5"
```

### Exit codes

`noid exec` forwards the exit code from the command inside the VM:

```bash
noid exec --name my-vm -- true   # exits 0
noid exec --name my-vm -- false  # exits 1
```

Special exit code `124` means the command timed out (default: 30 seconds, configured server-side).

## Step 6: Interactive console

Attach to the VM's serial console for a live terminal session:

```bash
noid console my-vm
```

This gives you a direct, interactive shell inside the VM. Type commands, see output in real time.

**Press Ctrl+Q to detach.** The VM keeps running after you disconnect.

The console uses WebSocket for bidirectional communication. Only one console session per VM at a time.

## Step 7: Set an active VM

If you're working with the same VM repeatedly, set it as the active VM for the current directory:

```bash
noid use my-vm
```

This writes the VM name to a `.noid` file in the current directory. After this, you can omit the VM name from commands:

```bash
noid exec -- ls -la        # targets my-vm
noid console                # targets my-vm
noid info                   # targets my-vm
noid checkpoint             # targets my-vm
noid destroy                # targets my-vm
```

Check which VM is currently active:

```bash
noid current
```

```
Server: http://127.0.0.1:7654
Active VM: my-vm
```

This is useful when working on a project where each directory corresponds to a different VM.

## Step 8: Checkpoint a running VM

Capture the complete state of a running VM -- memory, CPU registers, disk:

```bash
noid checkpoint --name my-vm --label before-deploy
```

```
Checkpoint 'a1b2c3d4' created (label: before-deploy)
```

The VM pauses briefly (typically under 1 second), snapshots everything, and resumes. Your processes keep running as if nothing happened.

### Labels are optional

```bash
noid checkpoint --name my-vm
```

Works fine without a label, but labels make it easier to identify checkpoints later.

## Step 9: List checkpoints

```bash
noid checkpoints my-vm
```

```
+----------+---------------+---------------------+
| id       | label         | created             |
+----------+---------------+---------------------+
| a1b2c3d4 | before-deploy | 2026-02-12 10:35:00 |
| e5f67890 |               | 2026-02-12 10:40:00 |
+----------+---------------+---------------------+
```

## Step 10: Restore from a checkpoint

### Clone into a new VM

Create a new VM from a checkpoint, leaving the original untouched:

```bash
noid restore --name my-vm a1b2c3d4 --as my-vm-copy
```

```
VM 'my-vm-copy' restored from checkpoint 'a1b2c3d4'
```

The new VM starts from the exact state captured in the checkpoint -- same memory, same running processes, same filesystem. On btrfs, the clone is instant.

### Restore in place

Replace the current VM with the checkpoint state:

```bash
noid restore --name my-vm a1b2c3d4
```

This destroys the current VM and recreates it from the checkpoint. Use this to "rewind" a VM to a known good state.

## Step 11: Destroy a VM

```bash
noid destroy my-vm
```

```
VM 'my-vm' destroyed
```

This kills the Firecracker process, removes all storage (rootfs, logs, snapshots), and deletes the database entry.

## Workflow examples

### Development workflow

Use checkpoints as save points while developing inside a VM:

```bash
# Set up a fresh VM
noid create dev
noid use dev

# Install your dependencies
noid exec -- apt-get update
noid exec -- apt-get install -y build-essential

# Checkpoint the clean state
noid checkpoint --label clean-install

# Do some work...
noid exec -- sh -c "echo 'hello' > /tmp/test.txt"

# Something went wrong? Restore the clean state
noid restore --name dev <checkpoint-id>
```

### Testing workflow

Spin up isolated VMs for parallel testing:

```bash
# Create a base VM and set it up
noid create test-base
noid exec --name test-base -- apt-get update
noid exec --name test-base -- apt-get install -y python3
noid checkpoint --name test-base --label ready

# Clone it for each test run
noid restore --name test-base <checkpoint-id> --as test-run-1
noid restore --name test-base <checkpoint-id> --as test-run-2
noid restore --name test-base <checkpoint-id> --as test-run-3

# Run tests in parallel
noid exec --name test-run-1 -- python3 /tests/suite_a.py &
noid exec --name test-run-2 -- python3 /tests/suite_b.py &
noid exec --name test-run-3 -- python3 /tests/suite_c.py &
wait

# Clean up
noid destroy test-run-1
noid destroy test-run-2
noid destroy test-run-3
```

### CI/CD preview environments

Create short-lived VMs for each deployment:

```bash
BRANCH="feature-login"
noid create "preview-${BRANCH}" --cpus 2 --mem 256
noid exec --name "preview-${BRANCH}" -- sh -c "cd /app && git pull && ./start.sh"
# ... run tests, show preview ...
noid destroy "preview-${BRANCH}"
```

## Command reference

| Command | Description |
|---|---|
| `noid auth setup --url URL --token TOKEN` | Configure server connection |
| `noid whoami` | Show authenticated user info |
| `noid current` | Show active server and VM |
| `noid use <name>` | Set active VM for current directory |
| `noid create <name> [--cpus N] [--mem MiB]` | Create and boot a VM |
| `noid list` | List all VMs |
| `noid info [name]` | Show VM details |
| `noid exec [--name NAME] -- <command...>` | Run a command inside a VM |
| `noid console [name]` | Attach interactive serial console (Ctrl+Q to detach) |
| `noid checkpoint [--name NAME] [--label TEXT]` | Snapshot a running VM |
| `noid checkpoints [name]` | List checkpoints for a VM |
| `noid restore [--name NAME] <id> [--as NEW]` | Restore from a checkpoint |
| `noid destroy [name]` | Stop and remove a VM |

Commands that show `[name]` use a positional argument. Commands that show `[--name NAME]` use a flag. All are optional if an active VM is set via `noid use`.

## Client config files

### ~/.noid/config.toml

Created by `noid auth setup`. Stores server URL and token:

```toml
[server]
url = "http://127.0.0.1:7654"
token = "noid_tok_..."
```

### .noid (per-directory)

Created by `noid use <name>`. Plain text file containing just the VM name:

```
my-vm
```

## Troubleshooting

### "connection refused" or "failed to connect"

The server is not running or is on a different address. Check:
- Is `noid-server serve` running?
- Does the URL in `~/.noid/config.toml` match the server's `listen` address?

### "unauthorized" (401)

Your token is invalid or expired. Ask your server admin for a new token and re-run:
```bash
noid auth setup --url <url> --token <new-token>
```

### "too many requests" (429)

Too many failed authentication attempts. Wait 60 seconds and try again with the correct token.

### `noid exec` times out

The default timeout is 30 seconds (configured server-side). Possible causes:
- The VM hasn't finished booting yet -- wait a few seconds after `noid create`
- The command genuinely takes longer than the timeout
- The VM is unresponsive (check with `noid console`)

### VM shows as "dead"

The Firecracker process exited. This usually means:
- The VM crashed (check with your server admin)
- The host ran out of memory
- KVM is not available

You can `noid destroy` a dead VM and create a new one, or restore from a checkpoint.

### Console won't connect

- Check that the VM is in a `running` state with `noid info`
- The server has a max WebSocket session limit (default 32) -- other sessions may need to close first
