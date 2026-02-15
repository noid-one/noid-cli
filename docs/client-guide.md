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
noid auth setup --url http://localhost --token noid_tok_your_token_here
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

The VM boots with default resources (1 vCPU, 256 MiB RAM). Override with flags:

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
| my-vm    | running | 1    | 256       | 2026-02-12 10:30:00 |
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
noid exec my-vm -- uname -a
```

```
Linux ubuntu-fc-uvm 4.14.174 #2 SMP ... x86_64 GNU/Linux
```

Commands after `--` are sent to the VM's shell via the serial console. You can run anything:

```bash
noid exec my-vm -- ls -la /
noid exec my-vm -- cat /etc/os-release
noid exec my-vm -- whoami
```

### Commands with special characters

Arguments are shell-escaped automatically, so spaces and special characters work:

```bash
noid exec my-vm -- echo "hello world"
noid exec my-vm -- sh -c "ls -la | head -5"
```

### Environment variables

Pass environment variables scoped to a single command with `-e` / `--env`:

```bash
noid exec -e GREETING=hello -- sh -c 'echo $GREETING'
```

```
hello
```

Each `-e` flag sets one variable. Use multiple flags for multiple variables:

```bash
noid exec -e DB_HOST=localhost -e DB_PORT=5432 -- psql
```

Variables only exist for the lifetime of that command -- they are not exported into the VM's shell environment and don't persist across exec calls:

```bash
noid exec -e SECRET=hunter2 -- sh -c 'echo $SECRET'   # prints: hunter2
noid exec -- sh -c 'echo $SECRET'                      # prints: (empty)
```

This is strictly safer than running `export` inside a console session: the value never touches shell history, never leaks to other processes, and has minimal blast radius.

Values are shell-escaped automatically, so special characters in values work safely:

```bash
noid exec -e MSG='hello world' -- sh -c 'echo $MSG'
noid exec -e API_KEY='sk-ant-abc123+/==' -- my-app
```

Variable names must match `[A-Za-z_][A-Za-z0-9_]*` (standard POSIX). Invalid names are rejected client-side before reaching the server.

### Exit codes

`noid exec` forwards the exit code from the command inside the VM:

```bash
noid exec my-vm -- true   # exits 0
noid exec my-vm -- false  # exits 1
```

Special exit code `124` means the command timed out (default: 30 seconds, configured server-side).

## Step 6: Interactive console

Attach to the VM's serial console for a live terminal session:

```bash
noid console my-vm
```

This gives you a direct, interactive shell inside the VM. Type commands, see output in real time.

**Type `exit` to detach.** The VM keeps running after you disconnect.

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
Server: http://localhost
Active VM: my-vm
```

This is useful when working on a project where each directory corresponds to a different VM.

## Step 8: Snapshot a running VM

A snapshot (called a "checkpoint" in noid) captures the **complete state** of a running VM at a point in time:

- **Memory** -- every byte of RAM, including in-flight data, open connections, and running processes
- **CPU state** -- registers, instruction pointer, and device state
- **Disk** -- the full root filesystem, including any files you've written or packages you've installed

Think of it like hibernating a laptop -- everything freezes in place and can be resumed later exactly as it was.

### Create a snapshot

```bash
noid checkpoint my-vm --label before-deploy
```

```
Checkpoint 'a1b2c3d4' created (label: before-deploy)
```

What happens under the hood:

