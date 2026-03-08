# Claude Code Project Instructions

## Build Environment

Nix provides the Rust toolchain. **Always prefix cargo/just commands with the nix wrapper:**

```bash
export PATH="/nix/var/nix/profiles/default/bin:$HOME/.nix-profile/bin:$PATH"
nix develop --command bash -c "COMMAND_HERE"
```

Or use the helper script for one-liners:
```bash
./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
./dev just verify
```

## Verification Before Every Commit

Run these four checks (all must pass):
```bash
./dev cargo build --workspace
./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
./dev cargo fmt --all -- --check
```

## Git Workflow — Feature Branches + Pull Requests

All work MUST happen on feature branches. Never commit directly to `main`.

1. **Create a feature branch** before starting work:
   ```bash
   git checkout -b feature/<short-description>
   ```
   Use prefixes: `feature/`, `fix/`, `refactor/`, `docs/` as appropriate.

2. **Commit to the feature branch** in logical working units as you go.

3. **When the goal is complete**, push and open a pull request:
   ```bash
   git push -u origin feature/<short-description>
   gh pr create --title "Short description" --body "## Summary\n- what changed and why\n\n## Test plan\n- verification steps"
   ```

4. **The PR is reviewed and merged** into `main` (squash or merge commit).

This applies to both manual work and TA-mediated goals. When `ta pr apply --git-commit` runs, the commit should land on a feature branch, not `main`.

## Rules

- Never commit directly to `main` — always use a feature branch + PR
- Never disable or skip tests
- Run tests after every code change, before committing
- Commit in logical working units
- All work stays within ~/development/TrustedAutonomy/
- Use `tempfile::tempdir()` for all test fixtures that need filesystem access

## Observability Mandate

All outcomes must be **observable** (with details and logging) and **actionable** (user knows what to do next). This applies to every error path, timeout, status message, and user-facing output.

- **Error messages**: Always include what happened, what was being attempted, and what the user can do about it. Never return bare "Error" or "failed" without context.
- **Timeouts**: State which operation timed out, what the timeout duration was, and how to configure it.
- **CLI output**: Commands should confirm what they did, not just succeed silently. Include counts, paths, and IDs where relevant.
- **Daemon/API errors**: Include the command or endpoint, relevant IDs, and suggest next steps.
- **Logging**: Use `tracing::warn`/`tracing::error` for operational issues. Include structured fields (command, duration, path) not just messages.
- **Scripts**: Print what step is running, what binary is being used, and on failure, print the exact command that can be re-run manually.

## Current State

- **Current version**: `0.9.9-alpha.1`
- See **PLAN.md** for the canonical development roadmap with per-phase status
- `ta plan list` / `ta plan status` show current progress
- Goals can link to plan phases: `ta run "title" --phase 4b`
- `ta draft apply` auto-updates PLAN.md when a phase completes

## Version Management

When completing a phase, you MUST update versions as part of the work:

1. **`apps/ta-cli/Cargo.toml`**: Update `version` to the phase's target version (e.g., `"0.2.0-alpha"`)
2. **This file (`CLAUDE.md`)**: Update "Current version" above to match
3. **`PLAN.md`**: Mark the phase `<!-- status: done -->` (done automatically by `ta draft apply --phase`)
4. **`docs/USAGE.md`**: Update with any new commands, flags, config options, or workflow changes. USAGE.md is the user onboarding guide — write feature documentation as "how to" sections, not version-annotated changelogs. Keep version references out of feature descriptions (use the Roadmap section for version tracking). When adding a new workflow or command, add it to the appropriate section with a clear code example.

Version format: `MAJOR.MINOR.PATCH-alpha` (semver). See `PLAN.md` "Versioning & Release Policy" for the full mapping of phases to versions. Sub-phases use pre-release dot notation: `v0.4.1.2` → `0.4.1-alpha.2`.

### How It Works (Overlay Flow)
1. `ta goal start "title" --phase 4b` → copies project to `.ta/staging/`
2. `ta run "title" --phase 4b` → creates goal + injects CLAUDE.md (with plan context) + launches agent + builds draft on exit
3. Agent works normally in staging copy — TA is invisible to the agent
4. `ta draft build --latest` → diffs staging vs source → creates draft package with artifacts
5. `ta draft view/approve/deny <id>` → review workflow
6. `ta draft apply <id> --git-commit` → copies changes back to source + updates PLAN.md + optional git commit

### Key Types
- **Artifact.resource_uri**: `"fs://workspace/<path>"` — URI-based identity for all changes
- **PatchSet.target_uri**: Same URI scheme for external resources (gmail://, drive://, etc.)
- **DraftStatus**: Draft → PendingReview → Approved/Denied → Applied/Superseded/Closed
- **GoalRunState**: Created → Configured → Running → PrReady → UnderReview → Approved → Applied → Completed
- **GoalRun.plan_phase**: Optional link to a PLAN.md phase (e.g., "4b")
- **CLAUDE.md injection**: `ta run` prepends TA context + plan progress, saves backup, restores before diff
