// outcome_keys.rs: the daemon's Ed25519 keys for signing outcome envelopes
// (red-team CR-04, plan phase v0.17.12.2).
//
// Layout inside the per-user, daemon-only state directory (never the project
// tree):
//
//   <state>/secrets/outcome-<kid>.key      private seed, owner-only, daemon only
//   <state>/outcome-keys/public-keys.json  public registry the poller reads
//
// Key lifecycle (statuses are in the registry, so a verifier sees them):
//   pending  : published so verifiers can pre-load it, not yet signing
//   active   : the one key that signs new envelopes
//   retired  : no longer signs, still verifies (envelopes it signed stay valid
//              until they go stale)
//   revoked  : never verifies; envelopes it signed are refused
//
// Rotation adds the new key id (pending) before the old one retires, so a
// poller that refreshes its registry between `stage` and `promote` never sees
// a signature from a key it has not heard of.

use std::path::{Path, PathBuf};

use base64::Engine;
use ed25519_dalek::SigningKey;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::state::ProjectState;

const REGISTRY_VERSION: u32 = 1;
const REGISTRY_FILE: &str = "public-keys.json";
const KEYS_DIRNAME: &str = "outcome-keys";

/// Why an operation on the outcome key store failed. Every message says what
/// was attempted, what happened and what to do.
#[derive(Debug, thiserror::Error)]
pub enum OutcomeKeyError {
    #[error("outcome key store I/O failed for {path}: {detail}. Check that the TA state directory is writable (`ta show state`).")]
    Io { path: PathBuf, detail: String },
    #[error("the outcome key registry at {path} is not valid ({detail}). Restore it from a backup, or remove it and run `ta show outcome-key` to rebuild it from the keys still in the state directory.")]
    BadRegistry { path: PathBuf, detail: String },
    #[error("the private outcome signing key {kid} is missing or unreadable at {path} ({detail}). Run `ta outcome-key rotate` then `ta outcome-key promote` to replace it.")]
    BadPrivateKey {
        kid: String,
        path: PathBuf,
        detail: String,
    },
    #[error("outcome key {0} is not in the registry. List keys with `ta show outcome-key`.")]
    UnknownKey(String),
    #[error("outcome key {kid} is {status:?} and cannot be used for that. {hint}")]
    WrongStatus {
        kid: String,
        status: KeyStatus,
        hint: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    Pending,
    Active,
    Retired,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicKeyEntry {
    pub kid: String,
    pub alg: String,
    /// base64url (no padding) of the 32-byte Ed25519 public key.
    pub public_key: String,
    pub status: KeyStatus,
    /// Unix seconds.
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<i64>,
}

impl PublicKeyEntry {
    /// `sha256:` + first 16 bytes (32 hex chars) of SHA-256 over the raw
    /// public key bytes, the value `ta show outcome-key` prints.
    pub fn fingerprint(&self) -> String {
        let raw = b64().decode(&self.public_key).unwrap_or_default();
        let digest = Sha256::digest(&raw);
        let hex: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
        format!("sha256:{hex}")
    }
}

/// The public registry file. This is the whole contract surface a poller
/// reads: see docs/superpowers/specs/2026-10-11-outcome-envelope-contract.md.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicRegistry {
    pub v: u32,
    pub keys: Vec<PublicKeyEntry>,
}

impl PublicRegistry {
    pub fn empty() -> Self {
        Self {
            v: REGISTRY_VERSION,
            keys: Vec::new(),
        }
    }
    pub fn get(&self, kid: &str) -> Option<&PublicKeyEntry> {
        self.keys.iter().find(|k| k.kid == kid)
    }
    pub fn active(&self) -> Option<&PublicKeyEntry> {
        self.keys.iter().find(|k| k.status == KeyStatus::Active)
    }
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

/// The outcome signing keys of one project.
pub struct OutcomeKeyStore {
    registry_path: PathBuf,
    secrets_dir: PathBuf,
}

impl OutcomeKeyStore {
    pub fn open(ps: &ProjectState) -> Self {
        Self {
            registry_path: Self::registry_path_for(ps),
            secrets_dir: ps.secrets_dir(),
        }
    }

