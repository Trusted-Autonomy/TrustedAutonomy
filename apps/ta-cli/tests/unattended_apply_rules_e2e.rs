// unattended_apply_rules_e2e.rs: end-to-end test of the rules-engine gate that
// guards an UNATTENDED file apply (v0.17.12.4).
//
// Seam note. The compiled `ta` binary can never reach the unattended branch of
// `ta draft apply`: the CLI derives attendance from `human_auth::proven_human_for`,
// which is true whenever `require_human_for("apply")` succeeded, and an agent or
// headless context without a credential is refused before the gate runs. The gate
// itself (`rules_gate::gate`) is `pub(crate)` in a binary-only crate, so it cannot
// be called from here either. This test therefore drives the same public pieces,
// in the same order, that `rules_gate::gate`, the file copy, and
// `rules_gate::mark_untrusted` use: `RulesLoader` (signature verification),
// `Loaded::decide`, `paths::check_no_links` for project and staging, then the
// copy and `untrusted::record`. No Keychain is involved: the owner key is a
// seed held in the test, and the trust anchor is a file in a tempdir state
// directory (`RulesLoader::with_dirs`).

use std::fs;
use std::path::{Path, PathBuf};

use ta_actions::rules::load::sign_owner_file;
use ta_actions::rules::untrusted::{self, AutoApplied};
use ta_actions::rules::{paths, signing, Loaded, Request, RulesLoader, When};

const RULES: &str =
    "version = 1\n[[allow]]\naction = \"apply\"\nwhen = \"unattended\"\npaths = [\"docs\"]\n";

struct Env {
    project: tempfile::TempDir,
    staging: tempfile::TempDir,
    state: tempfile::TempDir,
    seed: [u8; 32],
}

impl Env {
    fn new() -> Self {
        let e = Env {
            project: tempfile::tempdir().unwrap(),
            staging: tempfile::tempdir().unwrap(),
            state: tempfile::tempdir().unwrap(),
            seed: [7u8; 32],
        };
        fs::create_dir_all(e.project.path().join(".ta")).unwrap();
        // The trust anchor lives outside the tree, in the per-project state dir.
        fs::write(
            e.state.path().join("owner.pub"),
            signing::public_to_b64(&signing::public_of(&e.seed)),
        )
        .unwrap();
        e
    }

    fn loader(&self) -> RulesLoader {
        RulesLoader::with_dirs(
            self.project.path(),
            Some(self.state.path().to_path_buf()),
            None,
        )
    }

    fn write_rules(&self, text: &str, sign: bool) {
        fs::write(self.loader().owner_path(), text).unwrap();
        if sign {
            sign_owner_file(&self.loader(), &self.seed, text, chrono::Utc::now())
                .expect("rules must sign");
        }
    }

    fn stage(&self, rel: &str, body: &[u8]) {
        let p = self.staging.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn project_file(&self, rel: &str) -> PathBuf {
        self.project.path().join(rel)
    }

    /// The unattended apply, composed from the public pieces the CLI uses.
    /// All-or-nothing: every item is decided before anything is written.
    fn unattended_apply(&self, draft_id: &str, rels: &[&str]) -> Result<(), String> {
        let loaded: Loaded = self.loader().load(None);
        let mut refusals = Vec::new();
        let mut planned: Vec<(String, String)> = Vec::new();
        for rel in rels {
            let d = loaded.decide(&Request::new("apply", When::Unattended).with_path(rel));
            if d.is_deny() {
                refusals.push(format!("{rel}: {}", d.refusal("apply", When::Unattended)));
                continue;
            }
            for (label, base) in [
                ("project", self.project.path()),
                ("staging", self.staging.path()),
            ] {
                if let Err(l) = paths::check_no_links(base, rel) {
                    refusals.push(format!(
                        "{rel}: refuses links ({label}) {} {}",
                        l.path, l.why
                    ));
                }
            }
            planned.push((rel.to_string(), d.rule.clone()));
        }
        if !refusals.is_empty() {
            return Err(format!(
                "stopped before changing anything:\n  - {}",
                refusals.join("\n  - ")
            ));
        }
        for (rel, _) in &planned {
            let dest = self.project_file(rel);
            fs::create_dir_all(dest.parent().unwrap()).unwrap();
            fs::copy(self.staging.path().join(rel), dest).unwrap();
        }
        let now = chrono::Utc::now();
        let entries: Vec<AutoApplied> = planned
            .iter()
            .map(|(rel, rule)| {
                AutoApplied::new(
                    draft_id,
                    rel,
                    untrusted::sha256_of_file(&self.project_file(rel)),
                    rule,
                    now,
                )
            })
            .collect();
        untrusted::record(self.state.path(), &entries)
    }
}

#[test]
fn signed_allow_rule_applies_and_marks_content_untrusted() {
    let e = Env::new();
    e.write_rules(RULES, true);
    // CRLF content must round-trip byte for byte.
    let body = b"# Research\r\nline two\r\n";
    e.stage(&Path::new("docs").join("note.md").to_string_lossy(), body);

    e.unattended_apply("draft-1", &["docs/note.md"]).unwrap();

    assert_eq!(fs::read(e.project_file("docs/note.md")).unwrap(), body);
    assert!(untrusted::is_untrusted(e.state.path(), "docs/note.md"));
    assert!(untrusted::is_untrusted(e.state.path(), "Docs\\Note.md"));
    let marks = untrusted::load(e.state.path());
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].trust, "untrusted");
    assert_eq!(marks[0].draft_id, "draft-1");
    assert_eq!(
        marks[0].sha256,
        untrusted::sha256_of_file(&e.project_file("docs/note.md"))
    );
    assert!(marks[0].rule.contains("owner rules"), "{}", marks[0].rule);
    // The mark is outside the project tree, so applied content cannot clear it.
    assert!(!e.project_file(".ta/auto-applied.jsonl").exists());
}

