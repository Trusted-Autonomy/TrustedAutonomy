# setup-windows-dev.ps1
#
# One-time dev environment bootstrap for Windows collaborators. Checks what's
# already present and only installs what's missing -- safe to re-run.
#
# Covers what CI's Windows job (.github/workflows/ci.yml, "Windows Build")
# assumes is already present on GitHub-hosted runners:
#   - Rust toolchain (rustup), pinned per rust-toolchain.toml
#   - rustfmt / clippy components (also pinned in rust-toolchain.toml, but
#     rustup won't have them until the toolchain is actually installed once)
#   - MSVC Build Tools (the linker Rust needs on Windows -- link.exe)
#
# Does NOT touch Nix/./dev -- that toolchain path is macOS/Linux only (Nix
# has no native Windows support). Windows collaborators build/test with
# plain `cargo` directly; rust-toolchain.toml is picked up automatically by
# rustup with no wrapper needed.
#
# Usage (from repo root, any PowerShell -- does not require "Developer
# PowerShell for VS"):
#   .\scripts\setup-windows-dev.ps1

$ErrorActionPreference = "Stop"

function Test-Command($name) {
    return [bool](Get-Command $name -ErrorAction SilentlyContinue)
}

Write-Host "=== TrustedAutonomy Windows dev setup ===" -ForegroundColor Cyan
Write-Host ""

# --- 1. MSVC Build Tools (link.exe) ---
Write-Host "--- Checking MSVC Build Tools ---"
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$hasMsvc = $false
if (Test-Path $vswhere) {
    $vsInstall = & $vswhere -latest -products * `
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
        -property installationPath
    if ($vsInstall) { $hasMsvc = $true }
}
if ($hasMsvc) {
    Write-Host "OK: MSVC Build Tools (C++ workload) found." -ForegroundColor Green
} else {
    Write-Host "MISSING: MSVC Build Tools not detected." -ForegroundColor Yellow
    if (Test-Command "winget") {
        Write-Host "Installing via winget (Visual Studio 2022 Build Tools, C++ workload)..."
        winget install --id Microsoft.VisualStudio.2022.BuildTools --silent --override `
            "--wait --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64"
    } else {
        Write-Host "ERROR: winget not found and MSVC Build Tools missing." -ForegroundColor Red
        Write-Host "Install manually: https://visualstudio.microsoft.com/visual-cpp-build-tools/"
        Write-Host "  -> select the 'Desktop development with C++' workload."
        exit 1
    }
}
Write-Host ""

# --- 2. Rust toolchain (rustup) ---
Write-Host "--- Checking Rust toolchain ---"
if (Test-Command "rustup") {
    Write-Host "OK: rustup already installed ($(rustup --version))." -ForegroundColor Green
} else {
    Write-Host "MISSING: rustup not found. Installing..." -ForegroundColor Yellow
    if (Test-Command "winget") {
        winget install --id Rustlang.Rustup --silent
    } else {
        Write-Host "winget not found -- downloading rustup-init.exe directly..."
        $installer = Join-Path $env:TEMP "rustup-init.exe"
        Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile $installer
        & $installer -y --default-toolchain none
        Remove-Item $installer -ErrorAction SilentlyContinue
    }
    # Refresh PATH in this session so subsequent steps see the new install.
    $env:Path = [System.Environment]::GetEnvironmentVariable("Path", "Machine") + ";" +
                [System.Environment]::GetEnvironmentVariable("Path", "User")
    if (-not (Test-Command "rustup")) {
        Write-Host "ERROR: rustup install did not complete. Restart your shell and re-run this script." -ForegroundColor Red
        exit 1
    }
    Write-Host "OK: rustup installed." -ForegroundColor Green
}
Write-Host ""

# --- 3. Toolchain + components pinned by rust-toolchain.toml ---
# `rustup` reads rust-toolchain.toml automatically when a cargo/rustc command
# runs inside the repo, but the toolchain + components need to exist locally
# first. Running `rustup show` from the repo root triggers that install.
Write-Host "--- Installing pinned toolchain + components (rustfmt, clippy, rust-analyzer, rust-src) ---"
Push-Location $PSScriptRoot\..
try {
    rustup show | Out-Null
    Write-Host "OK: toolchain resolved: $(rustup show active-toolchain)" -ForegroundColor Green
} finally {
    Pop-Location
}
Write-Host ""

# --- 4. Sanity check: cargo build works ---
Write-Host "--- Verifying cargo is functional ---"
$cargoVersion = cargo --version 2>&1
Write-Host "cargo: $cargoVersion"
Write-Host ""

Write-Host "=== Setup complete ===" -ForegroundColor Cyan
Write-Host "Next: run .\scripts\diagnose-windows-stack-overflow.ps1 from a"
Write-Host "'Developer PowerShell for VS' (needed for editbin, not for this script)."