    /// Where the public registry lives (printed by `ta show outcome-key`).
    pub fn registry_path_for(ps: &ProjectState) -> PathBuf {
        ps.dir.join(KEYS_DIRNAME).join(REGISTRY_FILE)
    }

    pub fn registry_path(&self) -> &Path {
        &self.registry_path
    }

    fn secret_path(&self, kid: &str) -> PathBuf {
        self.secrets_dir.join(format!("outcome-{kid}.key"))
    }

    /// Read the registry; an absent file is an empty registry.
    pub fn registry(&self) -> Result<PublicRegistry, OutcomeKeyError> {
        match std::fs::read_to_string(&self.registry_path) {
            Ok(raw) => serde_json::from_str(&raw).map_err(|e| OutcomeKeyError::BadRegistry {
                path: self.registry_path.clone(),
                detail: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PublicRegistry::empty()),
            Err(e) => Err(OutcomeKeyError::Io {
                path: self.registry_path.clone(),
                detail: e.to_string(),
            }),
        }
    }

    fn save(&self, reg: &PublicRegistry) -> Result<(), OutcomeKeyError> {
        let raw = serde_json::to_string_pretty(reg).map_err(|e| OutcomeKeyError::BadRegistry {
            path: self.registry_path.clone(),
            detail: e.to_string(),
        })?;
        if let Some(parent) = self.registry_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| OutcomeKeyError::Io {
                path: parent.to_path_buf(),
                detail: e.to_string(),
            })?;
        }
        // Write-then-rename so a poller reading concurrently never sees a
        // half-written registry.
        let tmp = self.registry_path.with_extension("json.tmp");
        std::fs::write(&tmp, raw).map_err(|e| OutcomeKeyError::Io {
            path: tmp.clone(),
            detail: e.to_string(),
        })?;
        std::fs::rename(&tmp, &self.registry_path).map_err(|e| OutcomeKeyError::Io {
            path: self.registry_path.clone(),
            detail: e.to_string(),
        })
    }

