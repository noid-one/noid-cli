#!/usr/bin/env bash
#
# provision-golden.sh — Update the golden snapshot used by `noid create`
#
# Mode 1: Promote an existing checkpoint
#   You need a checkpoint first: noid checkpoint --name my-vm --label before-deploy
#   then:
#   sudo bash scripts/provision-golden.sh --from-checkpoint <checkpoint_id>
#
# Mode 2: Full provisioning (create VM, install tools, checkpoint, promote)
#   sudo bash scripts/provision-golden.sh
#
set -euo pipefail

# --- Colors ---
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# --- Helpers ---
info()  { echo -e "  ${GREEN}✓${NC} $*"; }
warn()  { echo -e "  ${YELLOW}⚠${NC} $*"; }
fail()  { echo -e "  ${RED}✗${NC} $*" >&2; exit 1; }
step()  { echo -e "\n${GREEN}→${NC} $*"; }

# --- Config ---
NOID_USER="${NOID_USER:-firecracker}"
NOID_HOME=$(eval echo "~${NOID_USER}")
NOID_DIR="${NOID_HOME}/.noid"
GOLDEN_DIR="${NOID_DIR}/golden"
DB_PATH="${NOID_DIR}/noid.db"

# --- Parse args ---
CHECKPOINT_ID=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --from-checkpoint)
            CHECKPOINT_ID="${2:-}"
            [[ -z "$CHECKPOINT_ID" ]] && fail "--from-checkpoint requires a checkpoint ID"
            shift 2
            ;;
        -h|--help)
            echo "Usage: sudo bash $0 [--from-checkpoint <id>]"
            echo ""
            echo "Modes:"
            echo "  --from-checkpoint <id>  Promote an existing checkpoint to golden"
            echo "  (no flags)              Create a VM, install tools, checkpoint, promote"
            exit 0
            ;;
        *)
            fail "Unknown argument: $1"
            ;;
    esac
done

# --- Must run as root (for file ownership) ---
if [[ $EUID -ne 0 ]]; then
    fail "This script must be run as root (sudo)"
fi

