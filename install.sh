#!/bin/bash
# install.sh — Install noid and noid-server binaries from GitHub releases.
# Usage: curl -fsSL https://raw.githubusercontent.com/noid-one/noid-cli/master/install.sh | bash
set -euo pipefail

REPO="noid-one/noid-cli"
INSTALL_DIR="$HOME/.local/bin"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info() { echo -e "${GREEN}==>${NC} $1"; }
warn() { echo -e "${YELLOW}warning:${NC} $1"; }
fail() { echo -e "${RED}error:${NC} $1"; exit 1; }

# --- Pre-checks ---

OS="$(uname -s)"
ARCH="$(uname -m)"

if [ "$OS" != "Linux" ]; then
    fail "noid requires Linux (got $OS). Firecracker only runs on Linux."
fi

if [ "$ARCH" != "x86_64" ]; then
    fail "noid requires x86_64 (got $ARCH). Firecracker only supports x86_64."
fi

if ! command -v curl >/dev/null 2>&1; then
    fail "curl is required but not installed."
fi

# --- Install ---

BASE_URL="https://github.com/${REPO}/releases/latest/download"

mkdir -p "$INSTALL_DIR"

info "Downloading noid..."
curl -fsSL -o "${INSTALL_DIR}/noid" "${BASE_URL}/noid"
chmod +x "${INSTALL_DIR}/noid"

info "Downloading noid-server..."
curl -fsSL -o "${INSTALL_DIR}/noid-server" "${BASE_URL}/noid-server"
chmod +x "${INSTALL_DIR}/noid-server"

# --- PATH check ---

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
    warn "$INSTALL_DIR is not in your PATH."
    echo ""
    echo "  Add it by appending this to your shell profile (~/.bashrc or ~/.zshrc):"
    echo ""
    echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
    echo ""
fi

# --- Done ---

echo ""
info "Installed successfully!"
echo "  noid        → ${INSTALL_DIR}/noid"
echo "  noid-server → ${INSTALL_DIR}/noid-server"
echo ""
echo "  Next steps:"
echo "    noid --help              # see available commands"
echo "    noid auth setup          # connect to a server"
echo "    noid-server --help       # run your own server"
