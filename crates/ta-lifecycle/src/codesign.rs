//! The macOS signing step for auto-restart.
//!
//! TA signs `ta` and `ta-daemon` with a stable local identity so Keychain
//! "Always Allow" grants (keyed to the signing identity) survive rebuilds.
//! Rules, enforced here and tested:
//!
//! * never ad-hoc sign (identity `-` or empty is refused, there is no
//!   fallback), because this also runs on end users' machines and must not
//!   rewrite an installed release binary or replace a real signature;
//! * never create or request a new Keychain identity: it only passes the
//!   named identity to `codesign`, which fails fast, leaving the file
//!   untouched, when that identity is absent;
//! * always use the stable per-binary identifier.
//!
//! The identity name is supplied by the caller (TA uses
//! `ta_workspace::local_dev::codesign_identity`), keeping this crate free of
//! workspace dependencies.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Stable identifier for `ta` (matches `install_local.sh`).
pub const CLI_IDENTIFIER: &str = "com.trustedautonomy.ta";
/// Stable identifier for `ta-daemon` (matches `install_local.sh`).
pub const DAEMON_IDENTIFIER: &str = "com.trustedautonomy.ta-daemon";

const CODESIGN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignRequest {
    pub binary: PathBuf,
    pub identity: String,
    pub identifier: String,
}

/// Performs the actual signing. `sign` returns whether it succeeded.
pub trait Signer {
    /// `false` on platforms with no code signing: nothing is attempted.
    fn available(&self) -> bool;
    fn sign(&self, req: &SignRequest) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignOutcome {
    Signed,
    /// The named identity is not available (or signing failed); the binary
    /// was left untouched.
    IdentityUnavailable {
        identity: String,
    },
    /// An ad-hoc identity was requested and refused.
    RefusedAdHoc,
    NotApplicable,
}

impl SignOutcome {
    /// One log line saying what happened and what the user can do.
    pub fn log_line(&self, binary: &Path) -> String {
        match self {
            Self::Signed => format!("codesign: signed {} with the stable local identity", binary.display()),
            Self::IdentityUnavailable { identity } => format!(
                "codesign: local identity '{identity}' is not available, left {} untouched (no ad-hoc signing). \
                 Dev machines: create the certificate described in install_local.sh or set TA_CODESIGN_IDENTITY. \
                 Other machines: nothing to do.",
                binary.display()
            ),
            Self::RefusedAdHoc => format!(
                "codesign: refused an ad-hoc signing identity for {}; set TA_CODESIGN_IDENTITY to a named local certificate",
                binary.display()
            ),
            Self::NotApplicable => format!("codesign: not applicable on this platform, {} untouched", binary.display()),
        }
    }
}

/// Sign `binary` with the stable identity when it exists; otherwise do
/// nothing and say so in the returned outcome.
pub fn ensure_stable_codesign(
    signer: &dyn Signer,
    binary: &Path,
    identity: &str,
    identifier: &str,
) -> SignOutcome {
    if !signer.available() {
        return SignOutcome::NotApplicable;
    }
    let identity = identity.trim();
    if identity.is_empty() || identity == "-" {
        return SignOutcome::RefusedAdHoc;
    }
    let req = SignRequest {
        binary: binary.to_path_buf(),
        identity: identity.to_string(),
        identifier: identifier.to_string(),
    };
    if signer.sign(&req) {
        SignOutcome::Signed
    } else {
        SignOutcome::IdentityUnavailable {
            identity: identity.to_string(),
        }
    }
}

/// Real signer: `codesign --force --sign <identity> --identifier <id> <bin>`
/// on macOS, bounded by a short timeout so a Keychain prompt nobody is
/// present for cannot hang a restart. Not available elsewhere.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemCodesign;

impl Signer for SystemCodesign {
    fn available(&self) -> bool {
        cfg!(target_os = "macos")
    }

    fn sign(&self, req: &SignRequest) -> bool {
        use std::process::{Command, Stdio};
        let mut child = match Command::new("codesign")
            .arg("--force")
            .arg("--sign")
            .arg(&req.identity)
            .arg("--identifier")
            .arg(&req.identifier)
            .arg(&req.binary)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(s)) => return s.success(),
                Ok(None) if start.elapsed() >= CODESIGN_TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => return false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Mock {
        available: bool,
        succeeds: bool,
        calls: RefCell<Vec<SignRequest>>,
    }
    impl Signer for Mock {
        fn available(&self) -> bool {
            self.available
        }
        fn sign(&self, req: &SignRequest) -> bool {
            self.calls.borrow_mut().push(req.clone());
            self.succeeds
        }
    }
    fn mock(available: bool, succeeds: bool) -> Mock {
        Mock {
            available,
            succeeds,
            calls: RefCell::new(vec![]),
        }
    }

    #[test]
    fn signs_with_the_named_identity_and_stable_identifier() {
        let m = mock(true, true);
        let out = ensure_stable_codesign(
            &m,
            Path::new("/i/ta-daemon"),
            "Trusted Autonomy Local Dev",
            DAEMON_IDENTIFIER,
        );
        assert_eq!(out, SignOutcome::Signed);
        let calls = m.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].identity, "Trusted Autonomy Local Dev");
        assert_eq!(calls[0].identifier, "com.trustedautonomy.ta-daemon");
    }

    #[test]
    fn never_ad_hoc_signs_even_when_asked_to() {
        for bad in ["-", "", "  ", " - "] {
            let m = mock(true, true);
            let out = ensure_stable_codesign(&m, Path::new("/i/ta"), bad, CLI_IDENTIFIER);
            assert_eq!(out, SignOutcome::RefusedAdHoc, "identity {bad:?}");
            assert!(
                m.calls.borrow().is_empty(),
                "signer must not be invoked for {bad:?}"
            );
        }
    }

    #[test]
    fn absent_identity_does_nothing_special_and_says_so() {
        let m = mock(true, false);
        let out = ensure_stable_codesign(
            &m,
            Path::new("/i/ta-daemon"),
            "Missing Cert",
            DAEMON_IDENTIFIER,
        );
        assert_eq!(
            out,
            SignOutcome::IdentityUnavailable {
                identity: "Missing Cert".into()
            }
        );
        // Exactly one attempt with the named identity: no retry with another.
        assert_eq!(m.calls.borrow().len(), 1);
        let line = out.log_line(Path::new("/i/ta-daemon"));
        assert!(
            line.contains("Missing Cert")
                && line.contains("untouched")
                && line.contains("TA_CODESIGN_IDENTITY"),
            "{line}"
        );
    }

    /// Source guard: the only `"-"` identity literal in the production code
    /// is the refusal check, so no code path can pass an ad-hoc identity on.
    #[test]
    fn production_source_never_builds_an_ad_hoc_sign_command() {
        let src = include_str!("codesign.rs").replace("\r\n", "\n");
        let production = src.split("#[cfg(test)]").next().unwrap();
        assert!(!production.contains(".arg(\"-\")"));
        assert_eq!(
            production.matches("\"-\"").count(),
            1,
            "the ad-hoc literal may appear only in the refusal check"
        );
        assert!(production.contains("\"--sign\""));
        assert_eq!(production.matches("\"--sign\"").count(), 1);
    }

    #[test]
    fn unavailable_platform_attempts_nothing() {
        let m = mock(false, true);
        assert_eq!(
            ensure_stable_codesign(&m, Path::new("/i/ta"), "X", CLI_IDENTIFIER),
            SignOutcome::NotApplicable
        );
        assert!(m.calls.borrow().is_empty());
    }
}
