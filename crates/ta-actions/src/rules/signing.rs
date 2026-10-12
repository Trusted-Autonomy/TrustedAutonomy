// rules/signing.rs: the owner's signature over the rules file (v0.17.12.4).
//
// An agent that can edit the rules file turns the rule table into advice. So
// the file is signed with an Ed25519 key the project owner holds OUTSIDE the
// project tree (OS keychain, or an owner-only file in the per-project state
// directory), and the daemon verifies the signature on every compile.
//
// Trust anchor. The public key the daemon checks against lives in the state
// directory too (`rules/owner.pub`), never in the tree and never in the
// signature file: if the signature file named its own key, an agent could
// replace both. A clone on another machine has no anchor until the owner runs
// `ta rules trust`, and without an anchor a rules file is refused.
//
// The signed message is a domain tag plus the file text with CRLF folded to
// LF and any BOM removed, so editing on Windows does not break the signature.
// The tag keeps an owner signature from being replayed as an org floor
// signature or the reverse.
//
// Fail closed: every failure below is a reason the file is refused, with what
// to do about it. None is a warning.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::model::normalize_text;

pub const SIG_ALG: &str = "ed25519";
pub const SIG_SCHEMA: u32 = 1;
/// Suffix of the signature file next to the rules file.
pub const SIG_SUFFIX: &str = ".sig";

/// What a signature is for. Part of the signed bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Owner,
    Floor,
}

impl Domain {
    fn tag(self) -> &'static str {
        match self {
            Domain::Owner => "owner",
            Domain::Floor => "floor",
        }
    }
}

/// The detached signature file (JSON).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SigFile {
    pub schema: u32,
    pub alg: String,
    /// Fingerprint of the signing public key (a hint, never trusted).
    pub key_id: String,
    /// sha256 of the signed text (a hint for humans; the signature is what counts).
    pub sha256: String,
    pub signed_at: String,
    /// Posture the file named when signed, and the digest of the shipped
    /// posture text, so a later TA that ships a different table is noticed.
    pub posture: String,
    pub posture_digest: String,
    pub signature: String,
}

/// Why a signature was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigProblem {
    NoTrustAnchor,
    MissingSignature,
    Malformed(String),
    UnsupportedAlg(String),
    KeyMismatch { signed_by: String, trusted: String },
    BadSignature,
}

impl SigProblem {
    /// What happened and what to do.
    pub fn describe(&self, what: &str) -> String {
        match self {
            SigProblem::NoTrustAnchor => format!(
                "{what} has no trusted owner key on this machine, so it cannot be verified. If \
                 you own this project and signed it elsewhere, run `ta rules trust --pubkey \
                 <key>` with the key `ta rules status` printed there. If this is your first \
                 time, run `ta rules init`."
            ),
            SigProblem::MissingSignature => format!(
                "{what} has no signature file. A rules file nobody signed is advice, not rules. \
                 Review it, then run `ta rules sign`."
            ),
            SigProblem::Malformed(why) => format!(
                "the signature for {what} cannot be read ({why}). Restore the signature file \
                 from version control, or review the rules and run `ta rules sign` again."
            ),
            SigProblem::UnsupportedAlg(a) => format!(
                "the signature for {what} uses \"{a}\", which this TA does not support. Sign it \
                 again with `ta rules sign`."
            ),
            SigProblem::KeyMismatch { signed_by, trusted } => format!(
                "{what} was signed by key {signed_by}, but this machine trusts {trusted}. If the \
                 owner changed, run `ta rules trust --pubkey <new key>`. If you did not expect \
                 this, someone replaced the file: restore it from version control."
            ),
            SigProblem::BadSignature => format!(
                "{what} was changed after it was signed (or the signature is forged). Nothing \
                 that file allows is in effect. Review `ta rules diff`, then run `ta rules sign` \
                 if the change is yours, or restore the file from version control."
            ),
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Short, stable name for a public key.
pub fn fingerprint(public: &[u8; 32]) -> String {
    let d = Sha256::digest(public);
    format!("ed25519:{}", &hex(&d)[..16])
}

pub fn generate_seed() -> [u8; 32] {
    let mut seed = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    seed
}

pub fn seed_to_b64(seed: &[u8; 32]) -> String {
    B64.encode(seed)
}

pub fn seed_from_b64(text: &str) -> Result<[u8; 32], String> {
    let bytes = B64
        .decode(text.trim())
        .map_err(|e| format!("the stored owner key is not valid base64: {e}"))?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
        format!(
            "the stored owner key has {} bytes, expected 32",
            bytes.len()
        )
    })
}

pub fn public_of(seed: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(seed).verifying_key().to_bytes()
}

pub fn public_to_b64(public: &[u8; 32]) -> String {
    B64.encode(public)
}

pub fn public_from_b64(text: &str) -> Result<[u8; 32], String> {
    let bytes = B64
        .decode(text.trim())
        .map_err(|e| format!("the public key is not valid base64: {e}"))?;
    let key = <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| format!("the public key has {} bytes, expected 32", bytes.len()))?;
    VerifyingKey::from_bytes(&key)
        .map_err(|e| format!("the public key is not a valid Ed25519 key: {e}"))?;
    Ok(key)
}

