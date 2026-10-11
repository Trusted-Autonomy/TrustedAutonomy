# Contributing to Trusted Autonomy

Thank you for your interest in contributing to Trusted Autonomy! This document provides guidelines for development, testing, and releases.

## Development Setup

### Prerequisites

- **Nix** (recommended): Provides reproducible dev environment
- **Rust**: 1.70+ if not using Nix
- **Git**: For version control

### Getting Started

1. **Clone the repository**
   ```bash
   git clone https://github.com/trustedautonomy/ta.git
   cd ta
   ```

2. **Enter the development environment**
   ```bash
   nix develop  # Recommended
   # OR install Rust toolchain manually
   ```

3. **Run the verification suite**
   ```bash
   ./dev cargo build --workspace
   ./dev cargo test --workspace
   ./dev cargo clippy --workspace --all-targets -- -D warnings
   ./dev cargo fmt --all -- --check
   ```

## Git Workflow

**All work MUST happen on feature branches. Never commit directly to `main`.**

1. **Create a feature branch**
   ```bash
   git checkout -b feature/<short-description>
   ```
   Use prefixes: `feature/`, `fix/`, `refactor/`, `docs/` as appropriate.

2. **Commit to the feature branch** in logical working units as you go.

3. **When complete**, push and open a pull request:
   ```bash
   git push -u origin feature/<short-description>
   gh pr create --title "Short description" \
     --body "## Summary\n- what changed and why\n\n## Test plan\n- verification steps"
   ```

4. **The PR is reviewed and merged** into `main` (squash or merge commit).

## Testing

### Running Tests

```bash
# All tests
./dev cargo test --workspace

# Specific crate
./dev cargo test -p ta-workspace

# With output
./dev cargo test --workspace -- --nocapture
```

### Test Requirements

- **All tests must pass** before committing
- **Never disable or skip tests** without explicit justification in the PR
- Use `tempfile::tempdir()` for filesystem-based tests
- Tests should be deterministic and isolated

### Hermetic tests and the leftover-process check

A unit test must never run the real `gh`, `ta` or `ta-daemon`. On Windows,
`cargo test` puts `target\debug` on `PATH`, so a test that spawns one of them
by name runs the freshly built binary, and anything it leaves behind keeps
running after the test ends. A Windows CI run once ended with about 7,000
orphaned `gh` processes and a leftover `ta-daemon`.

How to keep a test hermetic:

- **Inject the command.** Code that shells out takes the program or the whole
  call as a parameter, and the test passes a stub. Existing hooks:
  `GitAdapter::with_gh_program(path)` (a nonexistent path behaves like a
  machine without `gh`), `ta_workflow::step_executor::execute_pr_monitor_tick_with`
  (injected CI poller), `doctor::execute_with_gh_check`, and
  `daemon::start_with_binary` (injected daemon lookup).
- **Or put a stub first on `PATH`** for a child process you start yourself:
  write a `gh` script into a `tempfile::tempdir()` (see
  `crates/ta-submit/tests/git_open_review.rs`), `cfg(unix)` because it is a
  shell script.
- **Reap every child.** Wait on it, or kill its whole tree on a timeout with
  `ta_goal::process::kill_process_tree`. A test that starts a daemon holds it
  in a guard whose `Drop` kills it, and uses an ephemeral port.

CI enforces this. The last step of each required job
(`scripts/ci-leftover-processes.sh`, or `.ps1` on Windows) lists leftover
`gh`, `ta-daemon` and `ta` processes and fails when there is any `ta-daemon`
or more than 5 `gh` or `ta` processes. It runs even if an earlier step failed.
Run it locally after a test run (on a development machine your own running
`ta-daemon` and `ta` sessions count too, so read the list rather than the exit
code):

```bash
./dev cargo test --workspace
scripts/ci-leftover-processes.sh
```

If it fails, run the test binaries one at a time with a fake `gh` first on
`PATH` that logs its arguments, to find the test that reaches it.

The three channel plugins (`plugins/ta-channel-discord`, `-slack`, `-email`)
have their own `[workspace]` and are built only by their own CI step. Test one
locally with `./dev cargo test --manifest-path plugins/ta-channel-slack/Cargo.toml`.

## Code Quality

### Before Every Commit

Run these four checks (all must pass):

```bash
./dev cargo build --workspace
./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
./dev cargo fmt --all -- --check
```

### Linting

- Clippy warnings are treated as errors in CI
- Fix all warnings before submitting a PR
- Use `#[allow(clippy::...)]` sparingly and document why

### Formatting

- Use `rustfmt` for consistent code formatting
- Configuration in `rustfmt.toml` (if present)
- CI enforces formatting checks

## Release Process

### Version Numbering

