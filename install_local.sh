#!/usr/bin/env bash
# install_local.sh — Build TA from source and add it to your PATH.
#
# Usage:
#   ./install_local.sh              # Build ta + ta-daemon + channel plugins and install
#   ./install_local.sh --debug      # Build debug binaries (faster compile)
#   ./install_local.sh --no-daemon  # Build only the ta CLI, skip ta-daemon and plugins
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

# Auto-clean target/ if it exceeds 150GB to prevent disk exhaustion.
# The build that follows will repopulate only what is needed.
_AUTO_CLEAN_GB=100
if [[ -d target ]]; then
    _target_kb=$(du -sk target 2>/dev/null | cut -f1 || echo 0)
    _target_gb=$(( _target_kb / 1048576 ))
    if (( _target_gb >= _AUTO_CLEAN_GB )); then
        echo "NOTE: target/ is ${_target_gb}GB (>= ${_AUTO_CLEAN_GB}GB threshold) — cleaning before build..."
        rm -rf target
        echo "  target/ removed. Build will recreate only what is needed."
    fi
fi

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

# Use `install` instead of `cp` to create a fresh inode. On macOS,
# syspolicyd caches provenance decisions per-inode — `cp` overwrites
# can inherit a stale "kill" decision, causing SIGKILL on launch.
install -m 755 "$TA_BINARY" "$INSTALL_DIR/ta"
echo "Installed: $INSTALL_DIR/ta"
"$INSTALL_DIR/ta" --version

if [[ "$BUILD_DAEMON" == true ]]; then
    if [[ ! -f "$DAEMON_BINARY" ]]; then
        echo "Error: Build succeeded but ta-daemon binary not found at $DAEMON_BINARY"
        exit 1
    fi
    install -m 755 "$DAEMON_BINARY" "$INSTALL_DIR/ta-daemon"
    echo "Installed: $INSTALL_DIR/ta-daemon"

    # Build and install channel plugins so the plugin binary version always
    # matches the installed daemon. Version skew between the main binary and a
    # stale plugin was the root cause of the Discord notification flood regression
    # (v0.12.8 / Bug 1 hardening item 2).
    PLUGIN_INSTALL_DIR="${HOME}/.local/share/ta/plugins/channels"

    # Discord channel plugin.
    DISCORD_PLUGIN_DIR="${SCRIPT_DIR}/plugins/ta-channel-discord"
    if [[ -d "$DISCORD_PLUGIN_DIR" ]]; then
        echo "Building channel plugin: ta-channel-discord..."
        build_discord_plugin() {
            if [[ "$PROFILE" == "dev" ]]; then
                cargo build --manifest-path "$DISCORD_PLUGIN_DIR/Cargo.toml"
            else
                cargo build --release --manifest-path "$DISCORD_PLUGIN_DIR/Cargo.toml"
            fi
        }
        if command -v nix &>/dev/null && [[ -f "${SCRIPT_DIR}/flake.nix" ]]; then
            nix develop "${SCRIPT_DIR}" --command bash -c \
                "$(declare -f build_discord_plugin); PROFILE='$PROFILE' DISCORD_PLUGIN_DIR='$DISCORD_PLUGIN_DIR' build_discord_plugin"
        else
            build_discord_plugin
        fi

        DISCORD_BINARY="${DISCORD_PLUGIN_DIR}/${TARGET_DIR}/ta-channel-discord"
        if [[ -f "$DISCORD_BINARY" ]]; then
            mkdir -p "${PLUGIN_INSTALL_DIR}/discord"
            install -m 755 "$DISCORD_BINARY" "${PLUGIN_INSTALL_DIR}/discord/ta-channel-discord"
            echo "Installed: ${PLUGIN_INSTALL_DIR}/discord/ta-channel-discord"
        else
            echo "Warning: Discord plugin build succeeded but binary not found at $DISCORD_BINARY"
        fi
    else
        echo "Note: Discord plugin source not found at $DISCORD_PLUGIN_DIR — skipping plugin build."
    fi
fi


# Install USAGE.html — generate locally with pandoc, or download from latest release.
install_docs() {
    local docs_dir="$HOME/.local/share/ta"
    mkdir -p "$docs_dir"

    if command -v pandoc &>/dev/null; then
        echo "Generating USAGE.html with pandoc..."
        pandoc "$SCRIPT_DIR/docs/USAGE.md" \
            -s \
            --metadata title="Trusted Autonomy Usage Guide" \
            -c https://cdn.simplecss.org/simple.min.css \
            -o "$docs_dir/USAGE.html"
        echo "Installed: $docs_dir/USAGE.html"
    else
        # Pandoc not available — try to download from latest GitHub release.
        local repo="Trusted-Autonomy/TrustedAutonomy"
        echo "pandoc not found — attempting to download USAGE.html from latest release..."
        local latest_tag
        latest_tag=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" 2>/dev/null \
            | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')
        if [ -n "$latest_tag" ]; then
            local url="https://github.com/$repo/releases/download/$latest_tag/USAGE.html"
            if curl -fsSL "$url" -o "$docs_dir/USAGE.html" 2>/dev/null; then
                echo "Installed: $docs_dir/USAGE.html  (from release $latest_tag)"
            else
                echo "Note: Could not download USAGE.html — install pandoc to generate it locally."
                echo "  https://pandoc.org/installing.html"
            fi
        else
            echo "Note: pandoc not installed and GitHub release not reachable."
            echo "  Install pandoc to generate USAGE.html: https://pandoc.org/installing.html"
        fi
    fi
}

install_docs

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
echo ""
echo "Usage guide: $HOME/.local/share/ta/USAGE.html"
