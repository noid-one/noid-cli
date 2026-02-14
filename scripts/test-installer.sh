#!/bin/bash
# test-installer.sh — Tests the kernel version validation logic from install-server.sh.
# Usage: bash scripts/test-installer.sh
# Does NOT require root. Does NOT download anything. Uses temp files.
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

PASS=0
FAIL=0

assert_eq() {
    local desc="$1" expected="$2" actual="$3"
    if [ "$expected" = "$actual" ]; then
        echo -e "  ${GREEN}PASS${NC} $desc"
        PASS=$((PASS + 1))
    else
        echo -e "  ${RED}FAIL${NC} $desc: expected='$expected' actual='$actual'"
        FAIL=$((FAIL + 1))
    fi
}

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

KERNEL_FULL_VERSION="6.12.71"

# --- Helper: simulates the version check logic from install-server.sh ---
check_kernel() {
    local kernel_path="$1"
    local need_kernel=0
    if [ -f "$kernel_path" ]; then
        local current_version
        current_version=$(strings "$kernel_path" | grep -oP 'Linux version \K[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "")
        if [ -z "$current_version" ]; then
            need_kernel=1
        elif [ "$current_version" = "$KERNEL_FULL_VERSION" ]; then
            need_kernel=0
        else
            need_kernel=1
        fi
    else
        need_kernel=1
    fi
    echo "$need_kernel"
}

echo "=== Installer kernel version check tests ==="

# Test 1: No kernel file → needs kernel
echo ""
echo "Test 1: Missing kernel file"
result=$(check_kernel "$TMPDIR/nonexistent")
assert_eq "missing file triggers rebuild" "1" "$result"

# Test 2: Kernel with correct version string → skip
echo ""
echo "Test 2: Kernel with correct version (6.12.71)"
printf 'PADDING\0Linux version 6.12.71 (gcc)\0MORE' > "$TMPDIR/good_kernel"
result=$(check_kernel "$TMPDIR/good_kernel")
assert_eq "correct version skips rebuild" "0" "$result"

# Test 3: Kernel with stale 4.14 version → needs replacement
echo ""
echo "Test 3: Kernel with stale version (4.14)"
printf 'PADDING\0Linux version 4.14.174 (gcc)\0MORE' > "$TMPDIR/stale_kernel"
result=$(check_kernel "$TMPDIR/stale_kernel")
assert_eq "stale 4.14 kernel triggers rebuild" "1" "$result"

# Test 4: Kernel with different 6.x version → needs replacement
echo ""
echo "Test 4: Kernel with different version (6.1)"
printf 'PADDING\0Linux version 6.1.102 (gcc)\0MORE' > "$TMPDIR/old_6_kernel"
result=$(check_kernel "$TMPDIR/old_6_kernel")
assert_eq "old 6.1 kernel triggers rebuild" "1" "$result"

# Test 5: Empty file (no version string) → needs replacement
echo ""
echo "Test 5: Empty/corrupt kernel file"
echo "not a real kernel" > "$TMPDIR/empty_kernel"
result=$(check_kernel "$TMPDIR/empty_kernel")
assert_eq "corrupt kernel triggers rebuild" "1" "$result"

# Test 6: Kernel with matching major.minor but different patch → needs rebuild
echo ""
echo "Test 6: Same major.minor, different patch (6.12.99)"
printf 'PADDING\0Linux version 6.12.99 (gcc)\0MORE' > "$TMPDIR/patch_kernel"
result=$(check_kernel "$TMPDIR/patch_kernel")
assert_eq "different patch version triggers rebuild" "1" "$result"

# --- Summary ---
echo ""
TOTAL=$((PASS + FAIL))
if [ "$FAIL" -eq 0 ]; then
    echo -e "${GREEN}All $TOTAL tests passed${NC}"
else
    echo -e "${RED}$FAIL/$TOTAL tests failed${NC}"
    exit 1
fi