    fn mint(&self, status: KeyStatus, now: i64) -> Result<PublicKeyEntry, OutcomeKeyError> {
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        let signing = SigningKey::from_bytes(&seed);
        let mut suffix = [0u8; 4];
        rand::rngs::OsRng.fill_bytes(&mut suffix);
        let kid = format!(
            "k{}-{}",
            chrono::DateTime::from_timestamp(now, 0)
                .map(|d| d.format("%Y%m%d").to_string())
                .unwrap_or_else(|| "00000000".into()),
            suffix
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let path = self.secret_path(&kid);
        crate::human::write_private(&path, &b64().encode(seed)).map_err(|e| {
            OutcomeKeyError::Io {
                path: path.clone(),
                detail: e.to_string(),
            }
        })?;
        Ok(PublicKeyEntry {
            kid,
            alg: "ed25519".into(),
            public_key: b64().encode(signing.verifying_key().to_bytes()),
            status,
            created_at: now,
            retired_at: None,
            revoked_at: None,
        })
    }

    /// Make sure an active signing key exists, creating the first one on
    /// first use. Returns the registry afterwards.
    pub fn ensure_active(&self, now: i64) -> Result<PublicRegistry, OutcomeKeyError> {
        let mut reg = self.registry()?;
        if reg.active().is_some() {
            return Ok(reg);
        }
        // A pending key is promoted rather than minting a second one.
        if let Some(p) = reg.keys.iter_mut().find(|k| k.status == KeyStatus::Pending) {
            p.status = KeyStatus::Active;
        } else {
            let entry = self.mint(KeyStatus::Active, now)?;
            tracing::info!(kid = %entry.kid, "outcome signing key created");
            reg.keys.push(entry);
        }
        self.save(&reg)?;
        Ok(reg)
    }

    /// Step 1 of rotation: publish a new key id (pending). It does not sign
    /// yet, so verifiers can load it before it is used. Idempotent: a second
    /// call while a key is already pending returns that key.
    pub fn stage_rotation(&self, now: i64) -> Result<PublicKeyEntry, OutcomeKeyError> {
        self.ensure_active(now)?;
        let mut reg = self.registry()?;
        if let Some(p) = reg.keys.iter().find(|k| k.status == KeyStatus::Pending) {
            return Ok(p.clone());
        }
        let entry = self.mint(KeyStatus::Pending, now)?;
        reg.keys.push(entry.clone());
        self.save(&reg)?;
        tracing::info!(kid = %entry.kid, "outcome key staged for rotation");
        Ok(entry)
    }

    /// Step 2 of rotation: the pending key becomes the signing key and the
    /// previous active key retires (it keeps verifying).
    pub fn promote(&self, now: i64) -> Result<PublicKeyEntry, OutcomeKeyError> {
        let mut reg = self.registry()?;
        let Some(pos) = reg.keys.iter().position(|k| k.status == KeyStatus::Pending) else {
            return Err(OutcomeKeyError::WrongStatus {
                kid: "(none)".into(),
                status: KeyStatus::Retired,
                hint: "There is no pending key to promote. Run `ta outcome-key rotate` first, give pollers time to refresh their registry, then promote.".into(),
            });
        };
        for k in reg.keys.iter_mut() {
            if k.status == KeyStatus::Active {
                k.status = KeyStatus::Retired;
                k.retired_at = Some(now);
            }
        }
        reg.keys[pos].status = KeyStatus::Active;
        let promoted = reg.keys[pos].clone();
        self.save(&reg)?;
        tracing::info!(kid = %promoted.kid, "outcome key promoted to active");
        Ok(promoted)
    }

    /// Revoke `kid`: it never verifies again. Revoking the active key
    /// immediately promotes a pending key or mints a replacement so the
    /// daemon keeps stamping.
    pub fn revoke(&self, kid: &str, now: i64) -> Result<PublicRegistry, OutcomeKeyError> {
        let mut reg = self.registry()?;
        let Some(entry) = reg.keys.iter_mut().find(|k| k.kid == kid) else {
            return Err(OutcomeKeyError::UnknownKey(kid.to_string()));
        };
        let was_active = entry.status == KeyStatus::Active;
        entry.status = KeyStatus::Revoked;
        entry.revoked_at = Some(now);
        self.save(&reg)?;
        // Delete the private half: a revoked key must never sign again.
        let _ = std::fs::remove_file(self.secret_path(kid));
        tracing::warn!(
            kid,
            "outcome key revoked; envelopes it signed no longer verify"
        );
        if was_active {
            return self.ensure_active(now);
        }
        Ok(reg)
    }

    /// The active signing key: `(kid, signing key)`.
    pub fn active_signing_key(&self, now: i64) -> Result<(String, SigningKey), OutcomeKeyError> {
        let reg = self.ensure_active(now)?;
        let kid = reg
            .active()
            .map(|k| k.kid.clone())
            .ok_or_else(|| OutcomeKeyError::UnknownKey("(active)".into()))?;
        let path = self.secret_path(&kid);
        let bad = |detail: String| OutcomeKeyError::BadPrivateKey {
            kid: kid.clone(),
            path: path.clone(),
            detail,
        };
        let raw = std::fs::read_to_string(&path).map_err(|e| bad(e.to_string()))?;
        let seed = b64().decode(raw.trim()).map_err(|e| bad(e.to_string()))?;
        let seed: [u8; 32] = seed
            .try_into()
            .map_err(|_| bad("not 32 bytes".to_string()))?;
        Ok((kid, SigningKey::from_bytes(&seed)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::StateLocator;

    fn store(root: &Path, state: &Path) -> (OutcomeKeyStore, ProjectState) {
        let ps = ProjectState::open(root, &StateLocator::at(state)).unwrap();
        (OutcomeKeyStore::open(&ps), ps)
    }

    #[test]
    fn first_use_creates_one_active_key_outside_the_project_tree() {
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (s, ps) = store(project.path(), state.path());
        let (kid, key) = s.active_signing_key(1_000).unwrap();
        let reg = s.registry().unwrap();
        assert_eq!(reg.keys.len(), 1);
        assert_eq!(reg.active().unwrap().kid, kid);
        assert_eq!(
            reg.active().unwrap().public_key,
            b64().encode(key.verifying_key().to_bytes())
        );
        assert!(OutcomeKeyStore::registry_path_for(&ps).starts_with(state.path()));
        // Nothing key-shaped under the project tree.
        let leaked = walk(project.path());
        assert!(leaked.iter().all(|p| !p.contains("outcome")), "{leaked:?}");
    }

    fn walk(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                out.push(e.path().display().to_string());
                if e.path().is_dir() {
                    out.extend(walk(&e.path()));
                }
            }
        }
        out
    }

    #[test]
    fn private_seed_is_not_in_the_public_registry() {
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (s, _ps) = store(project.path(), state.path());
        let (kid, key) = s.active_signing_key(1_000).unwrap();
        let raw = std::fs::read_to_string(s.registry_path()).unwrap();
        assert!(!raw.contains(&b64().encode(key.to_bytes())));
        assert!(raw.contains(&kid));
    }

    #[cfg(unix)]
    #[test]
    fn private_key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (s, _ps) = store(project.path(), state.path());
        let (kid, _) = s.active_signing_key(1_000).unwrap();
        let mode = std::fs::metadata(s.secret_path(&kid))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o}");
    }

