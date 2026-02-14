# noid

A CLI for managing Firecracker microVMs with instant checkpointing and restore.

Create VMs in one command. Checkpoint them instantly. Clone and restore from any checkpoint.

noid runs as a **client-server** system: `noid-server` manages Firecracker VMs on a Linux host, and `noid` is a CLI client that talks to the server over HTTP and WebSocket. The client can run from anywhere.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/noid-one/noid-cli/master/install.sh | bash
```

This installs both `noid` and `noid-server` to `~/.local/bin/`. Or download manually:

```bash
mkdir -p ~/.local/bin

# Client (run from anywhere)
curl -fsSL -o ~/.local/bin/noid \
  https://github.com/noid-one/noid-cli/releases/latest/download/noid
chmod +x ~/.local/bin/noid

# Server (run on the VM host)
curl -fsSL -o ~/.local/bin/noid-server \
  https://github.com/noid-one/noid-cli/releases/latest/download/noid-server
chmod +x ~/.local/bin/noid-server
```

Make sure `~/.local/bin` is in your `PATH`. Both binaries can update themselves:

```bash
noid update
noid-server update
```

Or build from source:

```bash
git clone https://github.com/noid-one/noid-cli.git && cd noid-cli
cargo build --release --workspace
cp target/release/noid target/release/noid-server ~/.local/bin/
```

## Server setup

The server needs a Linux host with KVM support (`/dev/kvm`), Firecracker installed at `/usr/local/bin/firecracker`, and a kernel + rootfs image.

### 1. Get a kernel and rootfs

The install script handles everything (kernel build, rootfs build, networking, Firecracker):

```bash
sudo bash scripts/install-server.sh
```

This builds kernel 6.12.71 from source and creates an Ubuntu 25.04 rootfs. Re-running the installer is safe — it validates the existing kernel version and only rebuilds if the version doesn't match. If you previously had an older kernel (e.g. 4.14 or 6.1), it will be replaced automatically.

### 2. Configure and start the server

Edit `server.toml` to point to your images:

```toml
kernel = "/home/youruser/vmlinux.bin"
rootfs = "/home/youruser/rootfs.ext4"

# listen = "0.0.0.0:7654"       # default: binds all interfaces on port 7654
```

See `server.toml.example` for all options.

The default port is 7654, which does not require root.

```bash
noid-server serve --config server.toml
```

### 3. Add a user

```bash
noid-server add-user alice
```

This prints an API token (`noid_tok_...`). Save it — it can't be retrieved later.

## Client setup

From any machine (local or remote), configure the client to point at the server:

```bash
noid auth setup --url http://your-server --token noid_tok_...
```

Verify the connection:

```bash
noid whoami
# User: alice
# ID:   a1b2c3d4-...
```

## Usage

### Create a VM

```bash
noid create my-vm
# VM 'my-vm' created (state: running)

noid create beefy-vm --cpus 4 --mem 512
```

### Run commands

```bash
noid exec --name my-vm -- uname -a
# Linux noid 6.12.71 ...
```

Set an active VM to skip `--name`:

```bash
noid use my-vm
noid exec -- cat /etc/os-release
```

### Interactive console

```bash
noid console my-vm
```

Press **Ctrl+Q** to detach (the VM keeps running).

### Checkpoint and restore

```bash
# Snapshot a running VM
noid checkpoint --name my-vm --label before-deploy

# List checkpoints
noid checkpoints my-vm

# Clone from a checkpoint into a new VM
noid restore --name my-vm a1b2c3d4 --as my-vm-copy

# Or restore in-place (replaces the current VM)
noid restore --name my-vm a1b2c3d4
```

On btrfs, checkpoints and clones are instant (zero-copy). On ext4, they fall back to regular file copies.

### List and destroy

```bash
noid list
noid info my-vm
noid destroy my-vm
```

## Command reference

### Client (`noid`)

| Command | Description |
|---------|-------------|
| `noid auth setup --url URL --token TOKEN` | Configure server connection |
| `noid whoami` | Show authenticated user info |
| `noid current` | Show active server and VM |
| `noid use <name>` | Set active VM for this directory |
| `noid create <name> [--cpus N] [--mem MiB]` | Create and boot a new VM |
| `noid destroy [name]` | Stop and remove a VM |
| `noid list` | List all VMs |
| `noid info [name]` | Show VM details |
| `noid exec [--name NAME] -- <command...>` | Run a command inside a VM |
| `noid console [name]` | Interactive serial console (Ctrl+Q to detach) |
| `noid checkpoint [--name NAME] [--label TEXT]` | Snapshot a running VM |
| `noid checkpoints [name]` | List checkpoints |
| `noid restore [--name NAME] <id> [--as NEW]` | Restore from checkpoint |
| `noid update` | Update noid to the latest release |

### Server (`noid-server`)

| Command | Description |
|---------|-------------|
| `noid-server serve --config PATH` | Start the server |
| `noid-server add-user <name>` | Create a user and print their token |
| `noid-server rotate-token <name>` | Rotate a user's token |
| `noid-server list-users` | List all users |
| `noid-server remove-user <name>` | Remove a user and all their data |
| `noid-server update` | Update noid-server to the latest release |

## Architecture

```
noid (client)  ──HTTP/WS──>  noid-server  ──unix socket──>  firecracker
   any machine                 VM host       │                microVM
                                             │
                               noid-netd  <──┘  (TAP/IP/NAT, runs as root)
```

- REST API for lifecycle operations (create, destroy, list, checkpoint, restore)
- WebSocket for interactive sessions (console, exec)
- Token auth with SHA-256 hashed tokens and constant-time verification
- Multi-tenant: users are isolated at the DB and filesystem level
- No async runtime — fully synchronous (tiny_http + tungstenite + ureq)

### Workspace crates

| Crate | Purpose |
|-------|---------|
| `noid-client` | CLI binary (`noid`) |
| `noid-server` | Server binary (`noid-server`) |
| `noid-core` | VM engine: DB, storage, exec, auth |
| `noid-types` | Shared wire types (serde structs) |
| `noid-netd` | Privileged network daemon (TAP/IP/NAT) |
| `noid-local` | Legacy standalone CLI (pre-client-server) |

See [docs/server-guide.md](docs/server-guide.md) and [docs/client-guide.md](docs/client-guide.md) for detailed guides.
