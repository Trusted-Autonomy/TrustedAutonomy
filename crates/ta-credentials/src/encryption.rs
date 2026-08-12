// encryption.rs — Encryption-at-rest for FileVault (v0.17.6.2).
//
// `.ta/credentials.json` is encrypted with `age` (X25519 recipient/identity).
// Key custody prefers the OS keychain (macOS Keychain, Windows Credential
// Manager, Secret Service on Linux); when no backend is reachable, the key
// falls back to a chmod-0600 file next to the vault. Callers are expected to
// surface `KeyCustody::FallbackFile` loudly (see `ta doctor`) since it's a
// materially weaker guarantee than OS-native custody.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use age::secrecy::ExposeSecret;
use age::x25519::Identity;
use tracing::warn;

use crate::error::VaultError;

const KEYRING_SERVICE: &str = "trusted-autonomy-vault";
const KEYRING_USER: &str = "credential-vault-age-identity";

/// Filename of the fallback key file, stored next to the vault file itself.
pub const FALLBACK_KEY_FILENAME: &str = "credentials.key";

/// Where the vault's age identity is actually stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCustody {
    /// Stored in the OS-native keychain/credential manager.
    Keychain,
    /// Stored in a chmod-0600 file — used when no keychain backend is
    /// reachable (e.g. headless Linux with no Secret Service daemon).
    FallbackFile(PathBuf),
}

/// Load the vault's age identity, generating and persisting one on first use.
///
/// When `use_keychain` is true, tries the OS keychain first; on any failure
/// (no backend, locked session, etc.) falls back to a chmod-0600 file at
/// `<vault_dir>/credentials.key` and logs a loud warning so the gap is
/// observable outside of `ta doctor`. When `use_keychain` is false, the
/// keychain is never touched — see `CredentialsConfig::use_keychain` for why
/// (tests, headless servers).
pub fn load_or_create_identity(
    vault_dir: &Path,
    use_keychain: bool,
) -> Result<(Identity, KeyCustody), VaultError> {
    if use_keychain {
        match keyring_load_or_create() {
            Ok(identity) => return Ok((identity, KeyCustody::Keychain)),
            Err(reason) => {
                warn!(
                    reason = %reason,
                    "OS keychain unavailable for vault encryption key; falling back to \
                     file-based key custody (chmod 0600). Run `ta doctor` for details."
                );
            }
        }
    }
    let key_path = vault_dir.join(FALLBACK_KEY_FILENAME);
    let identity = file_load_or_create(&key_path)?;
    Ok((identity, KeyCustody::FallbackFile(key_path)))
}

/// Non-mutating probe of current key custody, for `ta doctor`. Returns `None`
/// when neither a keychain entry nor a fallback file exists yet (no vault
/// key has been created).
pub fn probe_key_custody(vault_dir: &Path) -> Option<KeyCustody> {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
        if entry.get_password().is_ok() {
            return Some(KeyCustody::Keychain);
        }
    }
    let key_path = vault_dir.join(FALLBACK_KEY_FILENAME);
    if key_path.exists() {
        return Some(KeyCustody::FallbackFile(key_path));
    }
    None
}

fn keyring_load_or_create() -> Result<Identity, String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .map_err(|e| format!("keyring entry error: {e}"))?;
    match entry.get_password() {
        Ok(secret) => {
            Identity::from_str(&secret).map_err(|e| format!("stored age identity is corrupt: {e}"))
        }
        Err(keyring::Error::NoEntry) => {
            let identity = Identity::generate();
            entry
                .set_password(identity.to_string().expose_secret())
                .map_err(|e| format!("keyring write error: {e}"))?;
            Ok(identity)
        }
        Err(e) => Err(format!("keyring read error: {e}")),
    }
}

