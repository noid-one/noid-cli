#!/bin/bash
# install.sh — Complete noid installation: dependencies, Firecracker, binaries, networking, rootfs.
# Usage: sudo bash scripts/install.sh
set -euo pipefail

FC_VERSION="1.14.1"
FC_ARCH="x86_64"
KERNEL_VERSION="6.1"
NOID_DIR="/home/firecracker"
NOID_REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="/usr/local/bin"
ROOTFS_PATH="${NOID_DIR}/rootfs-ubuntu2404.ext4"
KERNEL_PATH="${NOID_DIR}/vmlinux.bin"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

step() { echo -e "\n${GREEN}==>${NC} $1"; }
warn() { echo -e "${YELLOW}    [skip]${NC} $1"; }
fail() { echo -e "${RED}ERROR:${NC} $1"; exit 1; }

# --- Pre-checks ---

if [ "$(id -u)" -ne 0 ]; then
    fail "must run as root: sudo bash scripts/install.sh"
fi

if [ "$(uname -m)" != "x86_64" ]; then
    fail "only x86_64 is supported (got $(uname -m))"
fi

# --- Step 1: System dependencies ---

step "Installing system dependencies"
apt-get update -qq
apt-get install -y -qq \
    build-essential curl wget git \
    debootstrap e2fsprogs \
    iptables iproute2 \
    acl \
    > /dev/null
echo "    done"

# --- Step 2: Rust toolchain ---

step "Checking Rust toolchain"
CARGO_BIN="/home/firecracker/.cargo/bin"
export PATH="${CARGO_BIN}:${PATH}"
if [ -x "${CARGO_BIN}/cargo" ]; then
    warn "cargo already installed ($("${CARGO_BIN}/cargo" --version))"
else
    echo "    Installing Rust via rustup..."
    sudo -u firecracker bash -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y'
    echo "    done"
fi

# --- Step 3: Firecracker ---

step "Checking Firecracker"
if [ -x "${BIN_DIR}/firecracker" ]; then
    INSTALLED_FC=$("${BIN_DIR}/firecracker" --version 2>&1 | head -1 || true)
    if echo "$INSTALLED_FC" | grep -q "$FC_VERSION"; then
        warn "Firecracker ${FC_VERSION} already installed"
    else
        echo "    Upgrading from ${INSTALLED_FC} to ${FC_VERSION}"
        FC_NEEDS_INSTALL=1
    fi
else
    FC_NEEDS_INSTALL=1
fi

if [ "${FC_NEEDS_INSTALL:-}" = "1" ]; then
    echo "    Downloading Firecracker ${FC_VERSION}..."
    FC_URL="https://github.com/firecracker-microvm/firecracker/releases/download/v${FC_VERSION}/firecracker-v${FC_VERSION}-${FC_ARCH}.tgz"
    TMPDIR=$(mktemp -d)
    wget -q -O "${TMPDIR}/fc.tgz" "$FC_URL"
    tar -xzf "${TMPDIR}/fc.tgz" -C "$TMPDIR"
    cp "${TMPDIR}/release-v${FC_VERSION}-${FC_ARCH}/firecracker-v${FC_VERSION}-${FC_ARCH}" "${BIN_DIR}/firecracker"
    chmod +x "${BIN_DIR}/firecracker"
    rm -rf "$TMPDIR"
    echo "    installed: $("${BIN_DIR}/firecracker" --version 2>&1 | head -1)"
fi

# --- Step 4: Kernel ---

step "Checking kernel image"
if [ -f "$KERNEL_PATH" ]; then
    warn "kernel already exists at ${KERNEL_PATH}"
else
    echo "    Downloading Firecracker kernel ${KERNEL_VERSION}..."
    KERNEL_URL="https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/${FC_ARCH}/kernels/vmlinux-${KERNEL_VERSION}.bin"
    wget -q -O "$KERNEL_PATH" "$KERNEL_URL"
    chown firecracker:firecracker "$KERNEL_PATH"
    echo "    saved: ${KERNEL_PATH} ($(du -h "$KERNEL_PATH" | cut -f1))"
fi

# --- Step 5: Build noid ---

