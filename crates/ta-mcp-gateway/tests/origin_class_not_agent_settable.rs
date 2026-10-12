//! `origin_class` is accounting data set by the launcher only (v0.17.12.15).
//!
//! The only places the gateway may mention it are the lines that stamp the class
//! the launcher exported onto a new goal. Any other mention (a tool parameter, a
//! handler that reads one) would be a way for an agent to set it, so this scan
//! fails the build.

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn gateway_sources_only_stamp_the_inherited_class() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(!files.is_empty());
    let mut offenders = Vec::new();
    for file in files {
        // Normalize CRLF so a Windows checkout scans the same lines.
        let text = std::fs::read_to_string(&file)
            .unwrap()
            .replace("\r\n", "\n");
        // The in-file unit test mentions the name; stop at the test module.
        let body = text.split("#[cfg(test)]").next().unwrap();
        for (n, line) in body.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") || !line.contains("origin_class") {
                continue;
            }
            let allowed = line.contains("inherited_origin_class()");
            if !allowed {
                offenders.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "origin_class must not be read or accepted by the gateway except to stamp the \
         launcher's inherited class: {offenders:#?}"
    );
}