fn file_load_or_create(key_path: &Path) -> Result<Identity, VaultError> {
    if key_path.exists() {
        let content = fs::read_to_string(key_path)?;
        Identity::from_str(content.trim()).map_err(|e| VaultError::KeyUnreadable {
            path: key_path.to_path_buf(),
            reason: e.to_string(),
        })
    } else {
        if let Some(parent) = key_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let identity = Identity::generate();
        fs::write(key_path, identity.to_string().expose_secret())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(key_path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(identity)
    }
}

/// Encrypt `plaintext` to the given identity's recipient.
pub fn encrypt(identity: &Identity, plaintext: &[u8]) -> Result<Vec<u8>, VaultError> {
    let recipient = identity.to_public();
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .map_err(|e| VaultError::EncryptionFailed(e.to_string()))?;

    let mut encrypted = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut encrypted)
        .map_err(|e| VaultError::EncryptionFailed(e.to_string()))?;
    writer
        .write_all(plaintext)
        .map_err(|e| VaultError::EncryptionFailed(e.to_string()))?;
    writer
        .finish()
        .map_err(|e| VaultError::EncryptionFailed(e.to_string()))?;
    Ok(encrypted)
}

/// Decrypt `ciphertext` (produced by [`encrypt`]) with the given identity.
pub fn decrypt(
    identity: &Identity,
    vault_path: &Path,
    ciphertext: &[u8],
) -> Result<Vec<u8>, VaultError> {
    let fail = |reason: String| VaultError::DecryptionFailed {
        path: vault_path.to_path_buf(),
        reason,
    };

    let decryptor = age::Decryptor::new(ciphertext).map_err(|e| fail(e.to_string()))?;
    let mut reader = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|e| fail(e.to_string()))?;
    let mut decrypted = Vec::new();
    reader
        .read_to_end(&mut decrypted)
        .map_err(|e| fail(e.to_string()))?;
    Ok(decrypted)
}

/// Heuristic: does this vault file look like the pre-encryption plaintext
/// JSON format (used to transparently migrate existing vaults)?
pub fn looks_like_plaintext_json(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|b| *b == b'{')
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn file_fallback_generates_and_persists_identity() {
        let dir = TempDir::new().unwrap();
        let key_path = dir.path().join(FALLBACK_KEY_FILENAME);

        let identity1 = file_load_or_create(&key_path).unwrap();
        assert!(key_path.exists());
        let identity2 = file_load_or_create(&key_path).unwrap();

        // Same identity round-trips from disk (public keys match).
        assert_eq!(
            identity1.to_public().to_string(),
            identity2.to_public().to_string()
        );
    }

    #[test]
    #[cfg(unix)]
    fn file_fallback_key_is_chmod_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let key_path = dir.path().join(FALLBACK_KEY_FILENAME);
        file_load_or_create(&key_path).unwrap();

        let mode = fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn corrupt_fallback_key_produces_actionable_error() {
        let dir = TempDir::new().unwrap();
        let key_path = dir.path().join(FALLBACK_KEY_FILENAME);
        fs::write(&key_path, "not-a-valid-age-identity").unwrap();

        let err = file_load_or_create(&key_path)
            .err()
            .expect("expected an error");
        match err {
            VaultError::KeyUnreadable { path, .. } => assert_eq!(path, key_path),
            other => panic!("expected KeyUnreadable, got {other:?}"),
        }
    }

    #[test]
    fn encrypt_decrypt_round_trips() {
        let identity = Identity::generate();
        let plaintext = b"{\"credentials\":[]}";

        let ciphertext = encrypt(&identity, plaintext).unwrap();
        assert_ne!(ciphertext, plaintext);
        assert!(!looks_like_plaintext_json(&ciphertext));

        let decrypted = decrypt(&identity, Path::new("/tmp/vault.json"), &ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn decrypt_with_wrong_identity_produces_actionable_error() {
        let identity = Identity::generate();
        let other = Identity::generate();
        let ciphertext = encrypt(&identity, b"secret-data").unwrap();

        let result = decrypt(&other, Path::new("/tmp/vault.json"), &ciphertext);
        match result {
            Err(VaultError::DecryptionFailed { path, .. }) => {
                assert_eq!(path, Path::new("/tmp/vault.json"))
            }
            other => panic!("expected DecryptionFailed, got {other:?}"),
        }
    }

    #[test]
    fn looks_like_plaintext_json_detects_legacy_format() {
        assert!(looks_like_plaintext_json(b"{\"credentials\":[]}"));
        assert!(looks_like_plaintext_json(b"  \n{\"a\":1}"));
        assert!(!looks_like_plaintext_json(b"age-encryption.org/v1"));
        assert!(!looks_like_plaintext_json(&[0xa9, 0x1b, 0x00, 0x02]));
    }
}
