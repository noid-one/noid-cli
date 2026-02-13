#!/bin/bash
# install-server.sh — Complete noid server provisioning: dependencies, Firecracker, binaries, networking, rootfs.
# Usage: sudo bash scripts/install-server.sh
set -euo pipefail

FC_VERSION="1.14.1"
FC_ARCH="x86_64"
KERNEL_VERSION="6.1"
NOID_DIR="/home/firecracker"
NOID_REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="/usr/local/bin"
USER_BIN_DIR="${NOID_DIR}/.local/bin"
ROOTFS_PATH="${NOID_DIR}/rootfs.ext4"
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
    fail "must run as root: sudo bash scripts/install-server.sh"
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
export PATH="${USER_BIN_DIR}:${CARGO_BIN}:${PATH}"
if [ -x "${CARGO_BIN}/cargo" ]; then
    warn "cargo already installed ($(sudo -u firecracker "${CARGO_BIN}/cargo" --version 2>/dev/null))"
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

echo "    Installing binaries"
# Stop running daemons so we can overwrite their binaries
for proc in noid-server noid-netd; do
    if pkill -x "$proc" 2>/dev/null; then
        echo "    stopped running ${proc}"
        sleep 0.2
    fi
done
mkdir -p "$USER_BIN_DIR"
chown firecracker:firecracker "$USER_BIN_DIR"
cp target/release/noid "${USER_BIN_DIR}/noid"
cp target/release/noid-server "${USER_BIN_DIR}/noid-server"
cp target/release/noid-netd "${BIN_DIR}/noid-netd"
chmod +x "${USER_BIN_DIR}/noid" "${USER_BIN_DIR}/noid-server" "${BIN_DIR}/noid-netd"
chown firecracker:firecracker "${USER_BIN_DIR}/noid" "${USER_BIN_DIR}/noid-server"

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
    echo "    Building Ubuntu 25.04 rootfs (this takes a few minutes)..."
    MNT="/tmp/noid-rootfs-mnt"
    SIZE_MB=2048

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

    echo "    debootstrap plucky..."
    debootstrap --include=\
systemd,systemd-sysv,dbus,login,\
iproute2,iputils-ping,\
ca-certificates,curl,wget,sudo \
plucky "$MNT" http://archive.ubuntu.com/ubuntu

    mount --bind /dev "$MNT/dev"
    mount --bind /dev/pts "$MNT/dev/pts"
    mount -t proc proc "$MNT/proc"
    mount -t sysfs sys "$MNT/sys"

    cat > "$MNT/setup.sh" << 'CHROOT_SCRIPT'
#!/bin/bash
set -euo pipefail

# DNS — remove any symlink (Ubuntu defaults to resolved stub) and write a real file
rm -f /etc/resolv.conf
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

# Serial console
mkdir -p /etc/systemd/system/serial-getty@ttyS0.service.d
cat > /etc/systemd/system/serial-getty@ttyS0.service.d/override.conf << 'EOF'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin noid --keep-baud 115200,57600,38400,9600 ttyS0 $TERM
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
# Keep DNS static via /etc/resolv.conf for minimal images. resolved can fail
# in stripped environments and is not required for noid exec.
systemctl disable systemd-resolved 2>/dev/null || true
systemctl mask systemd-resolved.service 2>/dev/null || true

# Disable unnecessary services for fast boot
systemctl disable cron.service 2>/dev/null || true
systemctl disable rsyslog.service 2>/dev/null || true
systemctl mask getty@tty1.service
systemctl mask systemd-timesyncd.service
systemctl mask e2scrub_all.timer
systemctl mask fstrim.timer
systemctl mask logrotate.timer
systemctl mask motd-news.timer
systemctl mask dpkg-db-backup.timer
systemctl mask console-setup.service
systemctl mask systemd-update-utmp.service
systemctl mask systemd-update-utmp-runlevel.service

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

# --- Step 8: Reset DB (schema may have changed) ---

step "Resetting database (schema updated)"
DB_PATH="${NOID_DIR}/.noid/noid.db"
if [ -f "$DB_PATH" ]; then
    rm "$DB_PATH"
    echo "    deleted old ${DB_PATH}"
else
    warn "no existing database"
fi

# --- Step 9: Golden snapshot ---

step "Creating golden snapshot"
GOLDEN_DIR="${NOID_DIR}/.noid/golden"

if [ -f "${GOLDEN_DIR}/memory.snap" ]; then
    warn "golden snapshot already exists at ${GOLDEN_DIR}"
else
    echo "    Starting noid-server for template VM..."

    # Write a temp server config
    GOLDEN_TOML=$(mktemp)
    cat > "$GOLDEN_TOML" << GCEOF
