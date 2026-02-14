#!/bin/bash
# test-e2e-tls.sh — End-to-end smoke test: create VM and verify HTTPS works immediately.
# Requires: noid-server running, noid CLI configured, noid-netd running.
# Usage: bash scripts/test-e2e-tls.sh
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

VM_NAME="_test-tls-$$"
TIMEOUT=15

cleanup() {
    echo "Cleaning up..."
    noid destroy --name "$VM_NAME" 2>/dev/null || true
}
trap cleanup EXIT

echo "=== E2E TLS Smoke Test ==="
echo ""

# Pre-check: noid-server reachable
if ! noid whoami > /dev/null 2>&1; then
    echo -e "${RED}FAIL${NC}: noid-server not reachable (run 'noid whoami' to check)"
    exit 1
fi

# Step 1: Create VM
echo "Creating VM '${VM_NAME}'..."
noid create "$VM_NAME"
echo -e "  ${GREEN}OK${NC} VM created"

# Step 2: Wait for VM to be ready
echo "Waiting for VM to boot..."
RETRIES=0
while [ "$RETRIES" -lt 30 ]; do
    if noid exec --name "$VM_NAME" -- echo ready 2>/dev/null | grep -q ready; then
        break
    fi
    RETRIES=$((RETRIES + 1))
    sleep 1
done
if [ "$RETRIES" -ge 30 ]; then
    echo -e "${RED}FAIL${NC}: VM did not boot within 30 seconds"
    exit 1
fi
echo -e "  ${GREEN}OK${NC} VM ready"

# Step 3: Test HTTPS immediately (the critical test)
echo "Testing HTTPS (${TIMEOUT}s timeout)..."
START=$(date +%s)
HTTP_CODE=$(noid exec --name "$VM_NAME" -- \
    curl -sS -o /dev/null -w '%{http_code}' --connect-timeout "$TIMEOUT" --max-time "$TIMEOUT" \
    https://example.com 2>/dev/null || echo "TIMEOUT")
END=$(date +%s)
ELAPSED=$((END - START))

if [ "$HTTP_CODE" = "TIMEOUT" ] || [ "$HTTP_CODE" = "" ]; then
    echo -e "${RED}FAIL${NC}: HTTPS request timed out after ${ELAPSED}s"
    echo ""
    echo "This likely means:"
    echo "  - Stale kernel without virtio-rng (CRNG blocks for minutes)"
    echo "  - Golden snapshot taken before CRNG was initialized"
    echo "  - Missing MSS clamping iptables rule"
    echo ""
    echo "Fix: sudo bash scripts/install-server.sh"
    exit 1
fi

if [ "$HTTP_CODE" -ge 200 ] && [ "$HTTP_CODE" -lt 400 ]; then
    echo -e "  ${GREEN}PASS${NC} HTTPS returned ${HTTP_CODE} in ${ELAPSED}s"
else
    echo -e "${YELLOW}WARN${NC}: HTTPS returned ${HTTP_CODE} (non-2xx/3xx) in ${ELAPSED}s"
    echo "  TLS handshake succeeded (no timeout), but server returned an error."
    echo "  This is acceptable — the test verifies TLS works, not the remote server."
fi

# Step 4: Verify entropy source exists
echo "Checking /dev/urandom entropy..."
ENTROPY_CHECK=$(noid exec --name "$VM_NAME" -- cat /proc/sys/kernel/random/entropy_avail 2>/dev/null || echo "0")
# Trim whitespace
ENTROPY_CHECK=$(echo "$ENTROPY_CHECK" | tr -d '[:space:]')
if [ -n "$ENTROPY_CHECK" ] && [ "$ENTROPY_CHECK" -gt 0 ] 2>/dev/null; then
    echo -e "  ${GREEN}OK${NC} entropy_avail=${ENTROPY_CHECK}"
else
    echo -e "${YELLOW}WARN${NC}: could not read entropy_avail (got: '${ENTROPY_CHECK}')"
fi

echo ""
echo -e "${GREEN}=== TLS smoke test passed ===${NC}"