#[test]
fn signed_rules_with_crlf_line_endings_still_verify() {
    let e = Env::new();
    e.write_rules(&RULES.replace('\n', "\r\n"), true);
    e.stage("docs/crlf.md", b"x\r\n");
    e.unattended_apply("draft-crlf", &["docs/crlf.md"]).unwrap();
    assert!(e.project_file("docs/crlf.md").exists());
}

#[test]
fn paths_outside_the_allowed_prefix_are_refused() {
    let e = Env::new();
    e.write_rules(RULES, true);
    e.stage("src/main.rs", b"fn main() {}\n");
    let err = e.unattended_apply("d", &["src/main.rs"]).unwrap_err();
    assert!(err.contains("src/main.rs"), "{err}");
    assert!(!e.project_file("src/main.rs").exists());
    assert!(untrusted::load(e.state.path()).is_empty());
}

#[test]
fn protected_targets_are_refused_even_with_a_broad_allow_prefix() {
    let e = Env::new();
    e.write_rules(RULES, true);
    let before = fs::read_to_string(e.loader().owner_path()).unwrap();
    for bad in [
        ".ta/rules.toml",
        ".ta/rules.toml.sig",
        ".TA\\rules.toml",
        "docs/../.ta/rules.toml",
        ".git/config",
        ".git/hooks/pre-commit",
        "docs/../.git/config",
    ] {
        // A good file rides along: one bad item must refuse the whole draft.
        e.stage("docs/ok.md", b"ok\n");
        let err = e
            .unattended_apply("d", &["docs/ok.md", bad])
            .expect_err(bad);
        assert!(
            err.contains("stopped before changing anything"),
            "{bad}: {err}"
        );
        assert!(
            !e.project_file("docs/ok.md").exists(),
            "{bad}: partial apply"
        );
    }
    assert!(untrusted::load(e.state.path()).is_empty());
    assert_eq!(fs::read_to_string(e.loader().owner_path()).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn symlink_artifacts_are_refused_in_staging_and_in_the_project() {
    use std::os::unix::fs::symlink;

    // Link in staging pointing outside the tree.
    let e = Env::new();
    e.write_rules(RULES, true);
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), "s").unwrap();
    fs::create_dir_all(e.staging.path().join("docs")).unwrap();
    symlink(
        outside.path().join("secret"),
        e.staging.path().join("docs/link.md"),
    )
    .unwrap();
    let err = e.unattended_apply("d", &["docs/link.md"]).unwrap_err();
    assert!(err.contains("links"), "{err}");
    assert!(!e.project_file("docs/link.md").exists());

    // A link in the project that the write would otherwise follow.
    let e = Env::new();
    e.write_rules(RULES, true);
    e.stage("docs/victim.md", b"new\n");
    fs::create_dir_all(e.project.path().join("docs")).unwrap();
    symlink(
        outside.path().join("secret"),
        e.project_file("docs/victim.md"),
    )
    .unwrap();
    let err = e.unattended_apply("d", &["docs/victim.md"]).unwrap_err();
    assert!(err.contains("links"), "{err}");
    assert_eq!(
        fs::read_to_string(outside.path().join("secret")).unwrap(),
        "s"
    );

    // A linked parent directory.
    let e = Env::new();
    e.write_rules(RULES, true);
    e.stage("docs/sub/a.md", b"a\n");
    let elsewhere = tempfile::tempdir().unwrap();
    symlink(elsewhere.path(), e.project_file("docs/sub")).ok();
    if e.project_file("docs/sub").exists() {
        assert!(e.unattended_apply("d", &["docs/sub/a.md"]).is_err());
        assert!(!elsewhere.path().join("a.md").exists());
    }
}

#[test]
fn unsigned_rules_file_fails_closed() {
    let e = Env::new();
    e.write_rules(RULES, false);
    e.stage("docs/a.md", b"a\n");

    let loaded = e.loader().load(None);
    assert!(
        !loaded.problems.is_empty(),
        "an unsigned file must be reported as a problem"
    );
    let err = e.unattended_apply("d", &["docs/a.md"]).unwrap_err();
    assert!(err.contains("docs/a.md"), "{err}");
    assert!(
        err.to_lowercase().contains("sign") || err.contains("verified"),
        "{err}"
    );
    assert!(!e.project_file("docs/a.md").exists());
    assert!(untrusted::load(e.state.path()).is_empty());
}

#[test]
fn tampered_rules_file_fails_closed() {
    let e = Env::new();
    e.write_rules(RULES, true);
    e.stage("docs/a.md", b"a\n");
    e.stage("src/evil.rs", b"x\n");

    // Sanity: the signed file works before the edit.
    e.unattended_apply("d0", &["docs/a.md"]).unwrap();
    fs::remove_file(e.project_file("docs/a.md")).unwrap();
    fs::remove_file(e.state.path().join("auto-applied.jsonl")).unwrap();

    // Widen the allow rule after signing.
    let widened = RULES.replace("paths = [\"docs\"]", "paths = [\"docs\", \"src\"]");
    fs::write(e.loader().owner_path(), widened).unwrap();

    let loaded = e.loader().load(None);
    assert!(!loaded.problems.is_empty(), "tampering must be reported");
    for rel in ["src/evil.rs", "docs/a.md"] {
        let err = e.unattended_apply("d1", &[rel]).unwrap_err();
        assert!(err.contains(rel), "{err}");
        assert!(!e.project_file(rel).exists(), "{rel} must not be applied");
    }
    assert!(untrusted::load(e.state.path()).is_empty());
}
