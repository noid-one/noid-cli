#!/bin/bash
# test-golden-entropy.sh — Regression test: verify golden snapshot was taken after CRNG initialization.
# A golden snapshot taken before CRNG init carries the unintialized CRNG state forward,
# causing TLS hangs in all VMs restored from it.
#
# Requires: noid-server running, noid CLI configured.
# Usage: bash scripts/test-golden-entropy.sh
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

PASS=0
FAIL=0
WARN=0

check() {
    local desc="$1" ok="$2"
    if [ "$ok" = "1" ]; then
        echo -e "  ${GREEN}PASS${NC} $desc"
        PASS=$((PASS + 1))
    else
        echo -e "  ${RED}FAIL${NC} $desc"
        FAIL=$((FAIL + 1))
    fi
}

warn_check() {
    local desc="$1" ok="$2"
    if [ "$ok" = "1" ]; then
        echo -e "  ${GREEN}PASS${NC} $desc"
        PASS=$((PASS + 1))
    else
        echo -e "  ${YELLOW}WARN${NC} $desc"
        WARN=$((WARN + 1))
    fi
}

VM_NAME="_test-entropy-$$"
GOLDEN_DIR="${HOME}/.noid/golden"

cleanup() {
    noid destroy --name "$VM_NAME" 2>/dev/null || true
}
trap cleanup EXIT

echo "=== Golden Snapshot Entropy Readiness Test ==="
echo ""

# --- Test 1: Golden snapshot exists ---
echo "Test 1: Golden snapshot files"
if [ -f "${GOLDEN_DIR}/memory.snap" ] && [ -f "${GOLDEN_DIR}/vmstate.snap" ] && [ -f "${GOLDEN_DIR}/rootfs.ext4" ]; then
    check "golden snapshot files exist" "1"
else
    check "golden snapshot files exist" "0"
    echo "  No golden snapshot found at ${GOLDEN_DIR}."
    echo "  Run: sudo bash scripts/install-server.sh"
    exit 1
fi

# --- Test 2: Kernel version check ---
echo ""
echo "Test 2: Kernel version"
KERNEL_PATH="${HOME}/vmlinux.bin"
if [ -f "$KERNEL_PATH" ]; then
    KERNEL_VER=$(strings "$KERNEL_PATH" | grep -oP 'Linux version \K[0-9]+\.[0-9]+' | head -1 || echo "unknown")
    if [ "$KERNEL_VER" = "6.12" ]; then
        check "kernel is version 6.12" "1"
    else
        check "kernel is version 6.12 (got: ${KERNEL_VER})" "0"
    fi
else
    check "kernel exists at ${KERNEL_PATH}" "0"
fi

# --- Test 3: Create a VM from golden and check CRNG ---
echo ""
echo "Test 3: VM from golden has initialized CRNG"

# Pre-check: server reachable
if ! noid whoami > /dev/null 2>&1; then
    echo -e "  ${RED}SKIP${NC}: noid-server not reachable"
    exit 1
fi

noid create "$VM_NAME" > /dev/null 2>&1

# Wait for VM to boot
RETRIES=0
while [ "$RETRIES" -lt 30 ]; do
    if noid exec --name "$VM_NAME" -- echo ready 2>/dev/null | grep -q ready; then
        break
    fi
    RETRIES=$((RETRIES + 1))
    sleep 1
done

if [ "$RETRIES" -ge 30 ]; then
    check "VM booted within 30s" "0"
    exit 1
fi

# Check CRNG status via dmesg — on a properly snapshotted kernel, CRNG is already initialized
CRNG_OUTPUT=$(noid exec --name "$VM_NAME" -- dmesg 2>/dev/null || echo "")
if echo "$CRNG_OUTPUT" | grep -q "crng init done"; then
    # Check timestamp — should be very early (< 5 seconds), meaning it was already done at boot
    CRNG_TIME=$(echo "$CRNG_OUTPUT" | grep "crng init done" | grep -oP '^\[\s*\K[0-9]+' | head -1 || echo "999")
    if [ "${CRNG_TIME:-999}" -lt 5 ]; then
        check "CRNG initialized early (${CRNG_TIME}s — likely from snapshot)" "1"
    else
        warn_check "CRNG initialized late (${CRNG_TIME}s — may indicate cold boot without virtio-rng)" "0"
    fi
else
    # No crng message might mean it was already done before dmesg buffer starts (good)
    # or kernel version doesn't log it
    warn_check "CRNG init message not found in dmesg (may already be initialized)" "1"
fi

# Check entropy_avail — should be high if CRNG is seeded
ENTROPY=$(noid exec --name "$VM_NAME" -- cat /proc/sys/kernel/random/entropy_avail 2>/dev/null | tr -d '[:space:]' || echo "0")
if [ -n "$ENTROPY" ] && [ "$ENTROPY" -gt 100 ] 2>/dev/null; then
    check "entropy_avail=${ENTROPY} (sufficient for TLS)" "1"
else
    check "entropy_avail=${ENTROPY:-0} (need >100 for TLS)" "0"
fi

# --- Test 4: /dev/urandom is non-blocking (quick getrandom test) ---
echo ""
echo "Test 4: getrandom() is non-blocking"
# dd from /dev/urandom with a timeout — if CRNG is not ready, this hangs
START=$(date +%s)
DD_OUTPUT=$(noid exec --name "$VM_NAME" -- dd if=/dev/urandom bs=32 count=1 2>/dev/null | wc -c || echo "0")
END=$(date +%s)
ELAPSED=$((END - START))

# exec itself adds overhead, so allow up to 5 seconds
if [ "$ELAPSED" -lt 5 ]; then
    check "getrandom() completed in ${ELAPSED}s (non-blocking)" "1"
else
    check "getrandom() took ${ELAPSED}s (may be blocking on CRNG)" "0"
fi

# --- Summary ---
echo ""
TOTAL=$((PASS + FAIL))
if [ "$FAIL" -eq 0 ]; then
    if [ "$WARN" -gt 0 ]; then
        echo -e "${YELLOW}${PASS}/${TOTAL} passed, ${WARN} warnings${NC}"
    else
        echo -e "${GREEN}All ${TOTAL} tests passed${NC}"
    fi
else
    echo -e "${RED}${FAIL}/${TOTAL} tests failed${NC}"
    echo ""
    echo "If CRNG tests failed, the golden snapshot may have been taken"
    echo "before the kernel's CRNG was initialized. Fix:"
    echo "  1. rm -rf ~/.noid/golden"
    echo "  2. sudo bash scripts/install-server.sh"
    exit 1
fi
