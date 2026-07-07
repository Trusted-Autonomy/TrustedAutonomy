//! CLI Verb-Set Consolidation (v0.17.0.12.16).
//!
//! Implements the 10-verb primary CLI surface from
//! `docs/design/ta-concepts-and-architecture.md` §5/§11:
//! create/list/show/update/remove/run/approve/deny/apply/check/sync,
//! with nouns as positional subjects (`ta <verb> <noun> [id] [flags]`).
//!
//! `run`/`approve`/`deny`/`apply` already have first-class, well-shaped
//! top-level or `draft`-scoped forms (`ta run`, `ta draft approve/deny/apply`)
//! per the design doc's own note that these "already fit this shape and
//! need no change" — this module covers create/list/show/update/remove/check/sync.
//!
//! Rather than re-implementing every noun's logic a second time, a
//! verb+noun invocation is resolved to the equivalent legacy argv
//! (`["ta", <legacy-noun-word>, <legacy-action-word>, id?, ...extra]`) and
//! re-parsed through the *same* `Cli`/`Commands` clap definitions used by
//! the legacy noun-first surface, then dispatched through the same
//! `dispatch_raw` used everywhere else. This guarantees the new verb+noun
//! form and the legacy form execute byte-identical code — there is no
//! second copy of any command's behavior to drift out of sync.

/// One noun's verb -> legacy-subcommand-word mapping.
///
/// `keys` are the accepted spellings a user may type for this noun
/// (singular, plural, and the design doc's exact noun name where it
/// differs, e.g. "plan-phase"). `legacy` is the top-level clap keyword
/// (`ta <legacy> <action> ...`). `verbs` maps our verb name to the legacy
/// action's clap keyword (kebab-case, matching clap's auto-renaming of the
/// `Subcommand` variant).
struct NounEntry {
    keys: &'static [&'static str],
    legacy: &'static str,
    verbs: &'static [(&'static str, &'static str)],
}

/// Canonical (verb, noun) -> legacy subcommand mapping, built from a direct
/// read of every target command module's `Subcommand` enum (see the
/// v0.17.0.12.16 change summary for the file/line evidence per entry).
///
/// Only verbs with a clean, unambiguous, behavior-preserving legacy
/// equivalent are listed. Nouns/verbs not listed here have no first-class
/// verb+noun form yet — the legacy noun-first command keeps working
/// unchanged (deprecation notice only, no forwarding target), per the
/// doc's "deprecation/alias window, not a hard cutover" instruction.
const NOUN_TABLE: &[NounEntry] = &[
    NounEntry {
        keys: &["goal", "goals"],
        legacy: "goal",
        verbs: &[
            ("list", "list"),
            ("show", "status"),
            ("remove", "delete"),
            ("sync", "gc"),
        ],
    },
    NounEntry {
        keys: &["draft", "drafts"],
        legacy: "draft",
        verbs: &[
            ("list", "list"),
            ("show", "view"),
            ("remove", "close"),
            ("approve", "approve"),
            ("deny", "deny"),
            ("apply", "apply"),
            ("sync", "gc"),
        ],
    },
    NounEntry {
        keys: &["plan-phase", "plan-phases", "plan", "phase"],
        legacy: "plan",
        verbs: &[
            ("list", "list"),
            ("show", "status"),
            ("create", "create-phase"),
            ("check", "validate"),
            ("sync", "repair"),
        ],
    },
    NounEntry {
        keys: &["team", "teams"],
        legacy: "team",
        verbs: &[("list", "list"), ("update", "assign")],
    },
    NounEntry {
        keys: &["persona", "personas"],
        legacy: "persona",
        verbs: &[
            ("list", "list"),
            ("create", "new"),
            ("show", "show"),
            ("update", "set-agent"),
        ],
    },
    NounEntry {
        keys: &["workflow", "workflows"],
        legacy: "workflow",
        verbs: &[
            ("list", "list"),
            ("show", "show"),
            ("create", "new"),
            ("update", "update"),
            ("remove", "remove"),
            ("check", "validate"),
            ("sync", "update-index"),
        ],
    },
    NounEntry {
        keys: &["plugin", "plugins"],
        legacy: "plugin",
        verbs: &[
            ("create", "install"),
            ("list", "list"),
            ("show", "status"),
            ("remove", "remove"),
            ("check", "check"),
            ("sync", "upgrade"),
        ],
    },
    NounEntry {
        keys: &["template", "templates"],
        legacy: "template",
        verbs: &[
            ("create", "install"),
            ("list", "list"),
            ("remove", "remove"),
        ],
    },
    NounEntry {
        keys: &["session", "sessions"],
        legacy: "session",
        verbs: &[("list", "list"), ("show", "status"), ("remove", "abort")],
    },
    NounEntry {
        keys: &["credential", "credentials"],
        legacy: "credentials",
        verbs: &[("create", "add"), ("list", "list"), ("remove", "revoke")],
    },
    NounEntry {
        keys: &["event", "events"],
        legacy: "events",
        verbs: &[("show", "stats"), ("sync", "prune")],
    },
    NounEntry {
        keys: &["token", "tokens"],
        legacy: "token",
        verbs: &[("create", "create"), ("list", "list"), ("sync", "cleanup")],
    },
    NounEntry {
        keys: &["office"],
        legacy: "office",
        verbs: &[
            ("create", "start"),
            ("show", "status"),
            ("remove", "stop"),
            ("sync", "reload"),
        ],
    },
    NounEntry {
        keys: &["daemon"],
        legacy: "daemon",
        verbs: &[
            ("create", "start"),
            ("show", "status"),
            ("remove", "stop"),
            ("check", "health"),
            ("sync", "restart"),
        ],
    },
    NounEntry {
        keys: &["connector", "connectors"],
        legacy: "connector",
        verbs: &[
            ("create", "install"),
            ("list", "list"),
            ("show", "status"),
            ("remove", "stop"),
            ("sync", "restart"),
        ],
    },
    NounEntry {
        keys: &["community-resource", "community-resources", "community"],
        legacy: "community",
        verbs: &[("list", "list"), ("show", "get"), ("sync", "sync")],
    },
    NounEntry {
        keys: &["context"],
        legacy: "context",
        verbs: &[
            ("create", "store"),
            ("list", "list"),
            ("show", "recall"),
            ("remove", "forget"),
            ("check", "stats"),
        ],
    },
    NounEntry {
        keys: &["agent", "agents"],
        legacy: "agent",
        verbs: &[
            ("create", "new"),
            ("list", "list"),
            ("show", "info"),
            ("check", "doctor"),
            ("remove", "remove"),
            ("sync", "migrate"),
        ],
    },
];