# --- Promote checkpoint to golden ---
promote_checkpoint() {
    local CKPT_ID="$1"

    step "Finding checkpoint ${CKPT_ID} on disk"
    CHECKPOINT_DIR=$(find "${NOID_DIR}/storage/users" -path "*/checkpoints/*/${CKPT_ID}" -type d 2>/dev/null | head -1)
    [[ -z "$CHECKPOINT_DIR" ]] && fail "Checkpoint ${CKPT_ID} not found under ${NOID_DIR}/storage/users/"
    info "Found: ${CHECKPOINT_DIR}"

    # Verify required files exist
    for f in rootfs.ext4 memory.snap vmstate.snap; do
        [[ -f "${CHECKPOINT_DIR}/${f}" ]] || fail "Missing ${f} in checkpoint directory"
    done
    info "All snapshot files present"

    # --- Get cpus/mem_mib from DB ---
    step "Reading VM config from database"
    CPUS=1
    MEM_MIB=2048
    ROOTFS_PATH=""

    if command -v sqlite3 &>/dev/null && [[ -f "$DB_PATH" ]]; then
        DB_ROW=$(sqlite3 "$DB_PATH" \
            "SELECT v.cpus, v.mem_mib, v.rootfs FROM checkpoints c JOIN vms v ON c.vm_name = v.name AND c.user_id = v.user_id WHERE c.id = '${CKPT_ID}';" 2>/dev/null || true)
        if [[ -n "$DB_ROW" ]]; then
            CPUS=$(echo "$DB_ROW" | cut -d'|' -f1)
            MEM_MIB=$(echo "$DB_ROW" | cut -d'|' -f2)
            ROOTFS_PATH=$(echo "$DB_ROW" | cut -d'|' -f3)
            info "From DB: cpus=${CPUS}, mem_mib=${MEM_MIB}"
        else
            warn "Checkpoint not found in DB (VM may have been destroyed), using defaults"
        fi
    else
        warn "sqlite3 not available or DB missing, using defaults: cpus=${CPUS}, mem_mib=${MEM_MIB}"
    fi

    # --- Extract snapshot_rootfs_path from vmstate ---
    step "Extracting snapshot_rootfs_path from vmstate"
    SNAPSHOT_ROOTFS_PATH=""
    if command -v strings &>/dev/null; then
        SNAPSHOT_ROOTFS_PATH=$(strings "${CHECKPOINT_DIR}/vmstate.snap" | grep -oE '/[^ ]+/rootfs\.ext4' | head -1 || true)
    fi
    if [[ -n "$SNAPSHOT_ROOTFS_PATH" ]]; then
        info "Extracted: ${SNAPSHOT_ROOTFS_PATH}"
    else
        # Fallback: use the rootfs path from DB, or construct from checkpoint dir
        if [[ -n "$ROOTFS_PATH" ]]; then
            SNAPSHOT_ROOTFS_PATH="$ROOTFS_PATH"
            warn "Could not extract from vmstate, using DB rootfs path: ${SNAPSHOT_ROOTFS_PATH}"
        else
            SNAPSHOT_ROOTFS_PATH="${CHECKPOINT_DIR}/rootfs.ext4"
            warn "Could not extract from vmstate, using checkpoint path: ${SNAPSHOT_ROOTFS_PATH}"
        fi
    fi

    # --- Backup existing golden ---
    step "Updating golden snapshot"
    if [[ -d "$GOLDEN_DIR" ]]; then
        BACKUP="${GOLDEN_DIR}.bak.$(date +%s)"
        mv "$GOLDEN_DIR" "$BACKUP"
        info "Backed up existing golden to ${BACKUP}"
    fi

    # --- Copy files ---
    mkdir -p "$GOLDEN_DIR"
    cp --reflink=auto "${CHECKPOINT_DIR}/rootfs.ext4" "${GOLDEN_DIR}/rootfs.ext4"
    info "Copied rootfs.ext4 (reflink)"
    cp "${CHECKPOINT_DIR}/memory.snap" "${GOLDEN_DIR}/memory.snap"
    info "Copied memory.snap"
    cp "${CHECKPOINT_DIR}/vmstate.snap" "${GOLDEN_DIR}/vmstate.snap"
    info "Copied vmstate.snap"

    # --- Write config.json ---
    cat > "${GOLDEN_DIR}/config.json" << EOF
{"cpus": ${CPUS}, "mem_mib": ${MEM_MIB}, "snapshot_rootfs_path": "${SNAPSHOT_ROOTFS_PATH}"}
EOF
    info "Wrote config.json"

    # --- Fix ownership ---
    chown -R "${NOID_USER}:${NOID_USER}" "$GOLDEN_DIR"

    step "Done"
    info "Golden snapshot at ${GOLDEN_DIR}"
    echo ""
    echo "  Files:"
    ls -lh "${GOLDEN_DIR}/" | tail -n +2 | sed 's/^/    /'
    echo ""
    echo "  Config:"
    echo "    $(cat "${GOLDEN_DIR}/config.json")"
    echo ""
}

