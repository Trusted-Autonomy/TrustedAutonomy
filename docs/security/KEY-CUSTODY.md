# Key Custody: What TA Protects, Where, and What Is Not Checked (2026-11-03)

**Scope:** the secrets TA itself holds, where each one lives under each custody mode, who can read it, what an agent can and cannot see of it, how it is kept out of version control, and what CI does and does not verify. User-facing setup is in [docs/USAGE.md](../USAGE.md) ("Choosing where secrets are kept", "The credential vault key") and [CONTRIBUTING.md](../../CONTRIBUTING.md) ("Local setup: keys, keychain and custody"). This page is the evidence behind those instructions, and the list of what is not covered.

**Code reviewed:** branch `feature/dev-custody-file-default` (PR #742) at the time of writing. Line numbers refer to that branch and will drift.

**Method:** code reading, plus running the named unit tests. No exploit was run, and nothing here was checked against the real macOS Keychain, a real Linux Secret Service or the Windows Credential Manager. Where a claim rests on a test, the test is named. Where it rests only on reading code, it is labelled **UNTESTED**.

**Status labels:**
- **ENFORCED:** the code path was traced and a test (or a CI step) pins the behaviour.
- **UNTESTED:** the code path was traced, but no test or CI step pins it. It may regress silently.
- **GAP:** no safeguard exists. This page does not claim one.

---

## 1. Summary in plain language

- TA keeps five kinds of secret. Four are keys or credentials; the fifth, the vault file, is the encrypted store the vault key opens.
- A per-user switch, **custody**, decides where they live. `keychain` uses the OS keychain. `file` uses owner-only files (mode 0600) in a per-user state directory outside the project tree. Dev installs default to `file` because a self-signed binary has no Apple Team ID, so macOS re-prompts after every rebuild.
- Neither mode stops a program running as **your own OS user** from reading the secrets, unless that program is inside the OS sandbox. The sandbox is **off by default for a solo `ta run` worker** (section 4.3). Anything that says otherwise is wrong.
- The secrets are kept out of git by ignore rules and by a CI path check (section 5). Nothing stops you from committing a secret that is in an unusual place, or from forcing `git add -f`.
- There is **no key-rotation command** for the root key, the audit key or the vault key (section 7).

---

## 2. Custody modes

| Mode | Where secrets live | When it applies |
|---|---|---|
| `file` | Owner-only files, in the per-user state directory (section 3) | `TA_NO_KEYCHAIN` set; any cargo test binary; the custody file holds `file` |
| `keychain` | OS keychain items, with an owner-only file as a loud fallback when the keychain is unusable | The custody file holds `keychain`, or no custody file exists (the default) |

**Resolution order** (first match wins): `TA_NO_KEYCHAIN` set, then the cargo-test guard, then the custody file `~/.config/ta/human/custody` (or `$TA_USER_STATE_DIR/custody`) holding exactly `file` or `keychain`, then the default `keychain`. A custody file with any other content is ignored with a warning and the default applies. **ENFORCED:** `crates/ta-credentials/src/custody.rs:144-195` (`resolve_with`, `resolve`); the precedence table is unit-tested there with every input injected.

**Per-user switch, per-project vault key.** The custody mode is one value for the user. In `file` mode the vault key is a separate file for every project. In `keychain` mode it is a single item shared by all projects (`crates/ta-credentials/src/encryption.rs:124-175`; item names in `crates/ta-credentials/src/keychain.rs:33-40`). So switching mode does not move any secret by itself. `ta credentials custody file|keychain` copies a project's secrets across with read-back verification, never overwrites a different value, never deletes the old copy and prints cleanup commands (`custody.rs:362` `migrate`, `custody.rs:427` `switch`). It writes the custody file only after a clean migration. **Run it once in every project that has a `.ta/credentials.json`.** See the vault-key hazard in section 7.1.

**Why dev installs default to `file`.** A self-signed binary has no Team ID. macOS keychain access control then keys on the binary's cdhash, so every rebuild is a new identity and every keychain item prompts again. `install_local.sh` writes `file` to the custody file only when the binary has no Team ID, no custody file exists, and the keychain holds no TA items; it never overwrites an existing choice. **ENFORCED:** `scripts/custody-decision.sh`, exercised with a fake `codesign` and a temporary `HOME` by `scripts/test-custody-decision.sh`, which `apps/ta-cli/tests/custody_install_script.rs` runs under `cargo test`.

**`ta doctor` warns on the bad combination.** The "Key custody" check warns when the effective mode is `keychain` and the running `ta` has no Team ID (or the signature cannot be read). It runs `codesign` on the running binary and never reads the keychain, and it does nothing in `file` mode. **ENFORCED:** `apps/ta-cli/src/commands/doctor.rs:2623` (`check_keychain_custody_signing`); tests at `doctor.rs:2789`, `2826`, `2847`, which inject the parsed `codesign` output and the resolver result. The real `codesign` and the real keychain are not exercised by any test, and CI has no Team ID. **GAP:** no test or CI step covers real macOS signing behaviour.

---

## 3. The secrets

State directory per project: `~/Library/Application Support/TrustedAutonomy/projects/<project-id>/` (macOS), `$XDG_STATE_HOME/trusted-autonomy/projects/<project-id>/` (Linux, default `~/.local/state/...`), `%LOCALAPPDATA%\TrustedAutonomy\projects\<project-id>\` (Windows); `TA_STATE_DIR` overrides the root (`crates/ta-credentials/src/state.rs:8-13`). The `<project-id>` is minted once and recorded in the gitignored `<project>/.ta/project-id`. Control-plane files sit in its `secrets/` subdirectory. The human-credential directory is `~/.config/ta/human/`, or `$TA_USER_STATE_DIR` (`human.rs:89`).

| Secret | What it protects | In `keychain` mode | In `file` mode | Who can read it |
|---|---|---|---|---|
| **Capability-token root key** | Signs every biscuit capability token (`ta credentials grant`). Whoever holds it can mint any grant. | Service `trusted-autonomy-control-plane`, item `<project-id>/broker-root-key` | `<state>/secrets/broker-root-key`, 0600 | Your OS user; root. Not given to an agent by any code path found (section 4). |
| **Audit key** | HMAC key for the audit log and its signed checkpoints. Whoever holds it can re-sign a rewritten log. | Same service, item `<project-id>/audit-key` | `<state>/secrets/audit-key`, 0600 | As above. |
| **Vault key** (age identity) | Decrypts `.ta/credentials.json`. | One per-user item: service `trusted-autonomy-vault`, item `credential-vault-age-identity`. Falls back to the file below, with a warning, if the keychain is unusable. | `<state>/secrets/vault-key`, 0600, one per project | As above. |
| **Human credential** (`ta login`) | Lets a process act as the human: `ta draft approve/deny/apply` from an agent context requires it. | Service `trusted-autonomy-human`, item `human-credential` | `~/.config/ta/human/credential`, 0600, plus `credential.sha256` (digest) | As above, **and any process whose environment has `TA_HUMAN_CREDENTIAL`** (`human.rs:36,215-233`). |
| **Vault file** `credentials.json` | The service credentials you added with `ta credentials add` (GitHub tokens, API keys), age-encrypted. | `<project>/.ta/credentials.json`, 0600 | Same | Anyone who can read the file gets ciphertext; decrypting needs the vault key. |

Other files in the per-user directory: `custody` (the mode, 0600), `agent-context-seen`, `assertion-keys.json`. The Ed25519 outcome-signing and rules-owner keys also go through the same control-plane custody (`state.rs:509-514` lists `rules-owner-key`); they are outside this page's scope.

**File and directory modes.**
- Every secret file is created 0600: control-plane files by `write_bytes_private` (`state.rs:1235`), the human credential, digest and custody file by `write_private` (`human.rs:132`), the fallback key by `encryption.rs:306`, and the vault file by `file_vault.rs:113-114`. **ENFORCED:** tests `state_secret_files_are_mode_0600` (`state.rs:1470`, covers the root key only), `secret_and_digest_files_are_mode_0600` (`human.rs:978`), `file_fallback_key_is_chmod_0600` (`encryption.rs:386`). **UNTESTED:** the audit-key and vault-key files in `secrets/` go through the same function but have no mode assertion of their own, and the vault file's mode has no test I found.
- The vault file is written first and `chmod`ed afterwards, so it briefly has the umask's mode. Its contents are age-encrypted either way. **UNTESTED.**
- **Directories are created 0700 when TA creates them** (`human.rs:117` `create_private_dirs`, used at `state.rs:346`, `state.rs:1237` and `human.rs:138`; `custody.rs` tightens its own directory only if it was empty). A directory that already exists keeps its mode. This was added in this branch: before it, directories came from `create_dir_all` and took the default umask, typically 0755, so other users could list secret names. **ENFORCED:** `state_and_secrets_directories_are_mode_0700` (`state.rs:1494`), `login_creates_missing_state_directories_as_0700_and_leaves_existing_alone` (`human.rs:995`). `install_local.sh`'s custody directory is checked 0700 by `scripts/test-custody-decision.sh:47`.
- The project's `.ta/` directory is not created by custody code, so its mode is whatever the user's umask gave it. **GAP:** nothing sets or checks it. The vault file inside it is 0600.
- **Windows:** the Unix mode bits do not exist. TA relies on the state directory sitting under the user's profile, whose ACL already restricts access (`docs/USAGE.md`, "Key custody"). **UNTESTED:** no test asserts the ACL.

**A project cannot read another project's secrets by accident.** `ProjectState::open` refuses to run from a staging workspace (`state.rs:262-270`, test at `state.rs:1742`), so a goal's staging copy of the project does not get a state directory of its own. This is a code-level refusal, not a filesystem barrier.

---

## 4. What an agent can and cannot see

### 4.1 What it is handed

- **A capability token, never a key.** `SessionToken` has no secret field (`crates/ta-credentials/src/vault.rs:50-57`). `ta credentials grant` prints a signed biscuit holding the credential id, agent id, scopes and an expiry (`crates/ta-credential-broker/src/broker.rs:128-143`; `apps/ta-cli/src/commands/credentials.rs:617-630`). The root key is not in it. A holder can narrow the token but cannot widen it without the root key.
- **The token is a bearer token.** Presenting it to the shim or to the credential endpoint is enough; those paths do not check that the presenter is the agent it was issued to (`shim.rs:98`, `resolve_for_host`). The MCP `ta_external_action` path does compare the agent id (`crates/ta-mcp-gateway/src/tools/action.rs:853-863`). **GAP:** no agent-identity binding on the shim path.

### 4.2 When an agent does get a raw secret

This is the part that is easy to get wrong. **An agent is not always denied the raw vault value.**

- `ta run --credential-scopes ...` loads vault credentials into the agent's environment (`apps/ta-cli/src/commands/run.rs:6863-6884`, loader at `run.rs:6591`). For a credential **not** marked `broker_mediated = true` in `.ta/connectors.toml`, the code does `env.insert(cred.name, cred.value)`, which is the raw secret (`crates/ta-runtime/src/bare_process.rs:324`; the code calls it a "reduced-security fallback" and logs it at debug level only). **ENFORCED as behaviour** by `bare_process.rs:441-455`; this is by design, not a bug, but it means the agent holds the secret.
- For a `broker_mediated = true` credential only the biscuit goes into the agent's environment (`TA_SESSION_TOKEN_<name>`), never the secret. **ENFORCED:** `apply_credentials_to_env_withholds_raw_secret_for_broker_mediated_credential` (`bare_process.rs:514`).
- Without `--credential-scopes`, no vault credential is loaded for the agent, and the agent inherits the parent shell's full environment (`run.rs:6863`; flag documented in `apps/ta-cli/src/main.rs`, "legacy behavior of full environment inheritance"). Anything you exported in that shell, including `TA_HUMAN_CREDENTIAL`, is visible to the agent. Swarm sub-goals always get a narrowed environment.
- **Brokered secrets still reach a process in the agent's tree.** The git credential helper and the `gh` shim run as children of the agent's shell. They resolve the secret and hand it to git or to `gh` (`ta-credential-broker/src/shim.rs`, `credential_helper.rs`, `gh_shim.rs`). An agent can run `git credential fill` itself and read the answer. Nothing in TA prevents it. **GAP:** "broker-mediated" keeps the secret out of the agent's environment and files; it does not make it unobtainable by the agent.
- The secret is not written to the staged workspace (`.ta/` is always excluded from the staging overlay, `crates/ta-workspace/src/overlay.rs`), not logged by the injection path, and not put in argv.
- **GAP:** no test launches an agent and asserts that the vault secret is absent from its whole environment. The coverage is the unit tests above.

### 4.3 What the OS sandbox adds, and when it is on

The sandbox is default-on only for chat, chief-of-staff and team roles. A solo `ta run` worker is **not** sandboxed unless `[sandbox] enabled = true` or `[security] level = "mid"` or higher, because the default level is `low` (`crates/ta-goal/src/security.rs:27-31`; `docs/USAGE.md`, sandbox sections). Without the sandbox, a same-user agent can `cat` the 0600 key files and decrypt the vault. The code says so (`state.rs:22-27`, `human.rs:15-19`).

When the sandbox is on:
- It denies `<project>/.ta`, `<project>/.git`, the project's state directory and the `HOME_PROTECTED` list (`crates/ta-runtime/src/sandbox_launch.rs:882-891`, list at `:832`). **ENFORCED** for the state directory on macOS in CI by `crates/ta-runtime/tests/seatbelt_conformance.rs:153-171` (the `Seatbelt conformance` step of `.github/workflows/ci.yml`, macOS job only).
- **`~/.config/ta/human` was not on that list** until this branch. The `ta login` credential in `file` custody was therefore readable by a sandboxed agent on macOS, where the profile is a denylist. It is now the first `HOME_PROTECTED` entry (`sandbox_launch.rs:837`), pinned by `the_human_credential_directory_is_always_a_protected_path` (`sandbox_launch.rs:1545`). **UNTESTED:** that the macOS Seatbelt conformance probe denies this particular directory. The unit test proves it is in the deny list, and the conformance test proves listed paths are denied.
- It scrubs the environment to an allowlist, points `HOME` at a per-goal directory and drops every `TA_*` variable except five (`sandbox_launch.rs:1241`, `1325`; test `scrub_drops_secrets_and_control_plane_variables`, `sandbox_launch.rs:1869`). `TA_HUMAN_CREDENTIAL` is dropped. Model API keys such as `ANTHROPIC_API_KEY` still pass through. **GAP:** a sandboxed agent still holds the model API key.
- **The macOS profile is a denylist.** Reads outside the listed paths stay allowed. Linux (bubblewrap) mounts only what it is given; Windows relies on the AppContainer. Those are different guarantees, and I verified only the macOS one in CI.

### 4.4 Other things an agent can read

- `ta_fs_read` is not secret-aware. It is path-confined, but an agent can read `<project>/.ta/credentials.json` and gets age ciphertext (`crates/ta-connectors/fs/src/connector.rs`; only `.ta/build-cache` is refused). The vault key is not in `.ta/`, so the ciphertext alone does not decrypt. A legacy in-tree `credentials.key` is still read with a warning under the default posture and refused under `strict` (`state.rs:79`, `state.rs:956`), and `ta doctor` flags it.
- Redaction exists for audit artifacts (transcripts, the recorded tool-call list, wake-listener logs; `crates/ta-mcp-gateway/src/secret_redact.rs:67`). **GAP:** it is not applied to live `ta_fs_read` output or to `ta_external_action` responses.

**Rating.** Against a same-user, unsandboxed agent the human credential is a speed bump, not a wall (`human.rs:15-19`). With the sandbox on, on macOS, the state directory and `~/.config/ta/human` are denied; the model API key, and any credential you chose to inject, are not.

---

## 5. Keeping secrets out of version control

- **This repository's `.gitignore`** lists `.ta/credentials.json`, `.ta/*.key`, `.ta/*.pem`, `.ta/*.cred`, `.ta/broker_root.key`, `.ta/audit.key`, `.ta/credentials.key`, `.ta/daemon-tokens.json`, `.ta/tokens/` and `.env.local` (`.gitignore:196-229`). **UNTESTED** as a file: no test reads the repo's own `.gitignore`.
- **Projects TA initialises.** `ta init` and `ta setup vcs` write an ignore block built from one list, `LOCAL_TA_PATHS` (`crates/ta-workspace/src/partitioning.rs:41`, entries at `:95`, `:109`, `:118-120`), and the write is idempotent. **ENFORCED:** `partitioning.rs:761` asserts the block covers these paths; a hygiene test fails the build if code joins a `.ta/<literal>` path that is neither registered nor allowlisted; `init_makes_git_ignore_every_secret_under_dot_ta` (`apps/ta-cli/src/commands/init.rs:2387`) runs a real `git init`, `ta init` and `git check-ignore` on the vault, key, `*.cred`, token paths; `bare_init_on_git_repo_updates_gitignore` (`init.rs:2350`) checks the block text.
- **A project with no VCS** gets no ignore file.
- **`ta doctor` detects a secret that git can see:** `check_vcs` warns "`.ta/<path>` exists but is not ignored" (`doctor.rs:865`, **UNTESTED**: I found no test of that function), and `check_control_plane_secrets` flags in-tree keys, tokens and plaintext `*.cred` files, adding "not ignored by git: it can be committed", failing under the strict posture (`doctor.rs:256`; tests `doctor.rs:2856`, `2899`, `2930`). These only run when someone runs `ta doctor`.
- **`git add -f`, a rename, or a secret stored anywhere else defeats the ignore rules.** A committed `*.cred` must be removed from history and its credential rotated; `doctor` only prints that advice (`doctor.rs:243-248`).

---

## 6. What CI verifies, and what it does not

CI is `.github/workflows/ci.yml`.

**Verified on every pull request, including docs-only ones** (the `version-check` job, `ci.yml:101-107`): `scripts/secret-scan.sh --self-test` then `scripts/secret-scan.sh` over every tracked or not-ignored file (`git ls-files --cached --others --exclude-standard`). It reports `file:line` and the pattern for:
- Stripe-style, Anthropic `sk-ant-`, OpenAI `sk-proj-`, GitHub, Slack, AWS and Google keys, and PEM `PRIVATE KEY` blocks;
- since this branch, age secret keys (`AGE-SECRET-KEY-1...`) and `tahc_` human credentials;
- since this branch, any tracked or unignored file named like a secret: `.ta/credentials.json`, `.ta/credentials.key`, `.ta/audit.key`, `.ta/broker_root.key`, any `.ta/*.key|*.pem|*.cred`, and any `*.cred` (the vault is binary ciphertext, which text pattern scanning cannot see).

The allowlist is `scripts/secret-scan.allowlist`, which holds only `tests/fixtures/secret-shaped/*`. It applies to the path check too.

**Verified on code changes** (the `lint-and-test` job on ubuntu and macOS, `ci.yml:110-175`; docs-only pull requests skip it): `cargo fmt --check`, build, `cargo test --workspace`, clippy with `-D warnings`. That includes:
- the keychain guard, `crates/ta-credentials/tests/keychain_guard.rs`: a test binary that reaches the real keychain panics with an actionable message;
- `apps/ta-cli/tests/custody_install_script.rs` (the installer's custody decision with a fake `codesign`);
- `apps/ta-cli/tests/no_external_agents.rs` (the guard that keeps tests from reaching a paid agent);
- the file-mode tests in section 3 (Unix only, so ubuntu and macOS);
- the Seatbelt conformance step on macOS (`ci.yml:169-173`), and the Linux and Windows sandbox conformance jobs.

**What has NO CI check.** Stated plainly:
- No CI step runs on a real macOS keychain, a real Secret Service or with a Team ID. The keychain code is tested against injected backends and the guard only. The Windows job runs a Credential Manager probe (`ci.yml:384-389`); I did not read what it asserts.
- No CI step verifies the repo's own `.gitignore` entries, or that the installed `ta` has a Team ID.
- No test asserts the audit-key and vault-key file modes, the vault file's mode, or Windows ACLs.
- No pre-commit hook exists in the repo (no `.pre-commit-config.yaml`, no `.githooks`). The secret scan runs only in CI and when you run it yourself, so a secret is caught after push, not before commit.
- `.gitguardian.yaml` is configuration only. No workflow in this repository runs GitGuardian. Whether the GitHub app is enabled cannot be seen from the repo.
- No test launches an agent and asserts that no vault secret is in its environment (section 4.2).
- No CI step covers the unsandboxed solo `ta run` worker reading key files. It can, by design (section 4.3).
- The scan is pattern-based. A secret with an unusual shape, or one in a binary file, passes.

---

## 7. Recovery

Only paths the code supports are listed. **There is no key-rotation command** for the root key, the audit key or the vault key, and no re-encryption of `credentials.json` under a new key.

### 7.1 A project's vault will not open: its key is not in the active custody store

Cause: custody mode is per user, so after `ta credentials custody file` succeeded in one project (or after you wrote `file` into the custody file by hand, or set `TA_NO_KEYCHAIN=1`), every other project is in file custody too, while its vault key may still exist only as the shared keychain item (or the reverse after switching to `keychain`). `install_local.sh` avoids this for installs: it refuses to write `file` when the keychain already holds TA items (`scripts/custody-decision.sh`, `ta_keychain_holds_ta_secrets`).

What TA does: **it refuses to open the vault and creates no key.** The error says `the credential vault at <path> already holds encrypted credentials, but no encryption key was found in <where it looked>. No replacement key was created ...` and names both remedies (`VaultError::KeyMissing`, `crates/ta-credentials/src/error.rs:47`). **ENFORCED** (file custody, project and non-project vaults): `FileVault::open` reads the vault first and forbids key creation when it holds ciphertext (`file_vault.rs:59-72`), and `load_identity` returns `KeyMissing` instead of generating (`encryption.rs:133-146`, `:250-257` for the per-project state directory, `:359-364` for a bare vault directory). Tests: `missing_key_with_existing_encrypted_vault_is_refused_and_no_key_is_created` (`file_vault.rs:550`: error text, no key file written, vault bytes unchanged, refusal repeats), `missing_project_vault_key_in_the_state_dir_is_refused_and_not_recreated` (`:635`), `no_vault_yet_creates_a_key_and_vault_with_key_opens` (`:593`), `restoring_the_key_makes_a_refused_vault_open_again` (`:614`), `an_empty_vault_file_does_not_block_key_creation` (`:660`). A legacy plaintext vault and an empty file still get a key, because there is no ciphertext to protect. **UNTESTED:** the keychain branch of the same rule (`keyring_load_or_create(false)`, `encryption.rs:328`): an empty keychain with creation forbidden falls through to the key file and then fails with `KeyMissing`. The keychain test double is process-global, and the keychain tests were not extended.

Before this rule, a missing key was replaced silently with a new one, the vault then failed to decrypt with `DecryptionFailed`, and the new key shadowed the real one until you deleted it by hand.

Recovery:
1. Run `ta credentials custody file` (or `keychain`, whichever store holds the key) in that project. It copies the key with read-back verification and never deletes the source.
2. Retry; `ta doctor` should report the vault key in the expected custody.
3. If the old key is gone for good: `mv .ta/credentials.json .ta/credentials.json.lost`, then re-add credentials with `ta credentials add`. Nothing recovers the old contents.

`ta doctor` reports a vault file with no key (`doctor.rs:2608`) and now says the same thing.

**Scope.** This rule covers the vault key only. The root key and the audit key have no notion of existing evidence in the code (`broker.rs:95-98` regenerates a missing root key; the audit key is created only by `ta audit ledger checkpoint --create-key`), so they are unchanged (section 7.2).

### 7.2 A key is lost or deleted

- **Vault key:** the credentials in `credentials.json` cannot be recovered without the original key (`crates/ta-credentials/src/error.rs`). TA refuses to open the vault and writes no replacement key (section 7.1). Restore the key file from your backup or migrate it with `ta credentials custody file|keychain`; if it is gone for good, move the vault aside and re-add the credentials with `ta credentials add`.
- **Root key:** deleting it makes TA generate a new one (`crates/ta-credential-broker/src/broker.rs:95-98`). Every outstanding grant then fails verification; mint fresh grants with `ta credentials grant`. A corrupt key file is an error (`BrokerError::KeyUnreadable`) and does not regenerate.
- **Audit key:** with no key, `ta audit ledger verify` warns and skips HMAC verification, and checkpoint signing refuses until `ta audit ledger checkpoint --create-key`. A new key means older checkpoint signatures no longer verify (`apps/ta-cli/src/commands/audit.rs`, `crates/ta-audit/src/chain.rs`).
- **Human credential:** run `ta login` again. It mints a new credential and the old one stops working at once; `ta login logout` deletes it (`human.rs`, `apps/ta-cli/src/commands/login.rs`).

### 7.3 A secret leaked

| Secret | Supported action | Not supported |
|---|---|---|
| A credential in the vault (the upstream token) | Rotate it at the provider, then `ta credentials update <id> --secret-stdin`. Grants stay valid and resolve to the new value. Or `ta credentials revoke <id>`. | `revoke` deletes the vault entry and its session tokens. It does not revoke issued biscuits or anything upstream. |
| Human credential | `ta login` (replaces it), `ta login logout` | |
| Daemon bearer token | `ta token revoke <id> --kind daemon` | |
| Outcome-signing key, rules owner key | `ta outcome-key rotate|promote|revoke`; `ta rules rotate-key --yes` | |
| Root key, audit key, vault key | Delete the key file or keychain item, accepting the consequences in 7.2 | Any command that rotates or re-encrypts. `ta init --regen-audit-key` appears in an error string (`chain.rs:170-171`) but **does not exist**. |
| A single biscuit grant | Wait for its expiry | `CredentialBroker::revoke` (`broker.rs:401-412`) has no CLI command. |

### 7.4 Secrets inside the project tree (legacy layout)

`ta doctor --fix --only secrets` moves in-tree keys, daemon tokens and token files to the state directory, verifying each copy before removing the original, and refuses to overwrite a differing value (`apps/ta-cli/src/commands/doctor_fix.rs:113-125`, `state_secrets.rs`). It never generates or rotates a key. `*.cred` files are reported only.

---

## 8. Gaps found, in one place

1. A solo `ta run` worker is unsandboxed by default, so a same-user agent can read every key file (section 4.3).
2. Non-broker-mediated vault credentials are put in the agent's environment as raw secrets under `--credential-scopes`, and brokered secrets are still obtainable through the shims (section 4.2).
3. The shim and endpoint credential path does not bind a token to the agent it was issued to (section 4.1).
4. A sandboxed agent still holds the model API key (section 4.3).
5. No redaction on live MCP output (section 4.4).
6. ~~File custody plus an unmigrated per-project vault key silently generates a replacement key~~ **closed** (section 7.1). Mode switching is still per user while migration is per project, so the vault now refuses to open until you migrate; it no longer loses or shadows the key. The root and audit keys have no such guard.
7. No key rotation or re-encryption command for the root, audit and vault keys (section 7).
8. `ta init --regen-audit-key`, named in an error message, does not exist (`crates/ta-audit/src/chain.rs:170-171`).
9. `cargo run` binaries are not covered by the keychain guard; only test binaries are (section 2, CONTRIBUTING.md).
10. No CI coverage of real keychain, Team ID, Windows ACL or several file modes, and no pre-commit hook (section 6).
11. The project's `.ta/` directory mode is not set or checked (section 3).

**Fixed in this branch:** secret directories were not created 0700; `~/.config/ta/human` was not in the sandbox deny list; the secret scan did not match age keys or `tahc_` credentials and did not flag a committable `.ta/credentials.json`; there was no `ta doctor` warning for keychain custody with an unsigned binary; a missing vault key was silently replaced by a new one (section 7.1).
