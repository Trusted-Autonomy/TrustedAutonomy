//! Draft ID resolution — the single authoritative function for turning any
//! user-supplied ID string into a concrete [`DraftPackage`].
//!
//! # Resolution order
//!
//! 1. Exact UUID (`cbda7f5f-4a19-4752-bea4-802af93fc020`)
//! 2. Shortref/seq (`6ebf85ab/1`) — goal 8-char prefix + draft sequence number
//! 3. Legacy display_id (`cbda7f5f-1`)
//! 4. UUID prefix — unambiguous prefix of ≥4 chars (error if ambiguous)
//! 5. 8-char all-hex — resolves to the latest draft for that goal shortref
//! 6. Tag match
//!
//! All draft subcommands route through [`resolve_draft`] so that every ID
//! format surfaced in `ta draft list` is accepted as input by every command.

use crate::draft_package::DraftPackage;
use uuid::Uuid;

/// Error returned when a draft ID cannot be resolved.
#[derive(Debug, Clone)]
pub enum DraftResolveError {
    /// Nothing matched the provided ID.
    ///
    /// `hint` lists candidate short IDs and titles to help the user.
    NotFound { input: String, hint: String },
    /// The provided prefix matches more than one draft.
    ///
    /// `candidates` is a list of `"<short_id>  <title>"` strings.
    Ambiguous {
        input: String,
        candidates: Vec<String>,
    },
}

impl std::fmt::Display for DraftResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DraftResolveError::NotFound { input, hint } => {
                write!(f, "No draft matching \"{}\". {}", input, hint)
            }
            DraftResolveError::Ambiguous { input, candidates } => {
                write!(
                    f,
                    "Ambiguous ID \"{}\" matches {} drafts:\n  {}\nSpecify more characters.",
                    input,
                    candidates.len(),
                    candidates.join("\n  ")
                )
            }
        }
    }
}

impl std::error::Error for DraftResolveError {}

/// Resolve a user-supplied draft ID to the matching [`DraftPackage`].
///
/// Accepts:
/// - Full UUID
/// - Shortref/seq (`6ebf85ab/1`)
/// - Legacy display_id prefix (e.g. `cbda7f5f-1`)
/// - UUID prefix (≥4 chars, unambiguous)
/// - 8-char all-hex goal shortref (resolves to the latest draft for that goal)
/// - Tag match
///
/// Returns a reference into `packages`, or a [`DraftResolveError`].
pub fn resolve_draft<'a>(
    packages: &'a [DraftPackage],
    id: &str,
) -> Result<&'a DraftPackage, DraftResolveError> {
    let not_found = |hint: &str| DraftResolveError::NotFound {
        input: id.to_string(),
        hint: hint.to_string(),
    };

    // ── 1. Exact UUID ──────────────────────────────────────────────────────
    if let Ok(uuid) = Uuid::parse_str(id) {
        return packages
            .iter()
            .find(|p| p.package_id == uuid)
            .ok_or_else(|| not_found("Run `ta draft list` to see available drafts."));
    }

    // ── 2. Shortref/seq (`<8hex>/<N>`) ────────────────────────────────────
    if let Some((shortref_part, seq_part)) = id.split_once('/') {
        if shortref_part.len() == 8 && shortref_part.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(seq) = seq_part.parse::<u32>() {
                let matched: Vec<&DraftPackage> = packages
                    .iter()
                    .filter(|p| {
                        p.goal_shortref.as_deref() == Some(shortref_part) && p.draft_seq == seq
                    })
                    .collect();
                return match matched.len() {
                    0 => Err(not_found("Run `ta draft list` to see available drafts.")),
                    1 => Ok(matched[0]),
                    _ => {
                        // Should not happen (seq is unique per goal), but handle gracefully.
                        let candidates: Vec<String> = matched
                            .iter()
                            .map(|p| {
                                format!("{}  {}", &p.package_id.to_string()[..8], p.goal.title)
                            })
                            .collect();
                        Err(DraftResolveError::Ambiguous {
                            input: id.to_string(),
                            candidates,
                        })
                    }
                };
            }
        }
        // `/` present but doesn't look like shortref/seq — fall through to other matchers.
    }

    // ── 3. Legacy display_id match (`cbda7f5f-1` or prefix thereof) ───────
    let display_matches: Vec<&DraftPackage> = packages
        .iter()
        .filter(|p| {
            p.display_id
                .as_deref()
                .is_some_and(|did| did == id || did.starts_with(id))
        })
        .collect();
    if display_matches.len() == 1 {
        return Ok(display_matches[0]);
    }
    if display_matches.len() > 1 {
        let candidates: Vec<String> = display_matches
            .iter()
            .map(|p| format!("{}  {}", &p.package_id.to_string()[..8], p.goal.title))
            .collect();
        return Err(DraftResolveError::Ambiguous {
            input: id.to_string(),
            candidates,
        });
    }

    // ── 4. UUID prefix match ───────────────────────────────────────────────
    // Require ≥4 chars to avoid accidental broad matches.
    if id.len() >= 4 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') && !id.contains('/') {
        let prefix_matches: Vec<&DraftPackage> = packages
            .iter()
            .filter(|p| p.package_id.to_string().starts_with(id))
            .collect();
        if prefix_matches.len() == 1 {
            return Ok(prefix_matches[0]);
        }
        if prefix_matches.len() > 1 {
            let candidates: Vec<String> = prefix_matches
                .iter()
                .map(|p| format!("{}  {}", &p.package_id.to_string()[..8], p.goal.title))
                .collect();
            return Err(DraftResolveError::Ambiguous {
                input: id.to_string(),
                candidates,
            });
        }
    }

    // ── 5. 8-char all-hex: latest draft for that goal shortref ────────────
    if id.len() == 8 && id.chars().all(|c| c.is_ascii_hexdigit()) {
        let shortref_matches: Vec<&DraftPackage> = packages
            .iter()
            .filter(|p| p.goal_shortref.as_deref() == Some(id))
            .collect();
        if !shortref_matches.is_empty() {
            let latest = shortref_matches
                .iter()
                .max_by_key(|p| p.created_at)
                .unwrap();
            return Ok(latest);
        }
    }

    // ── 6. Tag match ──────────────────────────────────────────────────────
    let tag_matches: Vec<&DraftPackage> = packages
        .iter()
        .filter(|p| {
            p.tag
                .as_deref()
                .is_some_and(|t| t == id || t.starts_with(id))
        })
        .collect();
    if tag_matches.len() == 1 {
        return Ok(tag_matches[0]);
    }
    if tag_matches.len() > 1 {
        let candidates: Vec<String> = tag_matches
            .iter()
            .map(|p| format!("{}  {}", &p.package_id.to_string()[..8], p.goal.title))
            .collect();
        return Err(DraftResolveError::Ambiguous {
            input: id.to_string(),
            candidates,
        });
    }

    Err(not_found("Run `ta draft list` to see available drafts."))
}

