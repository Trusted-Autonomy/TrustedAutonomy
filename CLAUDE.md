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
- **Before any `git checkout`, `git commit`, or `git push`**: check for `.ta/apply.lock`. If it exists and the PID inside is alive, a `ta draft apply` is in progress — wait for it to finish before touching the git repo. Doing git operations mid-apply causes "no changes to commit" rollbacks. (v0.15.11.1 will enforce this at the TA level; until then, enforce manually.)
- Never disable or skip tests
- Run tests after every code change, before committing
- Always run `cargo fmt --all -- --check` before every `git push`
- Commit in logical working units
- All work stays within ~/development/TrustedAutonomy/
- Use `tempfile::tempdir()` for all test fixtures that need filesystem access

## Release Rules

- **Never manually create a GitHub release before CI runs.** Once a tag is used in a published release, GitHub permanently locks that tag — even if you delete the release and the tag. The only recovery is a new tag name. Let the `push: tags: v*` workflow trigger handle release creation.
- **To retrigger a release**: use `workflow_dispatch` with the tag input — do not delete and recreate releases or tags manually.
- **If a release workflow fails after assets uploaded**: the assets may be on a draft release. Check with `gh release view <tag> --json draft,assets`. If all assets are present and `draft: true`, publish it with `gh release edit <tag> --draft=false` rather than rerunning from scratch.
- **Tag naming**: use `v<semver>` for production releases (e.g. `v0.13.0`). The release workflow auto-triggers on `v*` tags. Non-`v*` tags (e.g. `public-alpha-v0.12.8`) require a manual `workflow_dispatch` trigger.

## Deferred Items Policy

Completed phases must not contain open "Remaining (deferred)" lists. When finishing a phase:

1. **Review every planned item** — verify each is done, partially done, or not started.
2. **If an item is not done**, discuss it with the user before closing the phase:
   - Is it still needed?
   - Which future phase should own it?
   - Should it be dropped?
3. **Move items to their target phase** with a one-line note (e.g., `→ v0.11.4`).
4. **Replace the "Remaining" section** with "Deferred items moved/resolved" showing where each item went or that it was completed in another phase.
5. **Never leave unchecked `[ ]` items in a `done` phase.** Every item is either checked `[x]`, moved to a named future phase, or explicitly dropped with rationale.

## Observability Mandate

All outcomes must be **observable** (with details and logging) and **actionable** (user knows what to do next). This applies to every error path, timeout, status message, and user-facing output.

- **Error messages**: Always include what happened, what was being attempted, and what the user can do about it. Never return bare "Error" or "failed" without context.
- **Timeouts**: State which operation timed out, what the timeout duration was, and how to configure it.
- **CLI output**: Commands should confirm what they did, not just succeed silently. Include counts, paths, and IDs where relevant.
- **Daemon/API errors**: Include the command or endpoint, relevant IDs, and suggest next steps.
- **Logging**: Use `tracing::warn`/`tracing::error` for operational issues. Include structured fields (command, duration, path) not just messages.
- **Scripts**: Print what step is running, what binary is being used, and on failure, print the exact command that can be re-run manually.

## Current State

**Current version**: `0.15.19-alpha`
- See **PLAN.md** for the canonical development roadmap with per-phase status
- `ta plan list` / `ta plan status` show current progress
- Goals can link to plan phases: `ta run "title" --phase 4b`
- `ta draft apply` auto-updates PLAN.md when a phase completes

## Version Management

**Single source of truth**: `Cargo.toml [workspace.package] version`. Everything else is derived from it.

**Version bumping is automatic.** When `ta draft apply --phase <id>` runs, the system auto-bumps `Cargo.toml`, `CLAUDE.md`, and `.release.toml` to the semver derived from the phase ID. **Agents must NOT manually set the version** — the system is the authority.

Phase ID → semver mapping:
- `v0.15.0` → `0.15.0-alpha`
- `v0.15.13.2` → `0.15.13-alpha.2`

CI (`version-check` job) enforces that `Cargo.toml` and `CLAUDE.md` agree — a mismatch is a build failure.

**Manual override** (release pinning, re-alignment): use `./scripts/bump-version.sh` which updates all locations atomically:
```bash
./scripts/bump-version.sh 0.14.22-rc.5 --last-tag public-alpha-v0.14.22.4
```

`last_release_tag` and `title_suffix` in `.release.toml` are human-controlled via `bump-version.sh` — updated when publishing a release, not on every phase.

   **Anti-regression rule scope**: Agents must not set the version to a value that would skip or re-order plan phases. Human-initiated version changes (e.g., re-aligning after divergence, pinning for a public release) are permitted with an explicit commit message explaining the intent.

4. **`PLAN.md`**: Mark the phase `<!-- status: done -->` (done automatically by `ta draft apply --phase`)
5. **`docs/USAGE.md`**: Update with any new commands, flags, config options, or workflow changes. USAGE.md is the user onboarding guide — write feature documentation as "how to" sections, not version-annotated changelogs. Keep version references out of feature descriptions (use the Roadmap section for version tracking). When adding a new workflow or command, add it to the appropriate section with a clear code example.

Version format: `MAJOR.MINOR.PATCH-alpha` (semver). See `PLAN.md` "Versioning & Release Policy" for the full mapping of phases to versions. Sub-phases use pre-release dot notation: `v0.4.1.2` → `0.4.1-alpha.2`.

### Plan Phase Numbers vs Binary Semver

Plan phase IDs and the binary semver are **the same track** — the version in `Cargo.toml` must always match the last completed plan phase. Sub-phases map as: completing `v0.13.17.2` → set `version = "0.13.17-alpha.2"`; completing `v0.13.17.3` → set `version = "0.13.17-alpha.3"`; completing `v0.14.0` → set `version = "0.14.0-alpha"`.

Semver requires exactly three numeric components (MAJOR.MINOR.PATCH). Four-part plan phases (`v0.13.17.3`) map to three-part semver plus a pre-release identifier (`0.13.17-alpha.3`).

**Resolved divergence**: v0.14.0–v0.14.2 were previously implemented before completing v0.13.17.x, leaving the binary ahead of the plan. This has been corrected: binary is now at `0.13.17-alpha.2` (human-initiated re-alignment, 2026-03-23). Going forward, the version must not advance past the current plan phase.

- Do not start a higher phase (e.g., `v0.14.3`) if lower phases (`v0.13.17.x`) still have `<!-- status: pending -->` markers. A guard for this is planned (`ta plan status --check-order`, v0.14.3).

### Public Release Process (after v0.13.17.3)

```bash
# 1. Verify all v0.13.17.x phases are marked done in PLAN.md
ta plan status

# 2. Pin version for the release
# Edit Cargo.toml [workspace.package]: version = "0.13.17-alpha.3"
# Edit CLAUDE.md: Current version = 0.13.17-alpha.3
git commit -m "chore: pin version to 0.13.17-alpha.3 for public release"

# 3. Build, test, install
./dev cargo test --workspace
./dev cargo build --release --workspace
bash install_local.sh

# 4. Trigger release workflow (creates GitHub release + uploads assets)
git tag public-alpha-v0.13.17.3
git push origin public-alpha-v0.13.17.3

# 5. Re-bump for ongoing development (next plan phase after 0.13.17.3)
# Edit Cargo.toml [workspace.package]: version = "0.13.17-alpha.4"
# Edit CLAUDE.md: Current version = 0.13.17-alpha.4
git commit -m "chore: bump version to 0.13.17-alpha.4 for post-release development"
git push
```

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

