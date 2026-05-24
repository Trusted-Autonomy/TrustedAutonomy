// api/links.rs — GET /api/links — cross-project link status (v0.16.1.5).
//
// Returns all configured links with their reachability status and manifest excerpt.
// Used by the Studio "Linked Projects" panel.

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;

use crate::api::AppState;

/// A linked project entry for the Studio UI.
#[derive(Debug, Serialize)]
pub struct LinkEntry {
    pub name: String,
    pub relationship: String,
    pub path: Option<String>,
    pub repo: Option<String>,
    pub description: String,
    /// "ok" | "missing_manifest" | "unreachable"
    pub status: String,
    /// True if manifest is from a local cache (may be stale).
    pub cached: bool,
    /// First paragraph of the manifest's Purpose section, or None.
    pub manifest_excerpt: Option<String>,
    /// Full manifest content, or None if not available.
    pub manifest: Option<String>,
}

/// `GET /api/links` — return all cross-project links with status.
pub async fn get_links(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let project_root = state.project_root.clone();

    let links = ta_workspace::links::load(&project_root);
    let cache_dir = project_root.join(".ta").join("link-cache");

    let entries: Vec<LinkEntry> = links
        .iter()
        .map(|link| {
            let status = link.status(&project_root, &cache_dir);
            let manifest = link.read_manifest(&project_root, &cache_dir);
            let manifest_excerpt = manifest.as_deref().and_then(extract_purpose_excerpt);

            let (status_str, cached) = match &status {
                ta_workspace::LinkStatus::Ok { cached } => ("ok", *cached),
                ta_workspace::LinkStatus::MissingManifest => ("missing_manifest", false),
                ta_workspace::LinkStatus::Unreachable { .. } => ("unreachable", false),
            };

            LinkEntry {
                name: link.name.clone(),
                relationship: link.relationship.badge().to_string(),
                path: link.path.clone(),
                repo: link.repo.clone(),
                description: link.description.clone(),
                status: status_str.to_string(),
                cached,
                manifest_excerpt,
                manifest,
            }
        })
        .collect();

    Json(entries).into_response()
}

/// Extract the first non-empty paragraph after `## Purpose` from a manifest.
fn extract_purpose_excerpt(content: &str) -> Option<String> {
    let mut in_purpose = false;
    let mut lines_buf = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "## Purpose" {
            in_purpose = true;
            continue;
        }
        if in_purpose {
            // Stop at the next ## section.
            if trimmed.starts_with("## ") {
                break;
            }
            lines_buf.push(trimmed);
        }
    }

    // Take lines until first blank line (paragraph break).
    let paragraph: Vec<&str> = lines_buf
        .iter()
        .skip_while(|l| l.is_empty())
        .take_while(|l| !l.is_empty())
        .copied()
        .collect();

    if paragraph.is_empty() {
        None
    } else {
        Some(paragraph.join(" "))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_purpose_paragraph() {
        let manifest = "name: foo\ntype: app\nlanguage: rust\n---\n\n## Purpose\n\nThis is the purpose paragraph.\nSecond sentence.\n\n## Architecture\n\nA.\n";
        let excerpt = extract_purpose_excerpt(manifest);
        assert_eq!(
            excerpt.as_deref(),
            Some("This is the purpose paragraph. Second sentence.")
        );
    }

    #[test]
    fn extract_purpose_missing_section() {
        let manifest = "name: foo\n---\n\n## Architecture\n\nA.\n";
        assert!(extract_purpose_excerpt(manifest).is_none());
    }

    #[test]
    fn extract_purpose_empty_paragraph() {
        let manifest = "name: foo\n---\n\n## Purpose\n\n## Architecture\n\nA.\n";
        assert!(extract_purpose_excerpt(manifest).is_none());
    }
}
