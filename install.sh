#!/bin/bash
# Trusted Autonomy installer script
# Usage: curl -fsSL https://raw.githubusercontent.com/trustedautonomy/ta/main/install.sh | sh

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

REPO="trustedautonomy/ta"
BINARY_NAME="ta"
DAEMON_NAME="ta-daemon"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
INSTALL_DAEMON=true

# Parse arguments.
for arg in "$@"; do
    case "$arg" in
        --no-daemon) INSTALL_DAEMON=false ;;
        --help)
            echo "Usage: install.sh [--no-daemon]"
            echo "  --no-daemon  Skip installing the ta-daemon binary"
            exit 0
            ;;
    esac
done

# Detect OS and architecture
detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Linux*)
            OS_TYPE="linux"
            ;;
        Darwin*)
            OS_TYPE="darwin"
            ;;
        MINGW*|MSYS*|CYGWIN*)
            echo -e "${YELLOW}Windows detected. For native Windows, use:${NC}"
            echo "  winget install trustedautonomy.ta"
            echo "  scoop install ta"
            echo ""
            echo "Or use WSL2 and re-run this script inside Linux."
            exit 1
            ;;
        *)
            echo -e "${RED}Error: Unsupported operating system: $OS${NC}"
            exit 1
            ;;
    esac

    case "$ARCH" in
        x86_64)
            ARCH_TYPE="x86_64"
            ;;
        arm64|aarch64)
            ARCH_TYPE="aarch64"
            ;;
        *)
            echo -e "${RED}Error: Unsupported architecture: $ARCH${NC}"
            exit 1
            ;;
    esac

    # Construct target triple
    if [ "$OS_TYPE" = "linux" ]; then
        TARGET="${ARCH_TYPE}-unknown-linux-musl"
    else
        TARGET="${ARCH_TYPE}-apple-darwin"
    fi

    echo -e "${GREEN}Detected platform:${NC} $OS_TYPE $ARCH_TYPE"
    echo -e "${GREEN}Target:${NC} $TARGET"
}

# Get latest release version
get_latest_version() {
    echo -e "${GREEN}Fetching latest release...${NC}"

    # Try to get latest release from GitHub API
    if command -v curl > /dev/null; then
        VERSION=$(curl -s "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')
    else
        echo -e "${RED}Error: curl is required but not installed${NC}"
        exit 1
    fi

    if [ -z "$VERSION" ]; then
        echo -e "${RED}Error: Could not determine latest version${NC}"
        exit 1
    fi

    echo -e "${GREEN}Latest version:${NC} $VERSION"
}

# Download, verify, and install a single binary.
# Usage: download_and_install <name> <required>
download_and_install() {
    local name="$1"
    local required="$2"

    local download_url="https://github.com/$REPO/releases/download/$VERSION/${name}-${VERSION}-${TARGET}.tar.gz"
    local checksum_url="${download_url}.sha256"

    echo -e "${GREEN}Downloading ${name} from:${NC} $download_url"

    # Create temporary directory
    local tmp_dir
    tmp_dir=$(mktemp -d)

    # Download archive
    if ! curl -fsSL "$download_url" -o "$tmp_dir/${name}.tar.gz"; then
        if [[ "$required" == "true" ]]; then
            echo -e "${RED}Error: Failed to download ${name}${NC}"
            rm -rf "$tmp_dir"
            exit 1
        else
            echo -e "${YELLOW}Warning: ${name} not available in this release, skipping${NC}"
            rm -rf "$tmp_dir"
            return 0
        fi
    fi

    # Download and verify checksum
    if curl -fsSL "$checksum_url" -o "$tmp_dir/${name}.tar.gz.sha256" 2>/dev/null; then
        echo -e "${GREEN}Verifying checksum...${NC}"
        cd "$tmp_dir"
        if command -v sha256sum > /dev/null; then
            sha256sum -c "${name}.tar.gz.sha256" 2>/dev/null || {
                echo -e "${RED}Error: Checksum verification failed for ${name}${NC}"
                cd - > /dev/null; rm -rf "$tmp_dir"; exit 1
            }
        elif command -v shasum > /dev/null; then
            shasum -a 256 -c "${name}.tar.gz.sha256" 2>/dev/null || {
                echo -e "${RED}Error: Checksum verification failed for ${name}${NC}"
                cd - > /dev/null; rm -rf "$tmp_dir"; exit 1
            }
        fi
        cd - > /dev/null
        echo -e "${GREEN}✓ Checksum verified${NC}"
    else
        echo -e "${YELLOW}Warning: No checksum for ${name}, skipping verification${NC}"
    fi

    # Extract and install
    tar xzf "$tmp_dir/${name}.tar.gz" -C "$tmp_dir"
    mkdir -p "$INSTALL_DIR"
    mv "$tmp_dir/${name}" "$INSTALL_DIR/${name}"
    chmod +x "$INSTALL_DIR/${name}"
    rm -rf "$tmp_dir"

    echo -e "${GREEN}✓ Installed ${name}${NC}"
}

# Download USAGE.html from the release and install to ~/.local/share/ta/.
install_docs() {
    local docs_url="https://github.com/$REPO/releases/download/$VERSION/USAGE.html"
    local docs_dir="$HOME/.local/share/ta"
    mkdir -p "$docs_dir"
    if curl -fsSL "$docs_url" -o "$docs_dir/USAGE.html" 2>/dev/null; then
        echo -e "${GREEN}✓ Installed USAGE.html${NC} → $docs_dir/USAGE.html"
    else
        echo -e "${YELLOW}Note: USAGE.html not bundled in this release — skipping${NC}"
    fi
}

# Download and install binaries
install_binary() {
    download_and_install "$BINARY_NAME" "true"

    if [[ "$INSTALL_DAEMON" == true ]]; then
        download_and_install "$DAEMON_NAME" "false"
    fi

    install_docs

    echo -e "${GREEN}✓ Installation complete!${NC}"
}

# Verify installation
verify_installation() {
    if [ -x "$INSTALL_DIR/$BINARY_NAME" ]; then
        VERSION_OUTPUT=$("$INSTALL_DIR/$BINARY_NAME" --version 2>&1 || true)
        echo -e "${GREEN}Verification:${NC}"
        echo "  $VERSION_OUTPUT"
    else
        echo -e "${RED}Warning: Binary was installed but is not executable${NC}"
        exit 1
    fi
}

# Check if install directory is in PATH
check_path() {
    case ":$PATH:" in
        *":$INSTALL_DIR:"*)
            echo -e "${GREEN}✓ $INSTALL_DIR is in your PATH${NC}"
            ;;
        *)
            echo -e "${YELLOW}Warning: $INSTALL_DIR is not in your PATH${NC}"
            echo -e "${YELLOW}Add the following to your shell profile (~/.bashrc, ~/.zshrc, etc.):${NC}"
            echo ""
            echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
            echo ""
            ;;
    esac
}

