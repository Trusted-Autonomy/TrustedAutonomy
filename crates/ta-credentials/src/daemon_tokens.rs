//! Daemon API bearer-token store format (`daemon-tokens.json`).
//!
//! The file lives in the per-user TA state directory, outside the project
//! tree. Both the daemon (which verifies bearer tokens) and `ta token`
//! (which mints and revokes them) use this one definition so the format
//! cannot drift.
//!
//! Two entry shapes are accepted:
//!
//! * **Digest entry** (written by `ta token create --kind daemon`): holds
//!   `digest`, the lowercase SHA-256 hex of the secret. The secret itself is
//!   shown once at creation and never stored.
//! * **Legacy entry** (hand-edited or written by older versions): holds the
//!   plaintext secret in `token`. Still accepted so existing installs keep
//!   working; replace it with a minted token when convenient.
//!
//! Secrets are compared by digest in constant time.

use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Token permission scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenScope {
    Read,
    /// Service-account scope for relays (the Wayfinder poller): may read the
    /// attention feed and call the relay endpoints (whiteboard, state
    /// changes), but is never accepted as a human for approve, deny, apply or
    /// answer. A relayed human action must carry that human's own assertion.
    Relay,
    Write,
    Admin,
}

impl TokenScope {
    pub fn allows_write(&self) -> bool {
        matches!(self, Self::Relay | Self::Write | Self::Admin)
    }

    /// Whether a bare token of this scope can stand in for a human on the
    /// gated routes (remote callers only; see `api::auth`). `Relay` cannot.
    pub fn counts_as_human(&self) -> bool {
        matches!(self, Self::Write | Self::Admin)
    }

    #[allow(dead_code)]
    pub fn allows_admin(&self) -> bool {
        matches!(self, Self::Admin)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Relay => "relay",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }

    /// Parse the scopes `ta token create --kind daemon` may mint. `admin` is
    /// deliberately not mintable from the CLI.
    pub fn parse_mintable(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "read" => Ok(Self::Read),
            "relay" => Ok(Self::Relay),
            "write" => Ok(Self::Write),
            other => Err(format!(
                "unknown daemon token scope '{other}'. Use one of: relay, read, write."
            )),
        }
    }
}

/// One entry in `daemon-tokens.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRecord {
    /// Plaintext secret. Only present on legacy entries, and in memory on the
    /// record returned by a fresh mint; never serialized for digest entries.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token: String,
    /// SHA-256 hex digest of the secret (digest entries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Stable identifier shown by `ta token list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub scope: TokenScope,
    pub created_at: DateTime<Utc>,
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Lowercase SHA-256 hex of a secret.
pub fn digest_of(secret: &str) -> String {
    let d = Sha256::digest(secret.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time equality for two equal-purpose byte strings (digests).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u8;
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

/// A fresh random secret in the daemon's `ta_<64 hex>` format.
pub fn generate_secret() -> String {
    let bytes: [u8; 32] = rand::thread_rng().gen();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("ta_{hex}")
}

impl TokenRecord {
    /// The digest this record is matched by (computed for legacy entries).
    fn effective_digest(&self) -> Option<String> {
        match &self.digest {
            Some(d) => Some(d.clone()),
            None if !self.token.is_empty() => Some(digest_of(&self.token)),
            None => None,
        }
    }

    /// Whether `secret` is this record's secret. Compares digests in constant
    /// time; ignores expiry and revocation (see [`TokenRecord::is_active`]).
    pub fn matches(&self, secret: &str) -> bool {
        match self.effective_digest() {
            Some(d) => ct_eq(d.as_bytes(), digest_of(secret).as_bytes()),
            None => false,
        }
    }

    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.map(|e| e <= now).unwrap_or(false)
    }

    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        !self.is_revoked() && !self.is_expired(now)
    }

    /// Identifier for display: the id, or a digest-derived one for legacy
    /// entries (never derived from the plaintext).
    pub fn display_id(&self) -> String {
        match (&self.id, self.effective_digest()) {
            (Some(i), _) => i.clone(),
            (None, Some(d)) => format!("legacy-{}", &d[..8]),
            (None, None) => "unknown".into(),
        }
    }

    /// First 12 hex characters of the digest, for `list` output.
    pub fn short_digest(&self) -> String {
        self.effective_digest()
            .map(|d| d[..12.min(d.len())].to_string())
            .unwrap_or_else(|| "-".into())
    }

    pub fn is_legacy_plaintext(&self) -> bool {
        self.digest.is_none() && !self.token.is_empty()
    }
}

