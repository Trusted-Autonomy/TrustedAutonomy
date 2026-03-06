#!/usr/bin/env bash
# install_local.sh — Build TA from source and add it to your PATH.
#
# Usage:
#   ./install_local.sh              # Build ta + ta-daemon (release) and install
#   ./install_local.sh --debug      # Build debug binaries (faster compile)
#   ./install_local.sh --no-daemon  # Build only the ta CLI, skip ta-daemon
#
# After running, either:
#   1. Restart your shell, or
#   2. Run: export PATH="$HOME/.local/bin:$PATH"

set -euo pipefail

INSTALL_DIR="${HOME}/.local/bin"
PROFILE="${CARGO_BUILD_PROFILE:-release}"
BUILD_DAEMON=true

# Parse arguments.
for arg in "$@"; do
    case "$arg" in
        --debug)     PROFILE="dev" ;;
        --no-daemon) BUILD_DAEMON=false ;;
        *)           echo "Unknown option: $arg"; exit 1 ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Build target list.
BUILD_PACKAGES="-p ta-cli"
if [[ "$BUILD_DAEMON" == true ]]; then
    BUILD_PACKAGES="-p ta-cli -p ta-daemon"
fi

echo "Building${BUILD_DAEMON:+ ta-cli + ta-daemon} (profile: ${PROFILE})..."

# Detect build environment: Nix devShell or system Rust.
run_cargo() {
    if [[ "$PROFILE" == "dev" ]]; then
        cargo build $BUILD_PACKAGES
    else
        cargo build --release $BUILD_PACKAGES
    fi
}

if command -v nix &>/dev/null && [[ -f flake.nix ]]; then
    echo "  Using Nix devShell..."
    export PATH="/nix/var/nix/profiles/default/bin:$HOME/.nix-profile/bin:$PATH"
    nix develop --command bash -c "$(declare -f run_cargo); BUILD_PACKAGES='$BUILD_PACKAGES' PROFILE='$PROFILE' run_cargo"
elif command -v cargo &>/dev/null; then
    echo "  Using system Rust toolchain..."
    run_cargo
else
    echo "Error: Neither Nix nor Cargo found. Install Rust or Nix first."
    echo "  Rust: https://rustup.rs"
    echo "  Nix:  https://nixos.org/download"
    exit 1
fi

# Determine binary paths based on profile.
if [[ "$PROFILE" == "dev" ]]; then
    TARGET_DIR="target/debug"
else
    TARGET_DIR="target/release"
fi

TA_BINARY="${TARGET_DIR}/ta"
DAEMON_BINARY="${TARGET_DIR}/ta-daemon"

if [[ ! -f "$TA_BINARY" ]]; then
    echo "Error: Build succeeded but ta binary not found at $TA_BINARY"
    exit 1
fi

# Install to ~/.local/bin.
mkdir -p "$INSTALL_DIR"

cp "$TA_BINARY" "$INSTALL_DIR/ta"
chmod +x "$INSTALL_DIR/ta"
echo "Installed: $INSTALL_DIR/ta"
"$INSTALL_DIR/ta" --version

if [[ "$BUILD_DAEMON" == true ]]; then
    if [[ ! -f "$DAEMON_BINARY" ]]; then
        echo "Error: Build succeeded but ta-daemon binary not found at $DAEMON_BINARY"
        exit 1
    fi
    cp "$DAEMON_BINARY" "$INSTALL_DIR/ta-daemon"
    chmod +x "$INSTALL_DIR/ta-daemon"
    echo "Installed: $INSTALL_DIR/ta-daemon"
fi

echo ""

# Check if ~/.local/bin is in PATH.
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
    echo "Add to your PATH by adding this to your shell profile:"
    echo ""

    # Detect shell and suggest the right file.
    SHELL_NAME="$(basename "${SHELL:-bash}")"
    case "$SHELL_NAME" in
        zsh)  PROFILE_FILE="~/.zshrc" ;;
        bash) PROFILE_FILE="~/.bashrc" ;;
        fish) PROFILE_FILE="~/.config/fish/config.fish" ;;
        *)    PROFILE_FILE="~/.profile" ;;
    esac

    if [[ "$SHELL_NAME" == "fish" ]]; then
        echo "  fish_add_path $INSTALL_DIR"
    else
        echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
    fi
    echo ""
    echo "  (add to $PROFILE_FILE for persistence)"
    echo ""
    echo "Or for this session only:"
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
else
    echo "~/.local/bin is already in your PATH. You're all set."
fi

echo ""
echo "Quick start:"
echo "  ta shell    # interactive shell (starts daemon automatically)"
echo "  ta dev      # developer loop"
echo "  ta --help   # all commands"
