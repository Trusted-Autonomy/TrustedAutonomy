// packaging.rs — Tests for per-platform icon packaging (v0.10.18.7).
//
// These tests verify that:
// 1. The icon source files exist and are valid
// 2. The macOS .app bundle recipe produces correct structure
// 3. The Linux .desktop file is well-formed
// 4. The Windows build.rs icon embedding path is correct

use std::path::Path;

/// Verify all required icon source files exist.
#[test]
fn icon_source_files_exist() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let icons_dir = workspace_root.join("images/icons");

    // Master PNG
    assert!(
        icons_dir.join("icon_1024x1024.png").exists(),
        "Master 1024x1024 PNG must exist"
    );

    // Required PNG sizes
    for size in &[16, 32, 48, 128, 256, 512] {
        let name = format!("icon_{size}x{size}.png");
        assert!(
            icons_dir.join(&name).exists(),
            "Missing icon: {name}. Run 'just icons' to generate."
        );
    }

    // Platform-specific formats
    assert!(
        icons_dir.join("ta.icns").exists(),
        "macOS .icns must exist. Run 'just icons' on macOS to generate."
    );
    assert!(
        icons_dir.join("ta.ico").exists(),
        "Windows .ico must exist. Run 'just icons' to generate."
    );
}

/// Verify the Windows icon path referenced in build.rs exists.
#[test]
fn windows_ico_path_valid() {
    let build_rs_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ico_path = build_rs_dir.join("../../images/icons/ta.ico");
    assert!(
        ico_path.exists(),
        "build.rs references ../../images/icons/ta.ico but file not found at {}",
        ico_path.display()
    );
}

/// Verify the Linux .desktop file exists and contains required fields.
#[test]
fn linux_desktop_file_valid() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let desktop_path = workspace_root.join("ta.desktop");
    assert!(
        desktop_path.exists(),
        "ta.desktop file must exist at project root"
    );

    let content = std::fs::read_to_string(&desktop_path).unwrap();

    // Required XDG Desktop Entry fields
    assert!(
        content.contains("[Desktop Entry]"),
        "ta.desktop must contain [Desktop Entry] header"
    );
    assert!(
        content.contains("Name="),
        "ta.desktop must contain Name= field"
    );
    assert!(
        content.contains("Exec="),
        "ta.desktop must contain Exec= field"
    );
    assert!(
        content.contains("Icon=ta"),
        "ta.desktop must contain Icon=ta field"
    );
    assert!(
        content.contains("Type=Application"),
        "ta.desktop must contain Type=Application"
    );
    assert!(
        content.contains("Categories="),
        "ta.desktop must contain Categories= field"
    );
}

/// Verify the macOS .icns file has valid magic bytes.
#[test]
fn macos_icns_valid_format() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let icns_path = workspace_root.join("images/icons/ta.icns");
    let data = std::fs::read(&icns_path).unwrap();

    // ICNS magic bytes: "icns" (0x69636E73)
    assert!(
        data.len() >= 8,
        "ICNS file too small ({} bytes)",
        data.len()
    );
    assert_eq!(
        &data[..4],
        b"icns",
        "ICNS file does not start with 'icns' magic bytes"
    );
}

/// Verify web UI favicon assets exist in the daemon assets directory.
#[test]
fn web_favicon_assets_exist() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let assets_dir = workspace_root.join("crates/ta-daemon/assets");

    assert!(
        assets_dir.join("favicon.ico").exists(),
        "favicon.ico must exist in daemon assets"
    );
    assert!(
        assets_dir.join("icon-192.png").exists(),
        "icon-192.png must exist in daemon assets"
    );
    assert!(
        assets_dir.join("icon-512.png").exists(),
        "icon-512.png must exist in daemon assets"
    );
}

/// Verify index.html references favicon link tags.
#[test]
fn index_html_has_favicon_links() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let html =
        std::fs::read_to_string(workspace_root.join("crates/ta-daemon/assets/index.html")).unwrap();

    assert!(
        html.contains("favicon.ico"),
        "index.html must reference favicon.ico"
    );
    assert!(
        html.contains("icon-192.png"),
        "index.html must reference icon-192.png"
    );
}