# Print post-install instructions
print_instructions() {
    echo ""
    echo -e "${GREEN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${GREEN}Getting Started with Trusted Autonomy${NC}"
    echo -e "${GREEN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo ""
    echo "1. Initialize TA in your project:"
    echo "   cd your-project && ${BINARY_NAME} init from-existing"
    echo ""
    echo "2. Launch the interactive shell (starts daemon automatically):"
    echo "   ${BINARY_NAME} shell"
    echo ""
    echo "3. Or start the developer loop:"
    echo "   ${BINARY_NAME} dev"
    echo ""
    echo "4. Or run a single mediated goal:"
    echo "   ${BINARY_NAME} run \"Fix the auth bug\""
    echo "   ${BINARY_NAME} draft view <id>"
    echo "   ${BINARY_NAME} draft approve <id>"
    echo "   ${BINARY_NAME} draft apply <id> --git-commit"
    echo ""
    echo "For help: ${BINARY_NAME} --help"
    echo "Documentation: https://github.com/$REPO"
    echo "Usage guide:   $HOME/.local/share/ta/USAGE.html"
    echo ""
}

# Main execution
run_onboarding() {
    # Only run the onboarding wizard if stdin is an interactive terminal.
    if [ -t 0 ] && [ -t 1 ]; then
        echo ""
        echo -e "${GREEN}Running first-time setup wizard...${NC}"
        echo ""
        "${INSTALL_DIR}/${BINARY_NAME}" onboard || true
    else
        # Non-interactive install (piped or CI) — print the first-run hint.
        echo ""
        echo -e "${YELLOW}TA is not configured yet.${NC}"
        echo "Run 'ta onboard' in an interactive terminal to set up your AI provider"
        echo "and defaults (takes ~2 minutes)."
    fi
}

main() {
    echo -e "${GREEN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${GREEN}Trusted Autonomy Installer${NC}"
    echo -e "${GREEN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo ""

    detect_platform
    get_latest_version
    install_binary
    verify_installation
    check_path
    print_instructions
    run_onboarding
}

main