listen = "127.0.0.1:7654"
kernel = "${KERNEL_PATH}"
rootfs = "${ROOTFS_PATH}"
exec_timeout_secs = 30
GCEOF
    chown firecracker:firecracker "$GOLDEN_TOML"

    # Start server in background
    sudo -u firecracker env PATH="${CARGO_BIN}:${PATH}" \
        noid-server serve --config "$GOLDEN_TOML" &
    SERVER_PID=$!
    sleep 1

    golden_cleanup() {
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
        rm -f "$GOLDEN_TOML"
        # Ensure noid-netd is running even if the script exits early
        systemctl start noid-netd 2>/dev/null || true
    }
    trap golden_cleanup EXIT

    # Create template user and capture token
    GOLDEN_TOKEN=$(sudo -u firecracker env PATH="${USER_BIN_DIR}:${CARGO_BIN}:${PATH}" \
        noid-server add-user _template 2>/dev/null)
    echo "    Template user created"

    # Configure CLI
    sudo -u firecracker env PATH="${CARGO_BIN}:${PATH}" HOME="${NOID_DIR}" \
        noid auth setup --url http://127.0.0.1:7654 --token "$GOLDEN_TOKEN"
    echo "    CLI configured"

    # Create template VM
    echo "    Creating template VM (cold boot)..."
    sudo -u firecracker env PATH="${CARGO_BIN}:${PATH}" HOME="${NOID_DIR}" \
        noid create _golden

    # Wait for VM to be ready (poll with exec)
    echo "    Waiting for VM to boot..."
    RETRIES=0
    MAX_RETRIES=60
    while [ "$RETRIES" -lt "$MAX_RETRIES" ]; do
        if sudo -u firecracker env PATH="${CARGO_BIN}:${PATH}" HOME="${NOID_DIR}" \
            noid exec --name _golden -- echo ready 2>/dev/null | grep -q ready; then
            break
        fi
        RETRIES=$((RETRIES + 1))
        sleep 2
    done
    if [ "$RETRIES" -ge "$MAX_RETRIES" ]; then
        echo ""
        echo -e "    ${RED}Template VM failed to boot within timeout${NC}"
        echo -e "    ${YELLOW}VMs will use slow cold-boot (no golden snapshot)${NC}"
        echo ""
        sudo -u firecracker env PATH="${CARGO_BIN}:${PATH}" HOME="${NOID_DIR}" \
            noid destroy --name _golden 2>/dev/null || true
        sudo -u firecracker env PATH="${USER_BIN_DIR}:${CARGO_BIN}:${PATH}" \
            noid-server remove-user _template 2>/dev/null || true
        golden_cleanup
        trap - EXIT
    else
        echo "    Template VM ready"

        # Take checkpoint
        echo "    Taking golden snapshot..."
        sudo -u firecracker env PATH="${CARGO_BIN}:${PATH}" HOME="${NOID_DIR}" \
            noid checkpoint --name _golden --label golden

        # Find the checkpoint files — they're in the VM's subvolume
        # The checkpoint command creates memory.snap + vmstate.snap in the VM dir
        # We need to copy them to the golden dir
        mkdir -p "$GOLDEN_DIR"
        chown firecracker:firecracker "$GOLDEN_DIR"

        # Locate the template VM directory without requiring sqlite3 on host.
        VM_DIR=$(find "${NOID_DIR}/.noid/storage/users" -mindepth 3 -maxdepth 3 -type d -path "*/vms/_golden" | head -1)
        if [ -z "${VM_DIR}" ] || [ ! -d "${VM_DIR}" ]; then
            fail "could not locate template VM directory for _golden"
        fi

        cp --reflink=auto "${VM_DIR}/rootfs.ext4" "${GOLDEN_DIR}/rootfs.ext4"
        cp "${VM_DIR}/memory.snap" "${GOLDEN_DIR}/memory.snap"
        cp "${VM_DIR}/vmstate.snap" "${GOLDEN_DIR}/vmstate.snap"

        # Write template config for compatibility checking
        cat > "${GOLDEN_DIR}/config.json" << CONFEOF
{"cpus": 1, "mem_mib": 128, "snapshot_rootfs_path": "${VM_DIR}/rootfs.ext4"}
CONFEOF
        chown -R firecracker:firecracker "$GOLDEN_DIR"
        echo "    Golden snapshot saved to ${GOLDEN_DIR}"

        # Cleanup: destroy template VM and user
        sudo -u firecracker env PATH="${CARGO_BIN}:${PATH}" HOME="${NOID_DIR}" \
            noid destroy --name _golden 2>/dev/null || true
        sudo -u firecracker env PATH="${USER_BIN_DIR}:${CARGO_BIN}:${PATH}" \
            noid-server remove-user _template 2>/dev/null || true

        golden_cleanup
        trap - EXIT
    fi
fi

# --- Step 10: Server config ---

step "Writing server.toml (for production use)"
SERVER_TOML="${NOID_REPO}/server.toml"
cat > "$SERVER_TOML" << EOF
listen = "0.0.0.0:7654"
kernel = "${KERNEL_PATH}"
rootfs = "${ROOTFS_PATH}"
EOF
chown firecracker:firecracker "$SERVER_TOML"
echo "    ${SERVER_TOML}"

# --- Done ---

echo ""
echo -e "${GREEN}=== noid installed ===${NC}"
echo ""
echo "  User bins:    ${USER_BIN_DIR}/noid, noid-server"
echo "  System bin:   ${BIN_DIR}/noid-netd"
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
echo "  5. Optional: Install Claude Code in a VM, then checkpoint to update the golden snapshot"
