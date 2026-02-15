#!/bin/bash
# install-server.sh — Complete noid server provisioning: dependencies, Firecracker, binaries, networking, rootfs.
# Usage: sudo bash scripts/install-server.sh
set -euo pipefail

FC_VERSION="1.14.1"
FC_ARCH="x86_64"
KERNEL_FULL_VERSION="6.12.71"
NOID_DIR="/home/firecracker"
NOID_REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="/usr/local/bin"
USER_BIN_DIR="${NOID_DIR}/.local/bin"
ROOTFS_PATH="${NOID_DIR}/rootfs.ext4"
KERNEL_PATH="${NOID_DIR}/vmlinux.bin"
GOLDEN_DIR="${NOID_DIR}/.noid/golden"

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
    flex bison libelf-dev libssl-dev bc binutils \
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
NEED_KERNEL=0
if [ -f "$KERNEL_PATH" ]; then
    CURRENT_VERSION=$(strings "$KERNEL_PATH" | grep -oP 'Linux version \K[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "")
    if [ -z "$CURRENT_VERSION" ]; then
        warn "kernel at ${KERNEL_PATH} has unknown version — replacing"
        NEED_KERNEL=1
    elif [ "$CURRENT_VERSION" = "$KERNEL_FULL_VERSION" ]; then
        warn "kernel ${KERNEL_FULL_VERSION} already exists at ${KERNEL_PATH}"
    else
        warn "kernel at ${KERNEL_PATH} is version ${CURRENT_VERSION}, expected ${KERNEL_FULL_VERSION} — replacing"
        NEED_KERNEL=1
    fi
else
    NEED_KERNEL=1
fi
if [ "$NEED_KERNEL" = "1" ]; then
    echo "    Building kernel ${KERNEL_FULL_VERSION} from source (this takes a while)..."
    KERNEL_BUILD_DIR=$(mktemp -d)

    # Download kernel source
    KERNEL_TARBALL_URL="https://www.kernel.org/pub/linux/kernel/v6.x/linux-${KERNEL_FULL_VERSION}.tar.xz"
    echo "    Downloading ${KERNEL_TARBALL_URL}..."
    wget -q -O "${KERNEL_BUILD_DIR}/linux.tar.xz" "$KERNEL_TARBALL_URL"
    tar -xf "${KERNEL_BUILD_DIR}/linux.tar.xz" -C "$KERNEL_BUILD_DIR"
    KERNEL_SRC="${KERNEL_BUILD_DIR}/linux-${KERNEL_FULL_VERSION}"

    # Start from x86_64 defconfig — the standard config that includes all
    # virtio, block, and serial drivers as built-in. This is more reliable
    # than adapting the Firecracker CI 6.1 config via olddefconfig, which
    # silently drops or modularizes critical drivers on newer kernels.
    echo "    Generating x86_64 defconfig..."
    make -C "$KERNEL_SRC" defconfig > /dev/null 2>&1

    # Overlay Firecracker-specific options on top of defconfig.
    # Bake root=/dev/vda into the kernel — Firecracker presents the rootfs
    # as a virtio-blk device at /dev/vda. Without this, the kernel panics
    # with "VFS: Unable to mount root fs on unknown-block(0,0)".
    "${KERNEL_SRC}/scripts/config" --file "${KERNEL_SRC}/.config" \
        --enable VIRTIO_MMIO \
        --enable VIRTIO_BLK \
        --enable VIRTIO_NET \
        --enable VIRTIO_CONSOLE \
        --enable VIRTIO_BALLOON \
        --enable HW_RANDOM_VIRTIO \
        --enable SERIAL_8250 \
        --enable SERIAL_8250_CONSOLE \
        --enable EXT4_FS \
        --enable DEVTMPFS \
        --enable DEVTMPFS_MOUNT \
        --set-str CMDLINE "root=/dev/vda rw console=ttyS0" \
        --enable CMDLINE_BOOL

    # Resolve dependencies after config tweaks
    make -C "$KERNEL_SRC" olddefconfig > /dev/null 2>&1

    # Verify critical options are =y (not =m) — fail if any are wrong.
    echo "    Verifying kernel config..."
    CONFIG="${KERNEL_SRC}/.config"
    for opt in VIRTIO VIRTIO_MMIO VIRTIO_BLK EXT4_FS SERIAL_8250 SERIAL_8250_CONSOLE; do
        if ! grep -q "CONFIG_${opt}=y" "$CONFIG"; then
            echo "    FATAL: CONFIG_${opt} is not =y in .config:"
            grep "CONFIG_${opt}" "$CONFIG" || echo "    (missing entirely)"
            rm -rf "$KERNEL_BUILD_DIR"
            fail "kernel config verification failed: CONFIG_${opt} must be =y"
        fi
    done
    echo "    Config OK: all critical options are built-in"

    # Build uncompressed vmlinux (what Firecracker expects on x86_64)
    echo "    Compiling vmlinux ($(nproc) jobs)..."
    if ! make -C "$KERNEL_SRC" vmlinux -j"$(nproc)" 2>&1 | tail -20; then
        rm -rf "$KERNEL_BUILD_DIR"
        fail "kernel compilation failed"
    fi

    cp "${KERNEL_SRC}/vmlinux" "$KERNEL_PATH"
    chown firecracker:firecracker "$KERNEL_PATH"
    rm -rf "$KERNEL_BUILD_DIR"
    echo "    saved: ${KERNEL_PATH} ($(du -h "$KERNEL_PATH" | cut -f1))"

    # Invalidate golden snapshot — new kernel requires new snapshot
    if [ -d "$GOLDEN_DIR" ]; then
        echo "    Invalidating golden snapshot (kernel changed)"
        rm -rf "$GOLDEN_DIR"
    fi
fi

# --- Step 5: Build noid ---

step "Building noid workspace"
cd "$NOID_REPO"
sudo -u firecracker env PATH="${CARGO_BIN}:${PATH}" bash -c "cd ${NOID_REPO} && cargo build --release --workspace" 2>&1 | tail -3

echo "    Installing binaries"
# Stop running daemons so we can overwrite their binaries
for svc in noid-server noid-netd; do
    if systemctl is-active --quiet "${svc}.service" 2>/dev/null; then
        systemctl stop "${svc}.service"
        echo "    stopped ${svc}.service"
    elif pkill -x "$svc" 2>/dev/null; then
        echo "    stopped running ${svc}"
        sleep 0.2
    fi
done
mkdir -p "$USER_BIN_DIR"
chown firecracker:firecracker "$USER_BIN_DIR"
cp target/release/noid "${USER_BIN_DIR}/noid"
cp target/release/noid-server "${USER_BIN_DIR}/noid-server"
cp target/release/noid-netd "${USER_BIN_DIR}/noid-netd"
chmod +x "${USER_BIN_DIR}/noid" "${USER_BIN_DIR}/noid-server" "${USER_BIN_DIR}/noid-netd"
chown firecracker:firecracker "${USER_BIN_DIR}/noid" "${USER_BIN_DIR}/noid-server" "${USER_BIN_DIR}/noid-netd"

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
if ! iptables -t mangle -C FORWARD -p tcp --tcp-flags SYN,RST SYN -j TCPMSS --clamp-mss-to-pmtu 2>/dev/null; then
    iptables -t mangle -A FORWARD -p tcp --tcp-flags SYN,RST SYN -j TCPMSS --clamp-mss-to-pmtu
fi
echo "    NAT: 172.16.0.0/16 via ${DEFAULT_IF}"

# noid-netd systemd service (substitute actual binary path)
sed "s|/home/firecracker/.local/bin|${USER_BIN_DIR}|g" scripts/noid-netd.service > /etc/systemd/system/noid-netd.service
systemctl daemon-reload
systemctl enable noid-netd > /dev/null 2>&1
systemctl restart noid-netd
echo "    noid-netd: running"

# noid-server systemd service
sed "s|/home/firecracker/.local/bin|${USER_BIN_DIR}|g; s|/home/firecracker|${NOID_DIR}|g" \
    scripts/noid-server.service > /etc/systemd/system/noid-server.service
systemctl daemon-reload
systemctl enable noid-server > /dev/null 2>&1
echo "    noid-server: service installed"

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

# Map hostname to loopback so sudo (and other tools) can resolve it
cat > /etc/hosts << 'EOF'
127.0.0.1 localhost
127.0.1.1 noid
::1 localhost ip6-localhost ip6-loopback
EOF

# noid user
useradd -m -s /bin/bash -G sudo noid
passwd -d noid
echo 'noid ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/noid
chmod 0440 /etc/sudoers.d/noid
touch /home/noid/.hushlogin

# Serial console
mkdir -p /etc/systemd/system/serial-getty@ttyS0.service.d
cat > /etc/systemd/system/serial-getty@ttyS0.service.d/override.conf << 'EOF'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin noid --noissue --keep-baud 115200,57600,38400,9600 ttyS0 $TERM
EOF
systemctl enable serial-getty@ttyS0.service

# Networking — kernel ip= boot param handles static config.
# systemd-networkd must be disabled: it takes ownership of eth0 and flushes
# the kernel-configured IP when no Address= is defined in .network files.
rm -rf /etc/systemd/network/
systemctl disable systemd-networkd 2>/dev/null || true
systemctl mask systemd-networkd.service systemd-networkd.socket 2>/dev/null || true
# Keep DNS static via /etc/resolv.conf for minimal images. resolved can fail
# in stripped environments and is not required for noid exec.
systemctl disable systemd-resolved 2>/dev/null || true
systemctl mask systemd-resolved.service 2>/dev/null || true

# Disable unnecessary services for fast boot
systemctl disable cron.service 2>/dev/null || true
systemctl disable rsyslog.service 2>/dev/null || true
systemctl mask getty@tty1.service
# timesyncd keeps the guest clock accurate (required for TLS cert validation).
# On snapshot restore the clock is stale; reconfigure_guest_network sets it
# from the host, and timesyncd keeps it drifting-free long-term.
systemctl unmask systemd-timesyncd.service 2>/dev/null || true
systemctl enable systemd-timesyncd.service 2>/dev/null || true
systemctl mask e2scrub_all.timer
systemctl mask fstrim.timer
systemctl mask logrotate.timer
systemctl mask motd-news.timer
rm -rf /etc/update-motd.d/*
: > /etc/legal
systemctl mask dpkg-db-backup.timer
systemctl mask console-setup.service
systemctl mask systemd-update-utmp.service
systemctl mask systemd-update-utmp-runlevel.service
systemctl mask systemd-journal-flush.service

# Disable bracketed paste mode — its ANSI escapes (\e[?2004h/l) pollute
# serial.log and can break exec marker parsing.
echo 'set enable-bracketed-paste off' >> /etc/inputrc

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

    # Wait for VM to be ready (poll with exec).
    # Detect kernel panic early via serial.log to avoid burning 60x30s retries.
    echo "    Waiting for VM to boot..."
    RETRIES=0
    MAX_RETRIES=60
    GOLDEN_VM_DIR=""
    while [ "$RETRIES" -lt "$MAX_RETRIES" ]; do
        # Re-discover VM dir on each iteration (directory may not exist yet on first pass)
        if [ -z "${GOLDEN_VM_DIR}" ]; then
            GOLDEN_VM_DIR=$(find "${NOID_DIR}/.noid/storage/users" -mindepth 3 -maxdepth 3 -type d -path "*/vms/_golden" 2>/dev/null | head -1)
        fi
        # Fail fast: check serial.log for kernel panic before burning a 30s exec timeout
        if [ -n "${GOLDEN_VM_DIR}" ] && [ -f "${GOLDEN_VM_DIR}/serial.log" ]; then
            if grep -q "Kernel panic" "${GOLDEN_VM_DIR}/serial.log" 2>/dev/null; then
                echo ""
                echo -e "    ${RED}Kernel panic detected in template VM:${NC}"
                head -5 "${GOLDEN_VM_DIR}/serial.log" | sed 's/^/    /'
                echo ""
                echo -e "    ${YELLOW}The kernel cannot boot. Check kernel config and rebuild.${NC}"
                break
            fi
        fi
        if sudo -u firecracker env PATH="${CARGO_BIN}:${PATH}" HOME="${NOID_DIR}" \
            noid exec --name _golden -- echo ready 2>/dev/null | grep -q ready; then
            break
        fi
        RETRIES=$((RETRIES + 1))
        sleep 2
    done
    if [ "$RETRIES" -ge "$MAX_RETRIES" ] || \
       { [ -n "${GOLDEN_VM_DIR}" ] && [ -f "${GOLDEN_VM_DIR}/serial.log" ] && grep -q "Kernel panic" "${GOLDEN_VM_DIR}/serial.log" 2>/dev/null; }; then
        echo ""
        echo -e "    ${RED}Template VM failed to boot${NC}"
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
{"cpus": 1, "mem_mib": 2048, "snapshot_rootfs_path": "${VM_DIR}/rootfs.ext4"}
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
# In-repo config (development convenience)
SERVER_TOML="${NOID_REPO}/server.toml"
cat > "$SERVER_TOML" << EOF
listen = "0.0.0.0:7654"
kernel = "${KERNEL_PATH}"
rootfs = "${ROOTFS_PATH}"
EOF
chown firecracker:firecracker "$SERVER_TOML"
echo "    ${SERVER_TOML}"

# System config (for systemd service)
mkdir -p /etc/noid
cp "$SERVER_TOML" /etc/noid/server.toml
chmod 644 /etc/noid/server.toml
echo "    /etc/noid/server.toml"

# --- Done: Start services ---

step "Starting services"
systemctl restart noid-netd
systemctl restart noid-server
echo "    noid-netd:    $(systemctl is-active noid-netd)"
echo "    noid-server:  $(systemctl is-active noid-server)"

echo ""
echo -e "${GREEN}=== noid installed ===${NC}"
echo ""
echo "  Binaries:     ${USER_BIN_DIR}/noid, noid-server, noid-netd"
echo "  Firecracker:  ${BIN_DIR}/firecracker (v${FC_VERSION})"
echo "  Kernel:       ${KERNEL_PATH}"
echo "  Rootfs:       ${ROOTFS_PATH}"
echo "  Config:       ${SERVER_TOML}, /etc/noid/server.toml"
echo "  Networking:   172.16.0.0/16 NAT via ${DEFAULT_IF}"
echo "  noid-netd:    $(systemctl is-active noid-netd)"
echo "  noid-server:  $(systemctl is-active noid-server)"
echo ""
echo "Services (survive reboot):"
echo "  systemctl status noid-server noid-netd"
echo "  journalctl -u noid-server -f"
echo ""
echo "Next steps:"
echo "  1. Add a user:        noid-server add-user alice"
echo "  2. Configure client:  noid auth setup --url http://localhost:7654 --token <token>"
echo "  3. Create a VM:       noid create myvm"
echo "  4. Optional: Install Claude Code in a VM, then checkpoint to update the golden snapshot"
