#Requires -Version 5.1
<#
.SYNOPSIS
    Sign Windows binaries and MSI with an EV code signing certificate.

.DESCRIPTION
    Decodes the base64-encoded PFX from WINDOWS_SIGNING_CERT_BASE64, writes it
    to a temp file, signs all supplied .exe/.msi files with signtool.exe using
    SHA-256 digest and an RFC 3161 timestamp, verifies each signature, then
    removes the temp PFX. If the signing secrets are absent the script exits 0
    (skip mode) so the release workflow continues without signing.

    The temp PFX is always removed in a finally block — even if signing or
    verification throws a terminating error.

.PARAMETER Artifacts
    One or more paths to .exe or .msi files to sign. Accepts pipeline input and
    wildcards resolved before being passed (e.g. artifacts\*.msi).

.EXAMPLE
    .\sign-windows.ps1 artifacts\ta.exe artifacts\ta-daemon.exe artifacts\ta-v1.0.0-x86_64-pc-windows-msvc.msi

.NOTES
    Required environment variables (set as GitHub Actions secrets):
      WINDOWS_SIGNING_CERT_BASE64  — base64-encoded PFX file
      WINDOWS_SIGNING_PASSWORD     — PFX password

    Timestamp server: http://timestamp.digicert.com (RFC 3161, SHA-256 digest).
    Signatures remain valid after cert expiry.
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, ValueFromRemainingArguments = $true)]
    [string[]]$Artifacts
)

$ErrorActionPreference = 'Stop'

# ── 1. Check secrets ──────────────────────────────────────────────────────────
$certBase64  = $env:WINDOWS_SIGNING_CERT_BASE64
$certPassword = $env:WINDOWS_SIGNING_PASSWORD

if ([string]::IsNullOrEmpty($certBase64)) {
    Write-Host "WINDOWS_SIGNING_CERT_BASE64 is not set — skipping code signing."
    Write-Host "To enable signing, add WINDOWS_SIGNING_CERT_BASE64 and WINDOWS_SIGNING_PASSWORD"
    Write-Host "as GitHub Actions repository secrets."
    exit 0
}

# ── 2. Locate signtool.exe ────────────────────────────────────────────────────
$signtool = $null

# Prefer the path advertised by the runner environment (GitHub Actions sets this).
if ($env:SIGNTOOL_PATH -and (Test-Path $env:SIGNTOOL_PATH)) {
    $signtool = $env:SIGNTOOL_PATH
}

if (-not $signtool) {
    # Search Windows SDK installations (newest first).
    $sdkBases = @(
        "${env:ProgramFiles(x86)}\Windows Kits\10\bin",
        "${env:ProgramFiles}\Windows Kits\10\bin"
    )
    foreach ($base in $sdkBases) {
        if (Test-Path $base) {
            $candidate = Get-ChildItem -Path $base -Recurse -Filter 'signtool.exe' -ErrorAction SilentlyContinue |
                         Sort-Object FullName -Descending |
                         Select-Object -First 1
            if ($candidate) {
                $signtool = $candidate.FullName
                break
            }
        }
    }
}

if (-not $signtool) {
    Write-Error "signtool.exe not found. Install the Windows SDK or set SIGNTOOL_PATH."
    exit 1
}
Write-Host "Using signtool: $signtool"

# ── 3. Decode PFX to a temp file ──────────────────────────────────────────────
$tempPfx = [System.IO.Path]::Combine([System.IO.Path]::GetTempPath(), [System.IO.Path]::GetRandomFileName() + '.pfx')
Write-Host "Writing temp PFX to: $tempPfx"

try {
    [System.IO.File]::WriteAllBytes($tempPfx, [System.Convert]::FromBase64String($certBase64))

    # ── 4. Sign each artifact ─────────────────────────────────────────────────
    foreach ($artifact in $Artifacts) {
        if (-not (Test-Path $artifact)) {
            Write-Error "Artifact not found: $artifact"
            exit 1
        }

        Write-Host "Signing: $artifact"

        $signArgs = @(
            'sign',
            '/fd',  'sha256',           # file digest algorithm
            '/tr',  'http://timestamp.digicert.com',  # RFC 3161 TSA
            '/td',  'sha256',           # timestamp digest algorithm
            '/f',   $tempPfx
        )
        if (-not [string]::IsNullOrEmpty($certPassword)) {
            $signArgs += '/p'
            $signArgs += $certPassword
        }
        $signArgs += $artifact

        & $signtool @signArgs
        if ($LASTEXITCODE -ne 0) {
            Write-Error "signtool sign failed for '$artifact' (exit $LASTEXITCODE)."
            exit 1
        }
        Write-Host "  signed OK"

        # ── 5. Verify ─────────────────────────────────────────────────────────
        Write-Host "Verifying: $artifact"
        & $signtool verify /pa $artifact
        if ($LASTEXITCODE -ne 0) {
            Write-Error "signtool verify failed for '$artifact' (exit $LASTEXITCODE)."
            exit 1
        }
        Write-Host "  verified OK"
    }

    Write-Host "All $($Artifacts.Count) artifact(s) signed and verified successfully."

} finally {
    # Always remove the temp PFX — even if signing or verification threw.
    if (Test-Path $tempPfx) {
        Remove-Item -Force $tempPfx
        Write-Host "Temp PFX removed."
    }
}