/// Return the canonical display ID for a draft — the string that `resolve_draft`
/// will accept back as input.
///
/// Prefers `<goal_shortref>/<draft_seq>` (shortest and most human-friendly),
/// then falls back to `display_id`, then to the first 8 chars of the UUID.
pub fn draft_canonical_id(pkg: &DraftPackage) -> String {
    if let (Some(shortref), seq) = (&pkg.goal_shortref, pkg.draft_seq) {
        if seq > 0 {
            return format!("{}/{}", shortref, seq);
        }
    }
    pkg.display_id
        .as_deref()
        .unwrap_or(&pkg.package_id.to_string()[..8])
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft_package::make_test_pkg;

    #[test]
    fn resolve_by_full_uuid() {
        let pkg = make_test_pkg("aabbccdd", 1);
        let id = pkg.package_id.to_string();
        let packages = vec![pkg];
        let result = resolve_draft(&packages, &id);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().package_id.to_string(), id);
    }

    #[test]
    fn resolve_by_shortref_seq() {
        let pkg = make_test_pkg("aabbccdd", 1);
        let packages = vec![pkg];
        let result = resolve_draft(&packages, "aabbccdd/1");
        assert!(result.is_ok());
        let found = result.unwrap();
        assert_eq!(found.goal_shortref.as_deref(), Some("aabbccdd"));
        assert_eq!(found.draft_seq, 1);
    }

    #[test]
    fn resolve_by_shortref_seq_second_draft() {
        let pkg1 = make_test_pkg("aabbccdd", 1);
        let mut pkg2 = make_test_pkg("aabbccdd", 2);
        pkg2.created_at = chrono::Utc::now() + chrono::Duration::seconds(5);
        let packages = vec![pkg1, pkg2];
        let result = resolve_draft(&packages, "aabbccdd/2");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().draft_seq, 2);
    }

    #[test]
    fn resolve_by_8char_shortref_returns_latest() {
        let pkg1 = make_test_pkg("aabbccdd", 1);
        let mut pkg2 = make_test_pkg("aabbccdd", 2);
        pkg2.created_at = chrono::Utc::now() + chrono::Duration::seconds(5);
        let packages = vec![pkg1, pkg2];
        let result = resolve_draft(&packages, "aabbccdd");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().draft_seq, 2);
    }

    #[test]
    fn resolve_by_uuid_prefix() {
        let pkg = make_test_pkg("aabbccdd", 1);
        let prefix = pkg.package_id.to_string()[..8].to_string();
        let packages = vec![pkg];
        let result = resolve_draft(&packages, &prefix);
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_ambiguous_tag_errors() {
        let mut pkg1 = make_test_pkg("11223344", 1);
        pkg1.tag = Some("my-tag".to_string());
        let mut pkg2 = make_test_pkg("55667788", 1);
        pkg2.tag = Some("my-tag".to_string());
        let packages = vec![pkg1, pkg2];
        let result = resolve_draft(&packages, "my-tag");
        assert!(matches!(result, Err(DraftResolveError::Ambiguous { .. })));
    }

    #[test]
    fn resolve_unknown_id_returns_not_found() {
        let pkg = make_test_pkg("aabbccdd", 1);
        let packages = vec![pkg];
        let result = resolve_draft(&packages, "ffffffff/99");
        assert!(matches!(result, Err(DraftResolveError::NotFound { .. })));
    }

    #[test]
    fn draft_canonical_id_prefers_shortref_seq() {
        let pkg = make_test_pkg("aabbccdd", 3);
        assert_eq!(draft_canonical_id(&pkg), "aabbccdd/3");
    }

    #[test]
    fn draft_canonical_id_falls_back_to_display_id() {
        let mut pkg = make_test_pkg("aabbccdd", 0); // seq=0 means not set
        pkg.display_id = Some("aabbccdd-01".to_string());
        assert_eq!(draft_canonical_id(&pkg), "aabbccdd-01");
    }
}