/// The full list of nouns and per-noun supported verbs, for `--help`-style
/// listings and error messages.
pub fn known_nouns() -> Vec<&'static str> {
    NOUN_TABLE.iter().map(|e| e.keys[0]).collect()
}

/// Convert a PascalCase identifier to clap's default kebab-case renaming
/// (e.g. "PostMortem" -> "post-mortem"), matching how clap's `Subcommand`
/// derive spells each variant on the command line.
pub fn to_kebab(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i != 0 {
                out.push('-');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Extract the kebab-cased action word from a `Subcommand` enum value's
/// `Debug` output (its variant name, e.g. `Status { id: "x" }` -> `"status"`).
///
/// Used to identify which legacy action a directly-typed command invoked,
/// for the deprecation notice's `new_form_for` lookup — this avoids
/// hand-maintaining a second copy of every enum's variant list.
pub fn action_word_from_debug<T: std::fmt::Debug>(cmd: &T) -> String {
    let debug = format!("{cmd:?}");
    let variant = debug.split([' ', '(', '{']).next().unwrap_or(&debug);
    to_kebab(variant)
}

fn find_entry(noun_raw: &str) -> Option<&'static NounEntry> {
    let normalized = noun_raw.trim().to_ascii_lowercase();
    NOUN_TABLE
        .iter()
        .find(|e| e.keys.iter().any(|k| *k == normalized))
}

/// Resolve a `ta <verb> <noun> [id] [extra...]` invocation to the
/// equivalent legacy argv (`["ta", <noun>, <action>, ...]`), suitable for
/// `Cli::try_parse_from`.
///
/// Returns a descriptive error (not a panic) when the noun is unknown or
/// the verb has no mapped equivalent for that noun yet — both are normal,
/// expected outcomes during the deprecation window, not bugs.
pub fn resolve(
    verb: &str,
    noun_raw: &str,
    id: Option<&str>,
    extra: &[String],
) -> anyhow::Result<Vec<String>> {
    let entry = find_entry(noun_raw).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown noun '{noun_raw}' for `ta {verb}`. Known nouns: {}.\n\
             See `ta {verb} --help` or docs/USAGE.md's CLI Verb Reference.",
            known_nouns().join(", ")
        )
    })?;

    let action = entry
        .verbs
        .iter()
        .find(|(v, _)| *v == verb)
        .map(|(_, action)| *action)
        .ok_or_else(|| {
            let supported: Vec<&str> = entry.verbs.iter().map(|(v, _)| *v).collect();
            anyhow::anyhow!(
                "`ta {verb} {noun_raw}` isn't mapped to the new CLI surface yet.\n\
                 Supported verbs for '{noun_raw}': {}.\n\
                 The legacy `ta {legacy} <action>` form still works unchanged — see docs/USAGE.md.",
                if supported.is_empty() {
                    "(none yet)".to_string()
                } else {
                    supported.join(", ")
                },
                legacy = entry.legacy
            )
        })?;

    let mut argv = vec![
        "ta".to_string(),
        entry.legacy.to_string(),
        action.to_string(),
    ];
    if let Some(id) = id {
        argv.push(id.to_string());
    }
    argv.extend(extra.iter().cloned());
    Ok(argv)
}