# --- Full provisioning mode ---
provision_from_scratch() {
    step "Checking prerequisites"

    # Need noid CLI on PATH
    if ! command -v noid &>/dev/null; then
        # Try common locations
        for p in "${NOID_HOME}/.cargo/bin/noid" /usr/local/bin/noid "${NOID_HOME}/.local/bin/noid"; do
            if [[ -x "$p" ]]; then
                export PATH="$(dirname "$p"):${PATH}"
                break
            fi
        done
        command -v noid &>/dev/null || fail "noid CLI not found on PATH"
    fi
    info "noid CLI: $(which noid)"

    # Need server running
    local SERVER_URL
    SERVER_URL=$(sudo -u "$NOID_USER" noid auth show-url 2>/dev/null || true)
    if [[ -z "$SERVER_URL" ]]; then
        # Try reading from config.toml
        if [[ -f "${NOID_DIR}/config.toml" ]]; then
            SERVER_URL=$(grep -oP 'url\s*=\s*"\K[^"]+' "${NOID_DIR}/config.toml" || true)
        fi
    fi
    [[ -z "$SERVER_URL" ]] && fail "No server URL configured. Run: noid auth setup --url <url> --token <token>"

    # Check healthz
    if command -v curl &>/dev/null; then
        if ! curl -sf "${SERVER_URL}/healthz" &>/dev/null; then
            fail "Server not responding at ${SERVER_URL}/healthz"
        fi
    fi
    info "Server healthy at ${SERVER_URL}"

    # Check token exists
    if [[ -f "${NOID_DIR}/config.toml" ]]; then
        if ! grep -q 'token' "${NOID_DIR}/config.toml"; then
            fail "No token in ${NOID_DIR}/config.toml. Run: noid auth setup --url <url> --token <token>"
        fi
    else
        fail "No config at ${NOID_DIR}/config.toml. Run: noid auth setup --url <url> --token <token>"
    fi
    info "CLI token configured"

    # --- Create temp VM ---
    step "Creating temporary VM: _provision"
    sudo -u "$NOID_USER" noid create _provision
    info "VM created"

    # Cleanup trap
    provision_cleanup() {
        echo ""
        warn "Cleaning up temporary VM..."
        sudo -u "$NOID_USER" noid destroy --name _provision 2>/dev/null || true
    }
    trap provision_cleanup EXIT

    # --- Wait for boot ---
    step "Waiting for VM to boot"
    RETRIES=0
    MAX_RETRIES=60
    while [[ "$RETRIES" -lt "$MAX_RETRIES" ]]; do
        if sudo -u "$NOID_USER" noid exec --name _provision -- echo ready 2>/dev/null | grep -q ready; then
            break
        fi
        RETRIES=$((RETRIES + 1))
        sleep 2
    done
    [[ "$RETRIES" -ge "$MAX_RETRIES" ]] && fail "VM failed to boot after ${MAX_RETRIES} retries"
    info "VM ready"

    # --- Install Claude Code ---
    step "Installing Claude Code"
    sudo -u "$NOID_USER" noid exec --name _provision -- sh -c 'curl -fsSL https://claude.ai/install.sh | sh'
    info "Claude Code installed"

    # --- Install opencode ---
    step "Installing opencode"
    sudo -u "$NOID_USER" noid exec --name _provision -- sh -c 'curl -fsSL https://opencode.ai/install | sh'
    info "opencode installed"

    # --- Take checkpoint ---
    step "Taking checkpoint"
    CKPT_OUTPUT=$(sudo -u "$NOID_USER" noid checkpoint --name _provision --label golden-provisioned 2>&1)
    echo "    ${CKPT_OUTPUT}"

    # Extract checkpoint ID from output (first 16-char hex string)
    CKPT_ID=$(echo "$CKPT_OUTPUT" | grep -oE '[0-9a-f]{16}' | head -1)
    [[ -z "$CKPT_ID" ]] && fail "Could not extract checkpoint ID from output"
    info "Checkpoint: ${CKPT_ID}"

    # --- Promote to golden ---
    promote_checkpoint "$CKPT_ID"

    # --- Cleanup (trap will destroy VM) ---
    step "Destroying temporary VM"
    sudo -u "$NOID_USER" noid destroy --name _provision 2>/dev/null || true
    trap - EXIT
    info "Cleanup complete"
}

# --- Main ---
if [[ -n "$CHECKPOINT_ID" ]]; then
    echo "Promoting checkpoint ${CHECKPOINT_ID} to golden snapshot"
    promote_checkpoint "$CHECKPOINT_ID"
else
    echo "Full golden provisioning (create VM → install tools → checkpoint → promote)"
    provision_from_scratch
fi