    #[test]
    fn rotation_stages_a_new_kid_before_the_old_one_retires() {
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (s, _ps) = store(project.path(), state.path());
        let (old_kid, _) = s.active_signing_key(1_000).unwrap();
        let staged = s.stage_rotation(2_000).unwrap();
        let reg = s.registry().unwrap();
        // Both published; the old one still signs.
        assert_eq!(reg.get(&staged.kid).unwrap().status, KeyStatus::Pending);
        assert_eq!(reg.active().unwrap().kid, old_kid);
        assert_eq!(s.active_signing_key(2_001).unwrap().0, old_kid);
        // Staging twice does not pile up keys.
        assert_eq!(s.stage_rotation(2_002).unwrap().kid, staged.kid);

        let promoted = s.promote(3_000).unwrap();
        assert_eq!(promoted.kid, staged.kid);
        let reg = s.registry().unwrap();
        assert_eq!(reg.get(&old_kid).unwrap().status, KeyStatus::Retired);
        assert_eq!(reg.get(&old_kid).unwrap().retired_at, Some(3_000));
        assert_eq!(s.active_signing_key(3_001).unwrap().0, staged.kid);
    }

    #[test]
    fn promote_without_a_staged_key_explains_what_to_do() {
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (s, _ps) = store(project.path(), state.path());
        s.active_signing_key(1).unwrap();
        let err = s.promote(2).unwrap_err().to_string();
        assert!(err.contains("ta outcome-key rotate"), "{err}");
    }

    #[test]
    fn revoking_the_active_key_installs_a_replacement() {
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (s, _ps) = store(project.path(), state.path());
        let (kid, _) = s.active_signing_key(1_000).unwrap();
        let reg = s.revoke(&kid, 2_000).unwrap();
        assert_eq!(reg.get(&kid).unwrap().status, KeyStatus::Revoked);
        let (new_kid, _) = s.active_signing_key(2_001).unwrap();
        assert_ne!(new_kid, kid);
        assert!(!s.secret_path(&kid).exists());
    }

    #[test]
    fn revoking_an_unknown_kid_is_an_error_naming_the_fix() {
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (s, _ps) = store(project.path(), state.path());
        let err = s.revoke("nope", 1).unwrap_err().to_string();
        assert!(err.contains("ta show outcome-key"), "{err}");
    }

    #[test]
    fn fingerprint_is_stable_and_prefixed() {
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let (s, _ps) = store(project.path(), state.path());
        s.active_signing_key(1).unwrap();
        let reg = s.registry().unwrap();
        let fp = reg.active().unwrap().fingerprint();
        assert!(fp.starts_with("sha256:") && fp.len() == 7 + 32, "{fp}");
        assert_eq!(fp, reg.active().unwrap().fingerprint());
    }
}