step "Building noid workspace"
cd "$NOID_REPO"
sudo -u firecracker env PATH="${CARGO_BIN}:${PATH}" bash -c "cd ${NOID_REPO} && cargo build --release --workspace" 2>&1 | tail -3

echo "    Installing binaries to ${BIN_DIR}"
cp target/release/noid "${BIN_DIR}/noid"
cp target/release/noid-server "${BIN_DIR}/noid-server"
cp target/release/noid-netd "${BIN_DIR}/noid-netd"
chmod +x "${BIN_DIR}/noid" "${BIN_DIR}/noid-server" "${BIN_DIR}/noid-netd"

# --- Step 6: Networking ---

step "Configuring host networking"

# IP forwarding
sysctl -w net.ipv4.ip_forward=1 > /dev/null
if ! grep -q '^net.ipv4.ip_forward=1' /etc/sysctl.conf 2>/dev/null; then
    echo 'net.ipv4.ip_forward=1' >> /etc/sysctl.conf
fi

# Detect default interface
DEFAULT_IF=$(ip route show default | awk '{print $5}' | head -1)
if [ -z "$DEFAULT_IF" ]; then
    fail "cannot detect default network interface"
fi

# iptables NAT
if ! iptables -t nat -C POSTROUTING -s 172.16.0.0/16 -o "$DEFAULT_IF" -j MASQUERADE 2>/dev/null; then
    iptables -t nat -A POSTROUTING -s 172.16.0.0/16 -o "$DEFAULT_IF" -j MASQUERADE
fi
if ! iptables -C FORWARD -i noid+ -o "$DEFAULT_IF" -j ACCEPT 2>/dev/null; then
    iptables -A FORWARD -i noid+ -o "$DEFAULT_IF" -j ACCEPT
fi
if ! iptables -C FORWARD -i "$DEFAULT_IF" -o noid+ -m state --state RELATED,ESTABLISHED -j ACCEPT 2>/dev/null; then
    iptables -A FORWARD -i "$DEFAULT_IF" -o noid+ -m state --state RELATED,ESTABLISHED -j ACCEPT
fi
echo "    NAT: 172.16.0.0/16 via ${DEFAULT_IF}"

# noid-netd systemd service
cp scripts/noid-netd.service /etc/systemd/system/noid-netd.service
systemctl daemon-reload
systemctl enable noid-netd > /dev/null 2>&1
systemctl restart noid-netd
echo "    noid-netd: running"

# --- Step 7: Rootfs ---

step "Checking rootfs"
if [ -f "$ROOTFS_PATH" ]; then
    warn "rootfs already exists at ${ROOTFS_PATH} ($(du -h "$ROOTFS_PATH" | cut -f1))"
    BUILD_ROOTFS=0
else
    BUILD_ROOTFS=1
fi

if [ "$BUILD_ROOTFS" = "1" ]; then
    echo "    Building Ubuntu 24.04 LTS rootfs (this takes a few minutes)..."
    MNT="/tmp/noid-rootfs-mnt"
    SIZE_MB=4096

    dd if=/dev/zero of="$ROOTFS_PATH" bs=1M count="$SIZE_MB" status=none
    mkfs.ext4 -qF "$ROOTFS_PATH"

    mkdir -p "$MNT"
    mount -o loop "$ROOTFS_PATH" "$MNT"

    rootfs_cleanup() {
        umount "$MNT/proc" 2>/dev/null || true
        umount "$MNT/sys" 2>/dev/null || true
        umount "$MNT/dev/pts" 2>/dev/null || true
        umount "$MNT/dev" 2>/dev/null || true
        umount "$MNT" 2>/dev/null || true
        rmdir "$MNT" 2>/dev/null || true
    }
    trap rootfs_cleanup EXIT

    echo "    debootstrap noble..."
    debootstrap --include=\
systemd,systemd-sysv,dbus,\
iproute2,iputils-ping,dnsutils,iptables,\
ca-certificates,curl,wget,\
openssh-server,sudo,\
vim,less,git,build-essential \
noble "$MNT" http://archive.ubuntu.com/ubuntu > /dev/null

    mount --bind /dev "$MNT/dev"
    mount --bind /dev/pts "$MNT/dev/pts"
    mount -t proc proc "$MNT/proc"
    mount -t sysfs sys "$MNT/sys"

    cat > "$MNT/setup.sh" << 'CHROOT_SCRIPT'