1. The VM **pauses** (typically under 1 second)
2. Firecracker writes the memory and CPU state to disk
3. The root filesystem is copied (using copy-on-write on btrfs, so it's fast)
4. The VM **resumes** -- processes continue as if nothing happened

The pause is brief enough that network connections and running services generally survive it.

### Labels are optional but recommended

```bash
noid checkpoint my-vm                          # no label
noid checkpoint my-vm --label clean-install    # with label
noid checkpoint my-vm --label claude-code      # descriptive labels help later
```

Good labeling practice: describe what state the VM is in, not when you took the snapshot. The timestamp is recorded automatically.

### With an active VM

If you've set an active VM with `noid use`, you can omit the name:

```bash
noid use my-vm
noid checkpoint --label before-deploy
```

## Step 9: List snapshots

```bash
noid checkpoints my-vm
```

```
+------------------+-----------------------+---------------------+
| id               | label                 | created             |
+------------------+-----------------------+---------------------+
| a1b2c3d4e5f67890 | clean-install         | 2026-02-12 10:35:00 |
| 63eddf94ead340e2 | claude-code           | 2026-02-12 10:40:00 |
+------------------+-----------------------+---------------------+
```

Each checkpoint gets a unique 16-character ID. Use this ID (or any unique prefix of it) when restoring.

## Step 10: Restore from a snapshot

There are two ways to restore: **clone** into a new VM, or **restore in place**.

### Clone into a new VM

Create a new VM from a snapshot, leaving the original untouched:

```bash
noid restore my-vm a1b2c3d4e5f67890 --as my-vm-copy
```

```
VM 'my-vm-copy' restored from checkpoint 'a1b2c3d4e5f67890'
```

The new VM boots into the exact state captured in the snapshot -- same memory contents, same running processes, same files on disk. It gets its own network identity (new IP address, new TAP device), so it won't conflict with the original.

This is the recommended way to use snapshots for:
- Spinning up multiple identical VMs from a prepared base image
- Testing a change without risking your working environment
- Giving each team member a clone of a shared dev environment

### Restore in place

Replace the current VM's state with the snapshot:

```bash
noid restore my-vm a1b2c3d4e5f67890
```

This **destroys the current VM** (kills the process, removes its storage) and recreates it from the snapshot. Use this to "rewind" a VM to a known good state.

**Warning**: Any changes made since the snapshot was taken are lost. There is no undo.

### What happens during restore

1. A new Firecracker process starts
2. The snapshot's memory and CPU state are loaded
3. The filesystem is cloned from the checkpoint
4. The VM's network is reconfigured (new TAP device and IP address)
5. The guest's clock is updated (it was frozen at snapshot time)
6. The VM resumes -- processes pick up where they left off

Because the VM gets a new IP address on restore, any hardcoded IP references inside the guest won't be valid. DNS names and hostnames continue to work normally.

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

Use snapshots as save points while developing inside a VM:

```bash
# Create a VM and set it as active
noid create dev
noid use dev

# Install tools inside the VM
noid exec -- apt-get update
noid exec -- apt-get install -y build-essential nodejs npm

# Snapshot the clean state before making changes
noid checkpoint --label clean-with-tools

# Do your work...
noid exec -- sh -c "cd /app && npm install && npm run build"

# Something went wrong? Rewind to the clean state
noid checkpoints dev                          # find the checkpoint ID
noid restore dev <checkpoint-id>       # VM restarts from snapshot

# Happy with the result? Snapshot again before the next risky step
noid checkpoint --label after-build
```

### Prepared environment workflow

Set up a VM once, snapshot it, then clone it for each use:

```bash
# One-time setup: create and configure a base VM
noid create base
noid exec base -- apt-get update
noid exec base -- apt-get install -y python3 pip git curl
noid exec base -- pip install pytest requests

# Snapshot the prepared environment
noid checkpoint base --label ready

# Later: spin up clones whenever you need a fresh copy
noid restore base <checkpoint-id> --as alice-dev
noid restore base <checkpoint-id> --as bob-dev
noid restore base <checkpoint-id> --as ci-runner

# Each clone starts with all tools pre-installed
noid exec alice-dev -- python3 --version   # works immediately
```

### Testing workflow

Spin up isolated VMs for parallel testing:

```bash
# Create a base VM and set it up
noid create test-base
noid exec test-base -- apt-get update
noid exec test-base -- apt-get install -y python3
noid checkpoint test-base --label ready

# Clone it for each test run
noid restore test-base <checkpoint-id> --as test-run-1
noid restore test-base <checkpoint-id> --as test-run-2
noid restore test-base <checkpoint-id> --as test-run-3

# Run tests in parallel
noid exec test-run-1 -- python3 /tests/suite_a.py &
noid exec test-run-2 -- python3 /tests/suite_b.py &
noid exec test-run-3 -- python3 /tests/suite_c.py &
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
noid exec "preview-${BRANCH}" -- sh -c "cd /app && git pull && ./start.sh"
# ... run tests, show preview ...
noid destroy "preview-${BRANCH}"
```

### Injecting secrets

Use `-e` to pass credentials without shell history exposure:

```bash
noid exec -e ANTHROPIC_API_KEY=sk-ant-... -- claude
noid exec -e AWS_ACCESS_KEY_ID=AKIA... -e AWS_SECRET_ACCESS_KEY=... -- aws s3 ls
```

The variable exists only for the duration of that command. Compare this to the unsafe alternative of running `export SECRET=...` inside a console session, where the value persists in the shell and may appear in history.

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
| `noid exec [name] [-e KEY=VAL]... -- <command...>` | Run a command inside a VM |
| `noid console [name]` | Attach interactive serial console (type "exit" to detach) |
| `noid checkpoint [name] [--label TEXT]` | Snapshot a running VM (memory + disk + CPU) |
| `noid checkpoints [name]` | List snapshots for a VM |
| `noid restore [name] <id> [--as NEW]` | Restore or clone a VM from a snapshot |
| `noid destroy [name]` | Stop and remove a VM |

All commands that take a VM name accept it as a positional argument. The name is optional if an active VM is set via `noid use`.

## Client config files

### ~/.noid/config.toml

Created by `noid auth setup`. Stores server URL and token:

```toml
[server]
url = "http://localhost"
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