fn message(domain: Domain, text: &str) -> Vec<u8> {
    let mut m = format!("ta-rules-v1\0{}\0", domain.tag()).into_bytes();
    m.extend_from_slice(normalize_text(text).as_bytes());
    m
}

/// Sign `text`. `posture` and `posture_digest` are recorded for drift checks.
pub fn sign(
    seed: &[u8; 32],
    domain: Domain,
    text: &str,
    posture: &str,
    posture_digest: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> SigFile {
    let key = SigningKey::from_bytes(seed);
    let signature = key.sign(&message(domain, text));
    SigFile {
        schema: SIG_SCHEMA,
        alg: SIG_ALG.into(),
        key_id: fingerprint(&key.verifying_key().to_bytes()),
        sha256: hex(&Sha256::digest(normalize_text(text).as_bytes())),
        signed_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        posture: posture.into(),
        posture_digest: posture_digest.into(),
        signature: B64.encode(signature.to_bytes()),
    }
}

/// A verified signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub key_id: String,
    pub sig: SigFile,
}

/// Verify `text` against its signature file and the trusted public key.
/// `sig_json` and `trusted_pub_b64` are `None` when the file or anchor is absent.
pub fn verify(
    domain: Domain,
    text: &str,
    sig_json: Option<&str>,
    trusted_pub_b64: Option<&str>,
) -> Result<Verified, SigProblem> {
    let Some(trusted_b64) = trusted_pub_b64 else {
        return Err(SigProblem::NoTrustAnchor);
    };
    let trusted = public_from_b64(trusted_b64).map_err(|e| {
        SigProblem::Malformed(format!("the trusted key on this machine is unusable: {e}"))
    })?;
    let Some(sig_json) = sig_json else {
        return Err(SigProblem::MissingSignature);
    };
    let sig: SigFile =
        serde_json::from_str(sig_json).map_err(|e| SigProblem::Malformed(e.to_string()))?;
    if sig.schema != SIG_SCHEMA {
        return Err(SigProblem::Malformed(format!(
            "unsupported signature schema {}",
            sig.schema
        )));
    }
    if sig.alg != SIG_ALG {
        return Err(SigProblem::UnsupportedAlg(sig.alg));
    }
    let trusted_id = fingerprint(&trusted);
    if sig.key_id != trusted_id {
        return Err(SigProblem::KeyMismatch {
            signed_by: sig.key_id,
            trusted: trusted_id,
        });
    }
    let raw = B64
        .decode(sig.signature.trim())
        .map_err(|e| SigProblem::Malformed(format!("signature is not base64: {e}")))?;
    let bytes = <[u8; 64]>::try_from(raw.as_slice()).map_err(|_| {
        SigProblem::Malformed(format!("signature has {} bytes, expected 64", raw.len()))
    })?;
    let verifying = VerifyingKey::from_bytes(&trusted)
        .map_err(|e| SigProblem::Malformed(format!("trusted key unusable: {e}")))?;
    verifying
        .verify_strict(&message(domain, text), &Signature::from_bytes(&bytes))
        .map_err(|_| SigProblem::BadSignature)?;
    Ok(Verified {
        key_id: trusted_id,
        sig,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-10-12T09:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn fixture() -> ([u8; 32], String, String) {
        let seed = [7u8; 32];
        let anchor = public_to_b64(&public_of(&seed));
        (seed, anchor, "[[deny]]\naction = \"email\"\n".to_string())
    }

    fn signed(seed: &[u8; 32], text: &str) -> String {
        serde_json::to_string(&sign(seed, Domain::Owner, text, "strict", "d", now())).unwrap()
    }

    #[test]
    fn round_trip_verifies_and_survives_crlf() {
        let (seed, anchor, text) = fixture();
        let sig = signed(&seed, &text);
        let v = verify(Domain::Owner, &text, Some(&sig), Some(&anchor)).unwrap();
        assert!(v.key_id.starts_with("ed25519:"));
        // The same file saved with Windows line endings and a BOM still verifies.
        let win = format!("\u{feff}{}", text.replace('\n', "\r\n"));
        verify(Domain::Owner, &win, Some(&sig), Some(&anchor)).unwrap();
    }

    #[test]
    fn any_edit_breaks_the_signature() {
        let (seed, anchor, text) = fixture();
        let sig = signed(&seed, &text);
        let edited = text.replace("deny", "allow");
        assert_eq!(
            verify(Domain::Owner, &edited, Some(&sig), Some(&anchor)).unwrap_err(),
            SigProblem::BadSignature
        );
        let trailing = format!("{text}\n");
        assert_eq!(
            verify(Domain::Owner, &trailing, Some(&sig), Some(&anchor)).unwrap_err(),
            SigProblem::BadSignature
        );
    }

    #[test]
    fn missing_pieces_fail_closed_with_a_fix() {
        let (seed, anchor, text) = fixture();
        let sig = signed(&seed, &text);
        let none = verify(Domain::Owner, &text, Some(&sig), None).unwrap_err();
        assert_eq!(none, SigProblem::NoTrustAnchor);
        assert!(none.describe("the rules file").contains("ta rules init"));
        let nosig = verify(Domain::Owner, &text, None, Some(&anchor)).unwrap_err();
        assert_eq!(nosig, SigProblem::MissingSignature);
        assert!(nosig.describe("the rules file").contains("ta rules sign"));
        for junk in ["", "not json", "{}", "{\"schema\":1}"] {
            assert!(matches!(
                verify(Domain::Owner, &text, Some(junk), Some(&anchor)).unwrap_err(),
                SigProblem::Malformed(_)
            ));
        }
    }

    #[test]
    fn attacker_key_cannot_replace_the_owner_key() {
        let (_seed, anchor, text) = fixture();
        // The agent signs its own edited file with its own key and names that key.
        let attacker = [9u8; 32];
        let edited = text.replace("deny", "allow");
        let sig = signed(&attacker, &edited);
        let err = verify(Domain::Owner, &edited, Some(&sig), Some(&anchor)).unwrap_err();
        assert!(matches!(err, SigProblem::KeyMismatch { .. }), "{err:?}");
        // Even lying about the key id does not help: the signature is checked
        // against the trusted key, not the one in the file.
        let mut forged: SigFile = serde_json::from_str(&sig).unwrap();
        forged.key_id = fingerprint(&public_from_b64(&anchor).unwrap());
        let err = verify(
            Domain::Owner,
            &edited,
            Some(&serde_json::to_string(&forged).unwrap()),
            Some(&anchor),
        )
        .unwrap_err();
        assert_eq!(err, SigProblem::BadSignature);
    }

    #[test]
    fn owner_and_floor_signatures_are_not_interchangeable() {
        let (seed, anchor, text) = fixture();
        let sig = serde_json::to_string(&sign(&seed, Domain::Floor, &text, "strict", "d", now()))
            .unwrap();
        assert_eq!(
            verify(Domain::Owner, &text, Some(&sig), Some(&anchor)).unwrap_err(),
            SigProblem::BadSignature
        );
        verify(Domain::Floor, &text, Some(&sig), Some(&anchor)).unwrap();
    }

    #[test]
    fn unknown_alg_and_unknown_fields_are_refused() {
        let (seed, anchor, text) = fixture();
        let mut sig = sign(&seed, Domain::Owner, &text, "strict", "d", now());
        sig.alg = "rsa".into();
        let err = verify(
            Domain::Owner,
            &text,
            Some(&serde_json::to_string(&sig).unwrap()),
            Some(&anchor),
        )
        .unwrap_err();
        assert!(matches!(err, SigProblem::UnsupportedAlg(_)));
        let mut v: serde_json::Value = serde_json::from_str(&signed(&seed, &text)).unwrap();
        v["extra"] = serde_json::json!(1);
        assert!(matches!(
            verify(Domain::Owner, &text, Some(&v.to_string()), Some(&anchor)).unwrap_err(),
            SigProblem::Malformed(_)
        ));
    }

    #[test]
    fn key_encoding_round_trips_and_rejects_garbage() {
        let seed = generate_seed();
        assert_eq!(seed_from_b64(&seed_to_b64(&seed)).unwrap(), seed);
        assert!(seed_from_b64("AAAA").is_err());
        assert!(public_from_b64("not base64!").is_err());
        assert!(public_from_b64("AAAA").is_err());
    }
}
