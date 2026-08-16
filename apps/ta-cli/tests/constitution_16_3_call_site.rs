// constitution_16_3_call_site.rs — Enforces constitution §16.3's named
// call-site invariant at CI time (v0.17.7.3), not just in prose: a PR that
// adds a new direct caller of `should_auto_approve_draft`,
// `check_advisor_auto_approve`, or `run_consensus` for the purpose of gating
// an apply/merge decision must fail this test unless the new call site is
// added to one of the allowlists below with a comment justifying it (the
// same "documented deviation" pattern PLAN.md itself uses).
//
// This is a grep-based structural check, not a full Rust-aware call-graph
// analysis — it can't distinguish "gating" from "non-gating" uses of these
// functions on its own, which is why the allowlists below exist: each entry
// documents *why* that file is allowed to reference the function by name.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Files allowed to reference `should_auto_approve_draft` by name.
fn policy_auto_approve_allowlist() -> HashSet<&'static str> {
    [
        // Definition + its own internal fallback caller.
        "crates/ta-policy/src/auto_approve.rs",
        // The one sanctioned graph-engine wrapper (constitution §16.1).
        "crates/ta-workflow/src/graph/nodes/policy_reviewer.rs",
        // `ta policy check` — a read-only dry-run diagnostic that prints
        // "WOULD AUTO-APPROVE"/"WOULD NOT AUTO-APPROVE"; it never applies or
        // blocks anything itself, so it isn't a gating call site.
        "apps/ta-cli/src/commands/policy.rs",
        // MCP submit-time convenience auto-approve (verification-command
        // execution + git_commit config, not the terminal apply/merge gate
        // constitution §16.3 targets). Explicitly scoped out of this
        // migration per v0.17.7.3's decision log — revisit if this path
        // grows its own apply-time authority beyond submit-time routing.
        "crates/ta-mcp-gateway/src/tools/draft.rs",
    ]
    .into_iter()
    .collect()
}

/// Files allowed to reference `check_advisor_auto_approve` by name.
fn check_advisor_auto_approve_allowlist() -> HashSet<&'static str> {
    [
        // Definition only — confirmed dead code with zero production
        // callers as of v0.17.7.3 (superseded by the graph engine's
        // `AdvisorConfidenceReviewer`, which reimplements the same
        // `ta_decision::decide()` check rather than wrapping this function).
        "crates/ta-session/src/advisor_agent.rs",
    ]
    .into_iter()
    .collect()
}

/// Files allowed to reference `run_consensus` by name.
fn run_consensus_allowlist() -> HashSet<&'static str> {
    [
        // Definition + its own tests.
        "crates/ta-workflow/src/consensus/mod.rs",
        // The one sanctioned graph-engine wrapper (constitution §16.1).
        "crates/ta-workflow/src/graph/nodes/weighted_decision.rs",
        // Generic `kind = "consensus"` pipeline-stage executor for the
        // step-based Workflow TOML engine — gates whether a *stage*
        // proceeds (config-driven since v0.17.7.3), not `ta draft apply`'s
        // approval gate specifically. Not in scope for the §16.3 migration,
        // which targets the apply/merge gate named in the constitution text.
        "apps/ta-cli/src/commands/governed_workflow.rs",
    ]
    .into_iter()
    .collect()
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Recursively collect every `.rs` file under `dir`, skipping `target/`.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some("target") {
                continue;
            }
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Assert that every `.rs` file under `crates/` and `apps/` containing a
/// call-syntax occurrence of `needle` (`"foo("`) is in `allowlist` (paths
/// relative to the workspace root, `/`-separated).
fn assert_only_allowlisted_callers(needle: &str, allowlist: &HashSet<&'static str>) {
    let root = workspace_root();
    let mut files = Vec::new();
    for sub in ["crates", "apps"] {
        collect_rs_files(&root.join(sub), &mut files);
    }

    let pattern = format!("{needle}(");
    let mut offenders = Vec::new();
    for file in &files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        if !content.contains(&pattern) {
            continue;
        }
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        // This file itself names all three functions in allowlist comments
        // and error messages — not a call site.
        if rel == "apps/ta-cli/tests/constitution_16_3_call_site.rs" {
            continue;
        }
        if !allowlist.contains(rel.as_str()) {
            offenders.push(rel);
        }
    }

    assert!(
        offenders.is_empty(),
        "constitution §16.3 violation: found a new, non-allowlisted reference to \
         `{needle}(` in {offenders:?}. `ta draft apply`'s approval check must call \
         exactly one graph instance (see apps/ta-cli/src/commands/workflow_graph.rs \
         ::run_apply_gate) — new gating logic must be a ReviewerNode/DecisionNode fed \
         into that graph, not a new direct caller of `{needle}`. If this reference is \
         genuinely not a gating call site, add it to the allowlist in \
         apps/ta-cli/tests/constitution_16_3_call_site.rs with a comment explaining why."
    );
}

#[test]
fn no_new_direct_callers_of_should_auto_approve_draft() {
    assert_only_allowlisted_callers(
        "should_auto_approve_draft",
        &policy_auto_approve_allowlist(),
    );
}

#[test]
fn no_new_direct_callers_of_check_advisor_auto_approve() {
    assert_only_allowlisted_callers(
        "check_advisor_auto_approve",
        &check_advisor_auto_approve_allowlist(),
    );
}

#[test]
fn no_new_direct_callers_of_run_consensus() {
    assert_only_allowlisted_callers("run_consensus", &run_consensus_allowlist());
}
