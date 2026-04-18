# Release Operations Guide

Operational runbook for TA releases — certificate management, CI pipeline, and platform-specific installer signing.

---

## Windows Code Signing

### Overview

The TA Windows MSI and executables are signed with an Extended Validation (EV) code signing certificate. EV certificates eliminate the Microsoft SmartScreen "Windows protected your PC" warning on first install, regardless of download count. Unsigned or OV-signed binaries require hundreds of installs before SmartScreen's reputation score clears.

The signing step in `release.yml` is **skip-safe**: if `WINDOWS_SIGNING_CERT_BASE64` is not set, the step exits 0 and the release continues unsigned. This allows releases to proceed before an EV cert is in place.

### Certificate Procurement (one-time, human step)

1. Purchase an Extended Validation (EV) Code Signing Certificate from a Microsoft-trusted CA:
   - **DigiCert** (recommended) — https://www.digicert.com/signing/code-signing-certificates
   - Sectigo or GlobalSign are also accepted
   - Cost: approximately $300–500/year
   - Lead time: 1–5 business days (identity verification required)

2. The **Common Name (CN)** on the cert must match the publisher name shown in Windows UAC prompts. Use the exact legal entity name (e.g., `Trusted Autonomy, Inc.`).

3. Export the certificate as a PFX/PKCS#12 file with a strong password.

4. Base64-encode the PFX:
   ```bash
   # macOS / Linux
   base64 -i certificate.pfx | tr -d '\n'

   # PowerShell (Windows)
   [Convert]::ToBase64String([IO.File]::ReadAllBytes('certificate.pfx'))
   ```

5. Add the values as GitHub Actions repository secrets:
   - `WINDOWS_SIGNING_CERT_BASE64` — the base64-encoded PFX
   - `WINDOWS_SIGNING_PASSWORD` — the PFX password

### Renewing the Certificate

EV certificates typically expire after 1–3 years. To renew:

1. Order a new EV cert from the same CA (or a different Microsoft-trusted CA).
2. Export the renewed cert as a PFX file.
3. Base64-encode it (see above).
4. Update the `WINDOWS_SIGNING_CERT_BASE64` and `WINDOWS_SIGNING_PASSWORD` secrets in GitHub → Settings → Secrets and variables → Actions.
5. No code changes are required — the release workflow reads the secrets at build time.

**Note**: Signed artifacts remain valid after the cert expires because the timestamp server (DigiCert RFC 3161 TSA at `http://timestamp.digicert.com`) countersigns the timestamp at signing time. The timestamp chain is valid independently of the cert's validity period.

### What to Do if the Cert Expires Mid-Release Cycle

1. **Binaries already signed** before expiry remain valid — the RFC 3161 timestamp proves they were signed while the cert was active.
2. **New builds** after expiry will produce unsigned binaries (the signing step skips gracefully).
3. **Renew immediately** following the steps above, then retrigger the release workflow via `workflow_dispatch` with the tag input.
4. If a release shipped with unsigned binaries, users will see the SmartScreen warning. Communicate via release notes and retrigger the release once the renewed cert is in place.

### Verifying a Signed MSI Locally

```powershell
# Verify Authenticode signature
signtool verify /pa /v ta-*.msi

# Or using PowerShell's built-in cmdlet
Get-AuthenticodeSignature ta-*.msi | Select-Object Status, SignerCertificate

# Expected output: Status = Valid
```

On Linux/macOS, use `osslsigncode verify`:
```bash
osslsigncode verify -in ta-*.msi
```

### CI Signing Details

The `Sign Windows artifacts` step in `release.yml` runs `scripts/sign-windows.ps1` with:

| Parameter | Value |
|-----------|-------|
| Binary targets | `ta.exe`, `ta-daemon.exe` |
| Installer target | `ta-<version>-x86_64-pc-windows-msvc.msi` |
| Digest algorithm | SHA-256 (`/fd sha256`) |
| Timestamp server | `http://timestamp.digicert.com` (RFC 3161) |
| Timestamp digest | SHA-256 (`/td sha256`) |
| Verification | `signtool verify /pa` after each file |

The step fails the build if any signature or verification returns a non-zero exit code.

---

## macOS Code Signing (planned)

macOS Gatekeeper hardening (`codesign --deep` + `notarytool` notarization) is tracked in the plan and will eliminate the "macOS cannot verify the developer" prompt. It requires an Apple Developer account ($99/year). See PLAN.md Phase v0.15.16 item 8.

---

## See Also

- `scripts/sign-windows.ps1` — signing helper script
- `.github/workflows/release.yml` — full release pipeline
- `docs/USAGE.md` — end-user installation guide