/// Mint a digest-only record. Returns `(record_to_store, secret)`; show the
/// secret once and store only the record.
pub fn mint(
    scope: TokenScope,
    label: Option<String>,
    expires_in: Option<Duration>,
) -> (TokenRecord, String) {
    let secret = generate_secret();
    let now = Utc::now();
    let id_bytes: [u8; 4] = rand::thread_rng().gen();
    let id = format!(
        "dt-{}",
        id_bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let rec = TokenRecord {
        token: String::new(),
        digest: Some(digest_of(&secret)),
        id: Some(id),
        scope,
        created_at: now,
        label,
        expires_at: expires_in.map(|d| now + d),
        revoked_at: None,
    };
    (rec, secret)
}

/// Find the active record that `secret` belongs to. Every record is compared
/// (no early exit on the first byte difference).
pub fn verify<'a>(
    records: &'a [TokenRecord],
    secret: &str,
    now: DateTime<Utc>,
) -> Option<&'a TokenRecord> {
    let mut found = None;
    for r in records {
        if r.matches(secret) && r.is_active(now) {
            found = Some(r);
        }
    }
    found
}

/// Read the token file. A missing file is an empty store; an unreadable or
/// malformed file is an error so a writer never overwrites tokens it could
/// not parse.
pub fn load_file(path: &Path) -> Result<Vec<TokenRecord>, String> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let content = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "could not read the daemon token file {} ({e}). Check its permissions.",
            path.display()
        )
    })?;
    if content.trim().is_empty() {
        return Ok(vec![]);
    }
    serde_json::from_str(&content).map_err(|e| {
        format!(
            "the daemon token file {} is not valid JSON ({e}). Fix or remove it; nothing was changed.",
            path.display()
        )
    })
}

/// Atomically write the token file (tmp + rename), owner-only on Unix.
pub fn save_file(path: &Path, records: &[TokenRecord]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json =
        serde_json::to_string_pretty(records).map_err(|e| std::io::Error::other(e.to_string()))?;
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_stores_digest_not_secret() {
        let (rec, secret) = mint(TokenScope::Relay, Some("poller".into()), None);
        let json = serde_json::to_string(&rec).unwrap();
        assert!(!json.contains(&secret));
        assert!(json.contains(&digest_of(&secret)));
        assert!(rec.matches(&secret));
        assert!(!rec.matches("ta_other"));
    }

    #[test]
    fn legacy_plaintext_entry_still_verifies() {
        let json = r#"[{"token":"ta_legacy","scope":"write","created_at":"2026-01-01T00:00:00Z","label":null}]"#;
        let recs: Vec<TokenRecord> = serde_json::from_str(json).unwrap();
        assert!(recs[0].is_legacy_plaintext());
        assert!(verify(&recs, "ta_legacy", Utc::now()).is_some());
        assert!(recs[0].display_id().starts_with("legacy-"));
    }

    #[test]
    fn expired_and_revoked_are_rejected() {
        let (mut rec, secret) = mint(TokenScope::Read, None, Some(Duration::minutes(-1)));
        assert!(verify(std::slice::from_ref(&rec), &secret, Utc::now()).is_none());
        rec.expires_at = None;
        assert!(verify(std::slice::from_ref(&rec), &secret, Utc::now()).is_some());
        rec.revoked_at = Some(Utc::now());
        assert!(verify(&[rec], &secret, Utc::now()).is_none());
    }

    #[test]
    fn ct_eq_behaves() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }

    #[test]
    fn save_and_load_roundtrip_and_malformed_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon-tokens.json");
        assert!(load_file(&path).unwrap().is_empty());
        let (rec, secret) = mint(TokenScope::Write, Some("w".into()), None);
        save_file(&path, &[rec]).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains(&secret));
        assert_eq!(load_file(&path).unwrap().len(), 1);
        std::fs::write(&path, "not json").unwrap();
        assert!(load_file(&path).is_err());
    }

    #[test]
    fn parse_mintable_rejects_admin() {
        assert!(TokenScope::parse_mintable("admin").is_err());
        assert_eq!(
            TokenScope::parse_mintable("Relay").unwrap(),
            TokenScope::Relay
        );
    }
}
