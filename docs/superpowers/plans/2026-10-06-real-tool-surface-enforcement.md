# Real Tool-Surface Enforcement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a persona's `allowed_tools` declaration a real, harness-enforced restriction on what tools a launched Claude Code agent can use, instead of text the agent is merely told about.

**Architecture:** `inject_claude_settings_with_security` (`apps/ta-cli/src/commands/run.rs`) already writes a real `.claude/settings.local.json` that Claude Code itself enforces. Today its allow-list always starts from a hardcoded `DEFAULT_ALLOWED_TOOLS` constant and always merges in the user's own global `~/.claude/settings.json` allow entries, regardless of persona. This plan threads the persona's `allowed_tools` field through to that function as the base allow-list when declared (skipping the global-settings merge, which would otherwise silently widen a narrow persona back out), moves the hardcoded default into `SecurityProfile` so it's per-security-level configurable, and adds an optional hard allow-ceiling for defense in depth.

**Tech Stack:** Rust, `ta-goal::security` (`SecurityProfile`, `SecurityOverrides`), `ta-goal::persona` (`PersonaConfig`), `apps/ta-cli/src/commands/run.rs`.

## Global Constraints

- Feature branches + PRs only; never commit directly to `main` (this plan's branch: `feature/real-tool-surface-enforcement`, already created off `main`).
- Before every commit, all four must pass, run through the Nix devShell: `./dev "cargo build --workspace"`, `./dev "cargo test --workspace"`, `./dev "cargo clippy --workspace --all-targets -- -D warnings"`, `./dev "cargo fmt --all -- --check"`.
- Observability Mandate: every error/deny path states what happened, what was being attempted, and what to do about it; never a bare "denied" or "failed." Do not introduce a silent failure path (e.g. a persona-load failure at the settings-injection call site must surface, not be swallowed).
- No bare `.unwrap()`/`.expect()` outside test code.
- Commit in logical units; run `git status` after each commit and confirm "nothing to commit, working tree clean."
- Never disable or skip tests. Run tests after every code change, before committing.
- Source of truth for this plan: `docs/superpowers/specs/2026-10-06-real-tool-surface-enforcement-design.md`. Read in full before starting; cite it directly rather than re-deriving its reasoning.

---

### Task 1: `SecurityProfile` gains `default_allowed_tools` and `max_allowed_tools`

**Files:**
- Modify: `crates/ta-goal/src/security.rs` (`SecurityProfile` struct at line 168, `from_level()` at line 213, `SecurityOverrides` struct at line 345)
- Test: `crates/ta-goal/src/security.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing new from outside this file.
- Produces: `SecurityProfile.default_allowed_tools: Vec<String>` and `SecurityProfile.max_allowed_tools: Option<Vec<String>>`, both populated by `SecurityProfile::from_level(level, overrides)`. `SecurityOverrides.default_allowed_tools: Option<Vec<String>>` and `SecurityOverrides.max_allowed_tools: Option<Vec<String>>`. Task 2 reads `security_profile.default_allowed_tools` and `security_profile.max_allowed_tools` by these exact field names.

- [ ] **Step 1: Write the failing tests**

Find the existing `#[cfg(test)] mod tests` block in `crates/ta-goal/src/security.rs` (it already has tests for `from_level` and override merging) and add these:

```rust
    #[test]
    fn default_allowed_tools_populated_for_every_level() {
        for level in [SecurityLevel::Low, SecurityLevel::Mid, SecurityLevel::High] {
            let profile = SecurityProfile::from_level(level, &SecurityOverrides::default());
            assert!(
                !profile.default_allowed_tools.is_empty(),
                "default_allowed_tools must be populated for level {:?}",
                level
            );
        }
    }

    #[test]
    fn default_allowed_tools_override_replaces_level_preset() {
        let overrides = SecurityOverrides {
            default_allowed_tools: Some(vec!["mcp__ta__*".to_string()]),
            ..Default::default()
        };
        let profile = SecurityProfile::from_level(SecurityLevel::Low, &overrides);
        assert_eq!(profile.default_allowed_tools, vec!["mcp__ta__*".to_string()]);
    }

    #[test]
    fn max_allowed_tools_defaults_to_none_for_low_and_mid() {
        let low = SecurityProfile::from_level(SecurityLevel::Low, &SecurityOverrides::default());
        assert_eq!(low.max_allowed_tools, None);
        let mid = SecurityProfile::from_level(SecurityLevel::Mid, &SecurityOverrides::default());
        assert_eq!(mid.max_allowed_tools, None);
    }

    #[test]
    fn max_allowed_tools_override_is_applied_at_any_level() {
        let overrides = SecurityOverrides {
            max_allowed_tools: Some(vec!["mcp__ta__*".to_string(), "Read(*)".to_string()]),
            ..Default::default()
        };
        let profile = SecurityProfile::from_level(SecurityLevel::High, &overrides);
        assert_eq!(
            profile.max_allowed_tools,
            Some(vec!["mcp__ta__*".to_string(), "Read(*)".to_string()])
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `./dev "cargo test -p ta-goal --lib default_allowed_tools_populated_for_every_level default_allowed_tools_override_replaces_level_preset max_allowed_tools_defaults_to_none_for_low_and_mid max_allowed_tools_override_is_applied_at_any_level"`

Expected: compile errors (`default_allowed_tools`/`max_allowed_tools` don't exist on `SecurityProfile`/`SecurityOverrides` yet).

- [ ] **Step 3: Add `DEFAULT_ALLOWED_TOOLS` to `security.rs` as the single source of truth**

In `crates/ta-goal/src/security.rs`, add this near `DEFAULT_MID_FORBIDDEN_TOOLS` (line 198):

```rust
/// Tools allowed in the injected Claude Code settings for the `Low` security
/// level (today's unrestricted default). This is the single source of truth
/// for this list. `apps/ta-cli/src/commands/run.rs` reads it via
/// `SecurityProfile.default_allowed_tools` rather than keeping its own copy,
/// so the list cannot drift between the two crates.
pub const DEFAULT_ALLOWED_TOOLS: &[&str] = &[
    "Bash(*)",
    "Read(*)",
    "Write(*)",
    "Edit(*)",
    "MultiEdit(*)",
    "Glob(*)",
    "Grep(*)",
    "WebFetch(*)",
    "WebSearch(*)",
    "NotebookEdit(*)",
    "Task(*)",
    "Skill(*)",
    "TodoRead(*)",
    "TodoWrite(*)",
    "mcp__ta__*",
];
```

- [ ] **Step 4: Add the two new fields to `SecurityProfile` and `SecurityOverrides`**

In `SecurityProfile` (line 168), add after `web_search_enabled: bool,` (line 193):

```rust
    /// Fallback tool allow-list used when a launch has no persona-declared
    /// `allowed_tools` at all. Configurable per security level so a future
    /// `Mid`/`High` preset can narrow it without touching the `Low` default.
    pub default_allowed_tools: Vec<String>,

    /// Optional hard ceiling on the tool allow-list, applied as an
    /// intersection on top of whichever base list (persona's own
    /// declaration, or `default_allowed_tools`) was chosen. `None` means no
    /// additional ceiling beyond the existing `forbidden_tool_patterns` deny
    /// list. Unset by default for every level; set via an explicit
    /// `SecurityOverrides.max_allowed_tools` override.
    pub max_allowed_tools: Option<Vec<String>>,
```

In `SecurityOverrides` (line 345), add after the last existing field (find it by reading the struct, then follow its exact existing pattern):

```rust
    /// Override the fallback allow-list used when no persona declares its
    /// own `allowed_tools`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_allowed_tools: Option<Vec<String>>,

    /// Set a hard intersection ceiling on every launch's tool allow-list,
    /// regardless of persona declaration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_allowed_tools: Option<Vec<String>>,
```

- [ ] **Step 5: Populate both fields in `from_level()` and apply overrides**

In `from_level()` (line 213), after the existing `forbidden_tool_patterns` block (around line 304-315) and before the `Self { ... }` construction (line 317), add:

```rust
        // default_allowed_tools: same baseline for every level today (YAGNI:
        // narrowing Mid/High's own default is a future, separate decision;
        // this plan only requires the field to exist and be configurable).
        let mut default_allowed_tools: Vec<String> = DEFAULT_ALLOWED_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect();
        if let Some(ref v) = overrides.default_allowed_tools {
            default_allowed_tools = v.clone();
        }

        let max_allowed_tools = overrides.max_allowed_tools.clone();
```

Then add both fields to the `Self { ... }` construction (line 317-326):

```rust
        Self {
            level,
            sandbox_enabled,
            forbidden_tool_patterns,
            approval_required,
            audit_mode,
            constitution_block_mode,
            secret_scan_mode,
            web_search_enabled,
            default_allowed_tools,
            max_allowed_tools,
        }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `./dev "cargo test -p ta-goal --lib"`

Expected: all pass, including the four new tests and every pre-existing `ta-goal` test (no regressions from adding two new struct fields).

- [ ] **Step 7: Commit**

```bash
git add crates/ta-goal/src/security.rs
git commit -m "Add default_allowed_tools/max_allowed_tools to SecurityProfile

Moves DEFAULT_ALLOWED_TOOLS into ta-goal as the single source of
truth for the Low-level fallback allow-list, makes it per-security-
level configurable via SecurityOverrides, and adds an optional hard
allow-ceiling (max_allowed_tools) for defense in depth."
```

---

### Task 2: Persona's `allowed_tools` becomes the real allow-list, with the global-settings-merge correctly skipped

**Files:**
- Modify: `apps/ta-cli/src/commands/run.rs` (`inject_claude_settings_with_security` at line 6594, its call site at line 3409, `DEFAULT_ALLOWED_TOOLS`'s local definition at line 6525)
- Test: `apps/ta-cli/src/commands/run.rs` (inline `#[cfg(test)] mod tests`, near the existing `inject_claude_settings` tests at lines 10455/10777/10798/10840)

**Interfaces:**
- Consumes: `ta_goal::SecurityProfile.default_allowed_tools`/`max_allowed_tools` (Task 1), `ta_goal::PersonaConfig::load(project_root, name) -> anyhow::Result<PersonaConfig>` and `.capabilities.allowed_tools: Vec<String>` (both already exist, confirmed in `crates/ta-goal/src/persona.rs`).
- Produces: `inject_claude_settings_with_security(staging_path, source_dir, extra_deny, web_search_enabled, persona_allowed_tools: &[String])`, the new parameter. `fn intersect_allowed_tools(base: &[String], ceiling: &[String]) -> Vec<String>`, a new pure helper function, used by the call site, not by `inject_claude_settings_with_security` itself (per the design doc's separation of concerns: that function's job is "write a final list," not "know about posture ceilings").

- [ ] **Step 1: Write the failing tests**

Add these to the existing `mod tests` block in `apps/ta-cli/src/commands/run.rs`, near the existing `inject_claude_settings` tests:

```rust
    #[test]
    fn intersect_allowed_tools_keeps_only_the_overlap() {
        let base = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let ceiling = vec!["B".to_string(), "C".to_string(), "D".to_string()];
        let result = intersect_allowed_tools(&base, &ceiling);
        assert_eq!(result, vec!["B".to_string(), "C".to_string()]);
    }

    #[test]
    fn persona_allowed_tools_becomes_the_exact_allow_list_with_no_global_merge() {
        let staging = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        // Global settings.json that, if merged, would leak Bash(*) into a
        // narrow persona's allow-list: this is exactly the bug this task
        // fixes. Point $HOME at an isolated temp dir so this test never
        // reads the real developer's global settings.
        let claude_dir = home.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.json"),
            r#"{"permissions": {"allow": ["Bash(*)"]}}"#,
        )
        .unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", home.path());

        let persona_tools = vec!["mcp__ta__ta_fs_read".to_string()];
        inject_claude_settings_with_security(staging.path(), None, &[], true, &persona_tools)
            .unwrap();

        if let Some(h) = original_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }

        let settings = std::fs::read_to_string(staging.path().join(SETTINGS_REL_PATH)).unwrap();
        assert!(settings.contains("mcp__ta__ta_fs_read"));
        assert!(
            !settings.contains("Bash(*)"),
            "persona's narrow allow-list must not be widened by the user's own global \
             settings.json, got: {}",
            settings
        );
    }

    #[test]
    fn empty_persona_allowed_tools_preserves_existing_default_behavior() {
        let staging = TempDir::new().unwrap();
        inject_claude_settings_with_security(staging.path(), None, &[], true, &[]).unwrap();
        let settings = std::fs::read_to_string(staging.path().join(SETTINGS_REL_PATH)).unwrap();
        // Unchanged from today: the broad default list is present.
        assert!(settings.contains("Bash(*)"));
        assert!(settings.contains("Read(*)"));
    }

    #[test]
    fn extra_deny_applies_regardless_of_which_allow_list_base_was_used() {
        let staging_default = TempDir::new().unwrap();
        inject_claude_settings_with_security(
            staging_default.path(),
            None,
            &["Bash(*rm -rf*)".to_string()],
            true,
            &[],
        )
        .unwrap();
        let settings_default =
            std::fs::read_to_string(staging_default.path().join(SETTINGS_REL_PATH)).unwrap();
        assert!(settings_default.contains("Bash(*rm -rf*)"));

        let staging_persona = TempDir::new().unwrap();
        inject_claude_settings_with_security(
            staging_persona.path(),
            None,
            &["Bash(*rm -rf*)".to_string()],
            true,
            &["mcp__ta__ta_fs_read".to_string()],
        )
        .unwrap();
        let settings_persona =
            std::fs::read_to_string(staging_persona.path().join(SETTINGS_REL_PATH)).unwrap();
        assert!(settings_persona.contains("Bash(*rm -rf*)"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `./dev "cargo test -p ta-cli --lib intersect_allowed_tools_keeps_only_the_overlap persona_allowed_tools_becomes_the_exact_allow_list_with_no_global_merge empty_persona_allowed_tools_preserves_existing_default_behavior extra_deny_applies_regardless_of_which_allow_list_base_was_used"`

Expected: compile errors: `intersect_allowed_tools` doesn't exist yet, and `inject_claude_settings_with_security` doesn't take a 5th argument yet.

- [ ] **Step 3: Remove the local `DEFAULT_ALLOWED_TOOLS` and reference `ta_goal`'s copy instead**

In `apps/ta-cli/src/commands/run.rs`, delete the local `DEFAULT_ALLOWED_TOOLS` constant (lines 6524-6544, the one starting `const DEFAULT_ALLOWED_TOOLS: &[&str] = &[` through its closing `];`). Any other reference to this constant in the file (grep for `DEFAULT_ALLOWED_TOOLS` to find all of them) should be updated to use the value passed in at the call site instead (see Step 5): there should be exactly one other reference, inside `inject_claude_settings_with_security` itself, which Step 4 rewrites anyway.

- [ ] **Step 4: Add the `intersect_allowed_tools` helper and update `inject_claude_settings_with_security`'s signature**

Add this function near `inject_claude_settings_with_security` (before it, so it's defined before use is unnecessary in Rust but keep it readable to place it just above):

```rust
/// Intersect a base allow-list with a posture-level ceiling, preserving
/// `base`'s own ordering. Used to apply `SecurityProfile.max_allowed_tools`
/// as a hard cap on top of whichever allow-list (a persona's own
/// declaration, or the level's `default_allowed_tools` fallback) was chosen
/// Never broader than either input.
fn intersect_allowed_tools(base: &[String], ceiling: &[String]) -> Vec<String> {
    base.iter()
        .filter(|t| ceiling.contains(t))
        .cloned()
        .collect()
}
```

Change `inject_claude_settings_with_security`'s signature (line 6594) to:

```rust
fn inject_claude_settings_with_security(
    staging_path: &Path,
    source_dir: Option<&Path>,
    extra_deny: &[String],
    web_search_enabled: bool,
    persona_allowed_tools: &[String],
) -> anyhow::Result<()> {
```

And update the allow-list construction (the block starting `let mut allow: Vec<String> = DEFAULT_ALLOWED_TOOLS` at line ~6651) to:

```rust
    let use_persona_allowlist = !persona_allowed_tools.is_empty();
    let mut allow: Vec<String> = if use_persona_allowlist {
        persona_allowed_tools
            .iter()
            .filter(|t| web_search_enabled || !t.starts_with("WebSearch"))
            .map(|s| format!("\"{}\"", s))
            .collect()
    } else {
        ta_goal::security::DEFAULT_ALLOWED_TOOLS
            .iter()
            .filter(|t| web_search_enabled || !t.starts_with("WebSearch"))
            .map(|s| format!("\"{}\"", s))
            .collect()
    };
```

(Confirmed: `crates/ta-goal/src/lib.rs:33` already has `pub mod security;`, so `ta_goal::security::DEFAULT_ALLOWED_TOOLS` is directly reachable with no new re-export needed. `crates/ta-goal/src/lib.rs:61-64` separately re-exports `SecurityProfile`/`SecurityLevel`/etc. at the crate root, which is why those appear elsewhere in this file as `ta_goal::SecurityProfile`. `DEFAULT_ALLOWED_TOOLS` is not in that re-export list, so use the full `ta_goal::security::DEFAULT_ALLOWED_TOOLS` path for it specifically, rather than assuming it's also at the crate root.)

Then wrap the entire "Merge all allow entries from global settings" block (the `if let Ok(home) = std::env::var("HOME") { ... }` block that reads `~/.claude/settings.json` and pushes into `allow`) in a condition so it only runs when there's no persona override:

```rust
    let mut global_default_mode: Option<String> = None;
    let mut global_skip_dangerous: Option<bool> = None;
    if !use_persona_allowlist {
        if let Ok(home) = std::env::var("HOME") {
            // ... existing body unchanged ...
        }
    }
```

(The existing body inside that `if let Ok(home) = ...` block, reading the global settings file, inheriting `defaultMode`/`skipDangerousModePermissionPrompt`, and merging allow entries, stays exactly as it is today; only the new `if !use_persona_allowlist` wrapper around the whole block is new.)

- [ ] **Step 5: Wire the call site to load the persona and compute the final allow-list**

At the call site (`run.rs:3409`), replace:

```rust
        inject_claude_settings_with_security(
            &staging_path,
            source,
            &security_profile.forbidden_tool_patterns,
            security_profile.web_search_enabled,
        )?;
```

with:

```rust
        let persona_allowed_tools: Vec<String> = match persona_name {
            Some(pname) => match ta_goal::PersonaConfig::load(&config.workspace_root, pname) {
                Ok(persona) => persona.capabilities.allowed_tools,
                Err(e) => {
                    anyhow::bail!(
                        "Could not load persona '{}' while preparing tool-surface \
                         restrictions: {}. Check .ta/personas/{}.toml exists.",
                        pname,
                        e,
                        pname
                    );
                }
            },
            None => Vec::new(),
        };

        let base_allowed_tools = if persona_allowed_tools.is_empty() {
            security_profile.default_allowed_tools.clone()
        } else {
            persona_allowed_tools
        };
        let final_allowed_tools = match &security_profile.max_allowed_tools {
            Some(ceiling) => intersect_allowed_tools(&base_allowed_tools, ceiling),
            None => base_allowed_tools,
        };
        // Pass final_allowed_tools as the persona override whenever a persona
        // declared a non-empty list OR a max_allowed_tools ceiling narrowed
        // the default. In both cases the caller has already resolved the
        // intended list, so inject_claude_settings_with_security should use
        // it verbatim rather than re-deriving its own default.
        let effective_persona_tools: &[String] =
            if !persona_allowed_tools.is_empty() || security_profile.max_allowed_tools.is_some() {
                &final_allowed_tools
            } else {
                &[]
            };

        inject_claude_settings_with_security(
            &staging_path,
            source,
            &security_profile.forbidden_tool_patterns,
            security_profile.web_search_enabled,
            effective_persona_tools,
        )?;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `./dev "cargo test -p ta-cli --lib intersect_allowed_tools_keeps_only_the_overlap persona_allowed_tools_becomes_the_exact_allow_list_with_no_global_merge empty_persona_allowed_tools_preserves_existing_default_behavior extra_deny_applies_regardless_of_which_allow_list_base_was_used"`

Expected: all four pass.

- [ ] **Step 7: Update the other two call sites that already exist (test-only wrapper and its own callers)**

`inject_claude_settings` (the `#[cfg_attr(not(test), allow(dead_code))]` test-only wrapper at line 6586) calls `inject_claude_settings_with_security(staging_path, source_dir, &[], true)`. Update this call to add the new 5th argument: `inject_claude_settings_with_security(staging_path, source_dir, &[], true, &[])`. Its own callers (the four existing tests at the line numbers given in this task's **Files** section) need no changes themselves, since they call the unchanged-arity `inject_claude_settings` wrapper, not the 5-argument function directly.

- [ ] **Step 8: Run the full relevant suite**

Run: `./dev "cargo test -p ta-cli --lib"` and `./dev "cargo test -p ta-goal --lib"`

Expected: all pass, including every pre-existing test in both crates (confirms the signature change didn't break anything using the old 4-argument call, and confirms Task 1's `SecurityProfile` changes still hold).

- [ ] **Step 9: Commit**

```bash
git add crates/ta-goal/src/security.rs apps/ta-cli/src/commands/run.rs
git commit -m "Thread persona.allowed_tools into the real Claude Code settings injection

A persona's declared allowed_tools becomes the actual, harness-
enforced allow-list when non-empty, used verbatim instead of the
default -- and critically, the user's own global ~/.claude/settings.json
allow entries are no longer merged on top in that case, since doing so
would silently widen a narrow persona's declaration right back out.
Empty persona_allowed_tools (today's state for every existing persona)
preserves exact current behavior. Also wires the optional
max_allowed_tools ceiling as an intersection on top of whichever base
list was chosen."
```

---

## Self-Review

**Spec coverage:** design doc point 1 (persona allow-list real + global-merge-skip correctness fix) → Task 2. Point 2 (configurable per-level default) → Task 1. Point 4 (optional hard allow-ceiling) → Task 1 (data) + Task 2 Step 5 (applied at the call site, per the design doc's explicit separation-of-concerns instruction that `inject_claude_settings_with_security` itself should not know about posture ceilings).

**Placeholder scan:** none. Every step has complete code, including the exact global-settings-merge wrapping that a naive implementation would miss.

**Type consistency:** `inject_claude_settings_with_security`'s new 5th parameter (`persona_allowed_tools: &[String]`) is the same type and name used consistently across Task 2's every step and test. `SecurityProfile.default_allowed_tools`/`max_allowed_tools` (Task 1) are read by their exact field names at the Task 2 call site.

**The three things most likely to go wrong, named explicitly for the implementer:**
1. The global-settings-merge skip (Task 2 Step 4's `if !use_persona_allowlist` wrapper) is the one place a naive reading of "add a parameter and use it for the base list" would compile, look correct, and pass a shallow test, while silently NOT closing the real gap this plan exists to close. Task 2's own test (`persona_allowed_tools_becomes_the_exact_allow_list_with_no_global_merge`) specifically catches this by writing a global settings file that would leak `Bash(*)` in if the skip isn't implemented.
2. `DEFAULT_ALLOWED_TOOLS`'s two-copies-drifting-apart risk is closed by Task 1 Step 3 defining it once in `ta-goal` and Task 2 Step 3 deleting the local copy in `apps/ta-cli`, not just adding a second list that happens to match today.
3. The empty-persona-tools regression guard (`empty_persona_allowed_tools_preserves_existing_default_behavior`) is a real, separate test, not an assumption folded into another test's assertions.