#!/bin/bash
set -euo pipefail

# DNS
cat > /etc/resolv.conf << 'EOF'
nameserver 1.1.1.1
nameserver 8.8.8.8
EOF

echo "noid" > /etc/hostname

# noid user
useradd -m -s /bin/bash -G sudo noid
passwd -d noid
echo 'noid ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/noid
chmod 0440 /etc/sudoers.d/noid

# SSH
mkdir -p /etc/ssh
sed -i 's/^#\?PasswordAuthentication.*/PasswordAuthentication no/' /etc/ssh/sshd_config
sed -i 's/^#\?PubkeyAuthentication.*/PubkeyAuthentication yes/' /etc/ssh/sshd_config

# Serial console
mkdir -p /etc/systemd/system/serial-getty@ttyS0.service.d
cat > /etc/systemd/system/serial-getty@ttyS0.service.d/override.conf << 'EOF'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --keep-baud 115200,57600,38400,9600 ttyS0 $TERM
EOF
systemctl enable serial-getty@ttyS0.service

# Networking
cat > /etc/systemd/network/20-eth0.network << 'EOF'
[Match]
Name=eth0

[Network]
DHCP=no
EOF
systemctl enable systemd-networkd
systemctl enable systemd-resolved

# Node.js 22 LTS
curl -fsSL https://deb.nodesource.com/setup_22.x | bash -
apt-get install -y nodejs

# Claude Code
npm install -g @anthropic-ai/claude-code

# Cleanup
systemctl disable apt-daily.timer 2>/dev/null || true
systemctl disable apt-daily-upgrade.timer 2>/dev/null || true
apt-get clean
rm -rf /var/lib/apt/lists/* /tmp/* /var/tmp/*
CHROOT_SCRIPT

    chmod +x "$MNT/setup.sh"
    chroot "$MNT" /setup.sh
    rm "$MNT/setup.sh"

    umount "$MNT/proc"
    umount "$MNT/sys"
    umount "$MNT/dev/pts"
    umount "$MNT/dev"
    umount "$MNT"
    rmdir "$MNT"
    trap - EXIT

    chown firecracker:firecracker "$ROOTFS_PATH"
    echo "    rootfs built: ${ROOTFS_PATH} ($(du -h "$ROOTFS_PATH" | cut -f1))"
fi

# --- Step 8: Server config ---

step "Writing server.toml"
SERVER_TOML="${NOID_REPO}/server.toml"
cat > "$SERVER_TOML" << EOF
listen = "0.0.0.0:7654"
kernel = "${KERNEL_PATH}"
rootfs = "${ROOTFS_PATH}"
EOF
chown firecracker:firecracker "$SERVER_TOML"
echo "    ${SERVER_TOML}"

# --- Step 9: Reset DB (schema changed) ---

step "Resetting database (schema updated)"
DB_PATH="${NOID_DIR}/.noid/noid.db"
if [ -f "$DB_PATH" ]; then
    rm "$DB_PATH"
    echo "    deleted old ${DB_PATH}"
else
    warn "no existing database"
fi

# --- Done ---

echo ""
echo -e "${GREEN}=== noid installed ===${NC}"
echo ""
echo "  Binaries:     ${BIN_DIR}/noid, noid-server, noid-netd"
echo "  Firecracker:  ${BIN_DIR}/firecracker (v${FC_VERSION})"
echo "  Kernel:       ${KERNEL_PATH}"
echo "  Rootfs:       ${ROOTFS_PATH}"
echo "  Config:       ${SERVER_TOML}"
echo "  Networking:   172.16.0.0/16 NAT via ${DEFAULT_IF}"
echo "  noid-netd:    $(systemctl is-active noid-netd)"
echo ""
echo "Next steps:"
echo "  1. Start the server:  noid-server serve --config server.toml"
echo "  2. Add a user:        noid-server add-user alice"
echo "  3. Configure client:  noid auth setup --url http://localhost:7654 --token <token>"
echo "  4. Create a VM:       noid create myvm"
echo "  5. Test networking:   noid exec --name myvm -- ping -c3 1.1.1.1"