Trusted Autonomy follows [Semantic Versioning](https://semver.org/):

- **MAJOR**: Incompatible API changes
- **MINOR**: Backward-compatible functionality additions
- **PATCH**: Backward-compatible bug fixes
- **Alpha/Beta**: Pre-release versions (e.g., `0.1.0-alpha`, `0.2.0-beta.1`)

### Creating a Release

**Prerequisites:**
- All CI checks passing on `main`
- All planned features/fixes merged
- CHANGELOG.md updated with release notes

**Steps:**

1. **Update version numbers**

   Update version in all relevant `Cargo.toml` files:
   ```bash
   # Update ta-cli/Cargo.toml
   version = "0.2.0"

   # Update workspace crates if needed
   ```

2. **Update CHANGELOG.md**

   Add release section with date:
   ```markdown
   ## [0.2.0] - 2026-02-13

   ### Added
   - Feature X with capability Y

   ### Changed
   - Improved Z performance

   ### Fixed
   - Bug in component W
   ```

3. **Commit version bump**
   ```bash
   git checkout -b release/v0.2.0
   git add Cargo.toml apps/ta-cli/Cargo.toml CHANGELOG.md Cargo.lock
   git commit -m "chore: bump version to 0.2.0"
   git push -u origin release/v0.2.0
   ```

4. **Open and merge release PR**
   ```bash
   gh pr create --title "Release v0.2.0" \
     --body "Prepare v0.2.0 release. See CHANGELOG.md for details."
   ```

   Get approval and merge to `main`.

5. **Create and push git tag**
   ```bash
   git checkout main
   git pull
   git tag -a v0.2.0 -m "Release v0.2.0"
   git push origin v0.2.0
   ```

6. **Automated release workflow**

   Pushing the tag triggers `.github/workflows/release.yml` which:
   - Builds binaries for all platforms (macOS x86_64/aarch64, Linux x86_64/aarch64)
   - Creates GitHub release with attached binaries
   - Publishes `ta-cli` to crates.io (requires `CARGO_REGISTRY_TOKEN` secret)

7. **Verify the release**

   - Check GitHub releases page: `https://github.com/trustedautonomy/ta/releases`
   - Verify binaries are attached and downloadable
   - Test installation via install script:
     ```bash
     curl -fsSL https://raw.githubusercontent.com/trustedautonomy/ta/main/install.sh | sh
     ```
   - Verify crates.io publication: `https://crates.io/crates/ta-cli`

### Hotfix Releases

For critical bugs in production:

1. Create branch from the release tag: `git checkout -b hotfix/v0.2.1 v0.2.0`
2. Fix the bug and test thoroughly
3. Update version to patch level (e.g., `0.2.1`)
4. Update CHANGELOG.md
5. Follow standard release process from step 3

### Pre-release (Alpha/Beta)

For testing unreleased features:

1. Use pre-release version: `0.3.0-alpha`, `0.3.0-beta.1`
2. Follow standard release process
3. GitHub release will be marked as "pre-release" automatically
4. Document known issues and testing scope in release notes

## CI/CD

### Continuous Integration

`.github/workflows/ci.yml` runs on every push and PR:
- Build all workspace crates
- Run all tests
- Clippy lint checks (warnings as errors)
- Formatting validation

### Release Pipeline

`.github/workflows/release.yml` triggers on version tags (`v*`):
- Cross-compilation for multiple targets
- Binary packaging (tar.gz with SHA256 checksums)
- GitHub release creation
- crates.io publishing

### Secrets Configuration

Required GitHub secrets:
- `CARGO_REGISTRY_TOKEN`: crates.io API token for publishing
- `CACHIX_AUTH_TOKEN`: (optional) Nix binary cache

## Documentation

- **README.md**: User-facing quick start and overview
- **PLAN.md**: Development roadmap with machine-parseable phase markers
- **docs/**: Architecture documentation and design docs
- **Code comments**: Focus on "why" not "what"

## Project Structure

```
trusted-autonomy/
├── apps/
│   └── ta-cli/          # Main CLI binary
├── crates/
│   ├── ta-audit/        # Audit log with hash chain
│   ├── ta-changeset/    # ChangeSet and PRPackage model
│   ├── ta-goal/         # Goal lifecycle and state machine
│   ├── ta-policy/       # Capability-based policy engine
│   ├── ta-workspace/    # Staging and overlay workspaces
│   └── ta-connectors/   # Filesystem and external connectors
├── .github/workflows/   # CI/CD pipelines
└── docs/                # Architecture and design docs
```

## Getting Help

- **Issues**: Report bugs or request features via GitHub Issues
- **Discussions**: Ask questions in GitHub Discussions
- **Documentation**: Read docs/ for architecture details

## Code of Conduct

Be respectful, constructive, and collaborative. We're building tools to empower humans, not replace them.
