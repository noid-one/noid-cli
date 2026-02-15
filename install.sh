#!/bin/bash
# install.sh — Install noid client (and noid-server on Linux) from GitHub releases.
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

# --- Platform detection ---

case "$(uname -s)" in
    Linux)  OS_TAG="linux" ;;
    Darwin) OS_TAG="darwin" ;;
    *)      fail "Unsupported OS: $(uname -s). noid supports Linux and macOS." ;;
esac

MACHINE="$(uname -m)"
case "$MACHINE" in
    x86_64)         ARCH_TAG="x86_64" ;;
    aarch64|arm64)  ARCH_TAG="aarch64" ;;
    *)              fail "Unsupported architecture: $MACHINE. noid supports x86_64 and aarch64." ;;
esac

if ! command -v curl >/dev/null 2>&1; then
    fail "curl is required but not installed."
fi

# --- Install ---

BASE_URL="https://github.com/${REPO}/releases/latest/download"
CLIENT_ASSET="noid-${OS_TAG}-${ARCH_TAG}"

# Validate constructed asset name (defense-in-depth)
case "$CLIENT_ASSET" in
    noid-linux-x86_64|noid-linux-aarch64|noid-darwin-x86_64|noid-darwin-aarch64)
        ;;  # valid
    *)
        fail "Invalid asset name constructed: $CLIENT_ASSET (this is a bug)"
        ;;
esac

mkdir -p "$INSTALL_DIR"

info "Downloading noid (${OS_TAG}/${ARCH_TAG})..."
curl -fsSL -o "${INSTALL_DIR}/noid" "${BASE_URL}/${CLIENT_ASSET}"
chmod +x "${INSTALL_DIR}/noid"

if [ "$OS_TAG" = "linux" ]; then
    info "Downloading noid-server..."
    curl -fsSL -o "${INSTALL_DIR}/noid-server" "${BASE_URL}/noid-server"
    chmod +x "${INSTALL_DIR}/noid-server"
fi

# --- PATH check ---

if ! echo "$PATH" | tr ':' '\n' | grep -Fqx "$INSTALL_DIR"; then
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
if [ "$OS_TAG" = "linux" ]; then
    echo "  noid-server → ${INSTALL_DIR}/noid-server"
fi
echo ""
echo "  Next steps:"
echo "    noid --help              # see available commands"
echo "    noid auth setup          # connect to a server"
if [ "$OS_TAG" = "linux" ]; then
    echo "    noid-server --help       # run your own server"
fi
