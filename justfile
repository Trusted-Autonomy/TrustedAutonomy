# Default recipe: run all checks then tests
default: check test

# Build all crates in the workspace
build:
    cargo build --workspace

# Run all tests
test:
    cargo nextest run --workspace 2>/dev/null || cargo test --workspace

# Run tests with standard cargo test (needed for doc tests, which nextest doesn't support)
test-doc:
    cargo test --workspace --doc

# Check formatting + linting (fails on any warning)
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

# Auto-format all code
fmt:
    cargo fmt --all

# Run a specific crate's tests (usage: just test-crate ta-audit)
test-crate CRATE:
    cargo nextest run -p {{CRATE}} 2>/dev/null || cargo test -p {{CRATE}}

# Verify everything before committing (format, lint, build, test)
verify: check build test

# Build and launch the TA shell (starts daemon automatically)
shell *ARGS:
    cargo build --bin ta-daemon --bin ta
    cargo run --bin ta -- shell {{ARGS}}

# Start the daemon in API mode (no shell)
daemon *ARGS:
    cargo run --bin ta-daemon -- --api {{ARGS}}

# Regenerate all icon formats from the master 1024px PNG.
# Requires: imagemagick (in Nix devShell). On macOS also uses iconutil.
icons:
    #!/usr/bin/env bash
    set -euo pipefail
    MASTER="images/icons/icon_1024x1024.png"
    if [ ! -f "$MASTER" ]; then
        echo "ERROR: Master icon not found at $MASTER" >&2
        exit 1
    fi
    echo "Generating PNG sizes from $MASTER..."
    for size in 16 32 48 128 256 512; do
        OUT="images/icons/icon_${size}x${size}.png"
        magick "$MASTER" -resize "${size}x${size}" "$OUT"
        echo "  → $OUT (${size}x${size})"
    done
    echo "Generating Windows .ico..."
    magick images/icons/icon_16x16.png images/icons/icon_32x32.png \
        images/icons/icon_48x48.png images/icons/icon_256x256.png \
        images/icons/ta.ico
    echo "  → images/icons/ta.ico"
    if command -v iconutil >/dev/null 2>&1; then
        echo "Generating macOS .icns via iconutil..."
        ICONSET=$(mktemp -d)/ta.iconset
        mkdir -p "$ICONSET"
        cp images/icons/icon_16x16.png   "$ICONSET/icon_16x16.png"
        cp images/icons/icon_32x32.png   "$ICONSET/icon_16x16@2x.png"
        cp images/icons/icon_32x32.png   "$ICONSET/icon_32x32.png"
        cp images/icons/icon_128x128.png "$ICONSET/icon_32x32@2x.png"
        cp images/icons/icon_128x128.png "$ICONSET/icon_128x128.png"
        cp images/icons/icon_256x256.png "$ICONSET/icon_128x128@2x.png"
        cp images/icons/icon_256x256.png "$ICONSET/icon_256x256.png"
        cp images/icons/icon_512x512.png "$ICONSET/icon_256x256@2x.png"
        cp images/icons/icon_512x512.png "$ICONSET/icon_512x512.png"
        cp images/icons/icon_1024x1024.png "$ICONSET/icon_512x512@2x.png"
        iconutil -c icns -o images/icons/ta.icns "$ICONSET"
        rm -rf "$(dirname "$ICONSET")"
        echo "  → images/icons/ta.icns"
    else
        echo "  ℹ iconutil not available (macOS only) — skipping .icns generation"
    fi
    echo "Done. All icon formats regenerated."

# Create macOS .app bundle from built binary.
# Run `just build` first, then `just package-macos`.
package-macos:
    #!/usr/bin/env bash
    set -euo pipefail
    APP="target/TrustedAutonomy.app"
    BINARY="target/debug/ta"
    if [ ! -f "$BINARY" ]; then
        BINARY="target/release/ta"
    fi
    if [ ! -f "$BINARY" ]; then
        echo "ERROR: ta binary not found. Run 'just build' first." >&2
        exit 1
    fi
    echo "Creating macOS .app bundle at $APP..."
    rm -rf "$APP"
    mkdir -p "$APP/Contents/MacOS"
    mkdir -p "$APP/Contents/Resources"
    cp "$BINARY" "$APP/Contents/MacOS/ta"
    cp images/icons/ta.icns "$APP/Contents/Resources/ta.icns"
    VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
    cat > "$APP/Contents/Info.plist" << PLIST
    <?xml version="1.0" encoding="UTF-8"?>
    <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
    <plist version="1.0">
    <dict>
        <key>CFBundleName</key>
        <string>Trusted Autonomy</string>
        <key>CFBundleDisplayName</key>
        <string>Trusted Autonomy</string>
        <key>CFBundleIdentifier</key>
        <string>com.trustedautonomy.ta</string>
        <key>CFBundleVersion</key>
        <string>${VERSION}</string>
        <key>CFBundleShortVersionString</key>
        <string>${VERSION}</string>
        <key>CFBundleExecutable</key>
        <string>ta</string>
        <key>CFBundleIconFile</key>
        <string>ta</string>
        <key>CFBundlePackageType</key>
        <string>APPL</string>
        <key>NSHighResolutionCapable</key>
        <true/>
    </dict>
    </plist>
    PLIST
    echo "  → $APP/Contents/MacOS/ta"
    echo "  → $APP/Contents/Resources/ta.icns"
    echo "  → $APP/Contents/Info.plist"
    echo "macOS .app bundle created at $APP"

# Install Linux desktop entry and icons to XDG standard locations.
# Uses ~/.local by default. Set PREFIX to override (e.g., just package-linux PREFIX=/usr).
package-linux PREFIX="$HOME/.local":
    #!/usr/bin/env bash
    set -euo pipefail
    BINARY="target/debug/ta"
    if [ ! -f "$BINARY" ]; then
        BINARY="target/release/ta"
    fi
    if [ ! -f "$BINARY" ]; then
        echo "ERROR: ta binary not found. Run 'just build' first." >&2
        exit 1
    fi
    echo "Installing Linux desktop integration to {{PREFIX}}..."
    mkdir -p "{{PREFIX}}/bin"
    cp "$BINARY" "{{PREFIX}}/bin/ta"
    for size in 16 32 48 128 256 512; do
        ICON_DIR="{{PREFIX}}/share/icons/hicolor/${size}x${size}/apps"
        mkdir -p "$ICON_DIR"
        cp "images/icons/icon_${size}x${size}.png" "$ICON_DIR/ta.png"
        echo "  → $ICON_DIR/ta.png"
    done
    mkdir -p "{{PREFIX}}/share/applications"
    cp ta.desktop "{{PREFIX}}/share/applications/ta.desktop"
    echo "  → {{PREFIX}}/share/applications/ta.desktop"
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache "{{PREFIX}}/share/icons/hicolor" 2>/dev/null || true
    fi
    echo "Linux desktop integration installed to {{PREFIX}}"
