# Releasing Trusted Autonomy

## One-Time Setup

### 1. GitHub Secrets

Go to **Settings > Secrets and variables > Actions** in your GitHub repo and add:

| Secret | Required | Purpose |
|---|---|---|
| `CARGO_REGISTRY_TOKEN` | No | Publish to crates.io. Get from [crates.io/settings/tokens](https://crates.io/settings/tokens) (scope: `publish-update`). If not set, the crates.io publish step is skipped. |

### 2. Verify CI Passes

Push a commit or PR to main and confirm the CI workflow (`.github/workflows/ci.yml`) passes on both Ubuntu and macOS.

---

## Release Process

All releases use `scripts/release.sh`. The script handles version bumping, verification, release notes, tagging, and pushing.

```bash
./scripts/release.sh 0.3.0-alpha
```

### What the script does

1. **Pre-flight checks**: clean working tree, on `main`, tag doesn't exist
2. **Collects commits** since last tag for release notes
3. **Generates release notes**: via `ta run` (agent-synthesized) if available, otherwise structured commit log. Shows draft and offers `$EDITOR` review.
4. **Bumps version** in all `Cargo.toml` files + `DISCLAIMER.md`
5. **Runs full verification**: `cargo build`, `cargo test`, `cargo clippy`, `cargo fmt --check`
6. **Commits** the version bump
7. **Creates annotated tag** with release notes embedded
8. **Prompts to push** — pushing the tag triggers the GitHub Actions release workflow

### What happens after push

The release workflow (`.github/workflows/release.yml`) runs automatically:

- Builds binaries for 4 targets: macOS aarch64, macOS x86_64, Linux x86_64 (musl), Linux aarch64 (musl)
- Creates a GitHub Release with binary tarballs, SHA256 checksums, USAGE.md, DISCLAIMER.md
- Generates HTML docs via pandoc (USAGE.html, DISCLAIMER.html)
- Publishes `ta-cli` to crates.io

### Post-release verification

```bash
# Check workflow status
gh run list --workflow=release.yml

# View the release
gh release view v0.3.0-alpha

# Test a downloaded binary
curl -fsSL https://github.com/trustedautonomy/ta/releases/latest/download/ta-v0.3.0-alpha-aarch64-apple-darwin.tar.gz | tar xz
./ta --version

# Test crates.io install (may take a few minutes to propagate)
cargo install ta-cli
```

### Windows smoke check: optimizer windows

CI cannot observe a console window, so check this by hand on a Windows build before publishing:

1. Install headroom (`pip install headroom-ai[all]`) and run `ta daemon restart`.
2. Confirm no console window opens for the optimizer process, and `ta compression status` shows `running`.
3. Break the optimizer (set `command` in `[compression.plugin]` to a script that exits 1) and restart the daemon. Confirm no window flashes, and that after 5 failures `ta compression status` shows `suspended` with a last failure.

---

## Versioning

TA follows semver: `MAJOR.MINOR.PATCH[-prerelease]`

- `0.2.2-alpha` — current pre-release
- `0.3.0-alpha` — next planned release
- `1.0.0` — first stable release

GitHub automatically marks releases containing `alpha` or `beta` as pre-releases.

---

## Troubleshooting

### Release workflow failed

```bash
gh run list --workflow=release.yml
gh run view <run-id> --log-failed
```

Common causes: expired `CARGO_REGISTRY_TOKEN`, cross-compilation failure, network timeout.

To retry: delete the failed release and tag, fix the issue, re-run.

```bash
gh release delete v0.3.0-alpha --yes
git tag -d v0.3.0-alpha
git push origin :refs/tags/v0.3.0-alpha
# Fix, then re-run release.sh
```

### crates.io "already uploaded"

Version already exists — bump the patch version and release again.

### Binary doesn't work

- Linux musl binaries are statically linked and work on most distros
- macOS binaries must match architecture (Intel vs Apple Silicon)
- Nix fallback: `nix run github:trustedautonomy/ta`

---

## Reference: crates.io Publishing

The release workflow publishes `ta-cli` only. Library crates remain workspace-internal for now.

**Publishing order** (if publishing libraries in the future):
1. `ta-audit`, `ta-policy`, `ta-changeset` (no internal deps)
2. `ta-workspace` (depends on ta-changeset)
3. `ta-goal` (depends on ta-changeset, ta-workspace)
4. `ta-mcp-gateway` (depends on ta-changeset, ta-policy)
5. `ta-connectors/*` (depend on ta-workspace, ta-changeset)
6. `ta-cli` (depends on everything)

**Manual publish** (if workflow fails):
```bash
cargo publish -p ta-cli --token $CARGO_REGISTRY_TOKEN
```

**Yank a bad version**:
```bash
cargo yank --vers 0.3.0-alpha -p ta-cli
```

## Reference: Homebrew (Future)

Homebrew distribution requires a tap repository (`trustedautonomy/homebrew-tap`) with a Ruby formula pointing to release binaries. Not yet set up — revisit after reaching stable releases. See git history for the full Homebrew setup guide if needed.