/// Reverse-lookup used by the deprecation notice: given the legacy
/// top-level keyword and matched action keyword (both already
/// kebab-cased by clap), find the new verb+noun spelling, if one exists.
pub fn new_form_for(legacy: &str, action: &str) -> Option<String> {
    for entry in NOUN_TABLE {
        if entry.legacy != legacy {
            continue;
        }
        if let Some((verb, _)) = entry.verbs.iter().find(|(_, a)| *a == action) {
            return Some(format!("ta {verb} {}", entry.keys[0]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_goal_list() {
        let argv = resolve("list", "goal", None, &[]).unwrap();
        assert_eq!(argv, vec!["ta", "goal", "list"]);
    }

    #[test]
    fn resolve_goal_show_with_id() {
        let argv = resolve("show", "goal", Some("abc123"), &[]).unwrap();
        assert_eq!(argv, vec!["ta", "goal", "status", "abc123"]);
    }

    #[test]
    fn resolve_goal_remove_maps_to_delete() {
        let argv = resolve("remove", "goal", Some("abc123"), &[]).unwrap();
        assert_eq!(argv, vec!["ta", "goal", "delete", "abc123"]);
    }

    #[test]
    fn resolve_passes_through_extra_flags() {
        let argv = resolve(
            "remove",
            "goal",
            Some("abc123"),
            &["--reason".to_string(), "no longer needed".to_string()],
        )
        .unwrap();
        assert_eq!(
            argv,
            vec![
                "ta",
                "goal",
                "delete",
                "abc123",
                "--reason",
                "no longer needed"
            ]
        );
    }

    #[test]
    fn resolve_accepts_plural_noun() {
        let argv = resolve("list", "goals", None, &[]).unwrap();
        assert_eq!(argv, vec!["ta", "goal", "list"]);
    }

    #[test]
    fn resolve_unknown_noun_errors() {
        let err = resolve("list", "spaceship", None, &[]).unwrap_err();
        assert!(err.to_string().contains("Unknown noun"));
    }

    #[test]
    fn resolve_unsupported_verb_for_noun_errors_not_panics() {
        // "team" has no "create" mapping — must be a clear error, not a panic.
        let err = resolve("create", "team", None, &[]).unwrap_err();
        assert!(err.to_string().contains("isn't mapped"));
    }

    #[test]
    fn new_form_for_round_trips_with_resolve() {
        // Every mapped (verb, noun) pair must round-trip through the reverse lookup.
        for entry in NOUN_TABLE {
            for (verb, action) in entry.verbs {
                let found = new_form_for(entry.legacy, action);
                assert_eq!(
                    found,
                    Some(format!("ta {verb} {}", entry.keys[0])),
                    "round-trip failed for legacy `{} {}`",
                    entry.legacy,
                    action
                );
            }
        }
    }

    #[test]
    fn new_form_for_unmapped_action_returns_none() {
        assert_eq!(new_form_for("goal", "input"), None);
    }

    #[test]
    fn every_noun_table_entry_has_at_least_one_verb() {
        for entry in NOUN_TABLE {
            assert!(
                !entry.verbs.is_empty(),
                "noun '{}' has no mapped verbs",
                entry.keys[0]
            );
        }
    }

    #[test]
    fn to_kebab_matches_clap_variant_renaming() {
        assert_eq!(to_kebab("List"), "list");
        assert_eq!(to_kebab("PostMortem"), "post-mortem");
        assert_eq!(to_kebab("CreatePhase"), "create-phase");
        assert_eq!(to_kebab("InstallQwen"), "install-qwen");
    }

    #[derive(Debug)]
    #[allow(dead_code)]
    enum FakeCommands {
        Status { id: String },
        List,
    }

    #[test]
    fn action_word_from_debug_extracts_variant_name() {
        assert_eq!(
            action_word_from_debug(&FakeCommands::Status {
                id: "abc".to_string()
            }),
            "status"
        );
        assert_eq!(action_word_from_debug(&FakeCommands::List), "list");
    }

    #[test]
    fn draft_apply_approve_deny_already_fit_the_shape() {
        // Per the design doc §5: "ta draft apply <id> ... already fit this
        // shape and need no change" — confirm the new top-level forms
        // (`ta apply draft`, `ta approve draft`, `ta deny draft`) resolve
        // to exactly the pre-existing legacy invocation.
        assert_eq!(
            resolve("apply", "draft", Some("d1"), &[]).unwrap(),
            vec!["ta", "draft", "apply", "d1"]
        );
        assert_eq!(
            resolve("approve", "draft", Some("d1"), &[]).unwrap(),
            vec!["ta", "draft", "approve", "d1"]
        );
        assert_eq!(
            resolve("deny", "draft", Some("d1"), &[]).unwrap(),
            vec!["ta", "draft", "deny", "d1"]
        );
    }
}
