# ta-shell.ps1 -- Build, start the TA daemon, and launch the interactive shell.
#
# Usage:
#   .\scripts\ta-shell.ps1 [-Port 7700] [-ProjectRoot .] [-NoBuild] [shell args...]
#
# The script builds the workspace, checks whether the daemon is already
# listening, starts one if not, and opens the shell.
# On exit, the daemon keeps running.

param(
    [int]$Port = $(if ($env:TA_DAEMON_PORT) { [int]$env:TA_DAEMON_PORT } else { 7700 }),
    [string]$Bind = $(if ($env:TA_DAEMON_BIND) { $env:TA_DAEMON_BIND } else { "127.0.0.1" }),
    [string]$ProjectRoot = ".",
    [switch]$NoBuild,
    [Parameter(ValueFromRemainingArguments)]
    [string[]]$ShellArgs
)

$ErrorActionPreference = "Stop"
$DaemonUrl = "http://${Bind}:${Port}"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $ScriptDir

# ── Build ─────────────────────────────────────────────────────
if (-not $NoBuild) {
    Write-Host "Building ta-daemon and ta..."
    Push-Location $RepoRoot
    try {
        cargo build --bin ta-daemon --bin ta
        if ($LASTEXITCODE -ne 0) {
            Write-Error "Build failed"
            exit 1
        }
    } finally {
        Pop-Location
    }
}

# ── Locate binaries ──────────────────────────────────────────
function Find-Binary($Name) {
    $release = Join-Path $RepoRoot "target\release\$Name.exe"
    $debug = Join-Path $RepoRoot "target\debug\$Name.exe"
    if (Test-Path $release) { return $release }
    if (Test-Path $debug) { return $debug }
    $found = Get-Command $Name -ErrorAction SilentlyContinue
    if ($found) { return $found.Source }
    Write-Error "Cannot find binary '$Name'"
    exit 1
}

$TaBin = Find-Binary "ta"
$DaemonBin = Find-Binary "ta-daemon"

Write-Host "Using daemon: $DaemonBin"
Write-Host "Using CLI:    $TaBin"

# ── Start daemon if needed ───────────────────────────────────
function Test-DaemonHealthy {
    try {
        $null = Invoke-RestMethod -Uri "$DaemonUrl/api/status" -TimeoutSec 2
        return $true
    } catch {
        return $false
    }
}

function Get-DaemonVersion {
    try {
        $status = Invoke-RestMethod -Uri "$DaemonUrl/api/status" -TimeoutSec 2
        return $status.version
    } catch { return $null }
}

function Get-BuiltVersion {
    try {
        $ver = & $DaemonBin --version 2>&1
        if ($ver -match '(\d[\d.]+-[a-z]+)') { return $Matches[1] }
    } catch {}
    return $null
}

if (Test-DaemonHealthy) {
    $runningVer = Get-DaemonVersion
    $builtVer = Get-BuiltVersion
    Write-Host "Daemon already running at $DaemonUrl (v$runningVer)"

    # Kill and restart if the running daemon is stale.
    if ($runningVer -and $builtVer -and ($runningVer -ne $builtVer)) {
        Write-Host "Stale daemon (v$runningVer) — restarting with v$builtVer..."
        Get-Process -Name "ta-daemon" -ErrorAction SilentlyContinue | Stop-Process -Force
        Start-Sleep -Seconds 1

        $daemonProcess = Start-Process -FilePath $DaemonBin `
            -ArgumentList "--api", "--project-root", $ProjectRoot `
            -PassThru -NoNewWindow

        $healthy = $false
        for ($i = 0; $i -lt 20; $i++) {
            if ($daemonProcess.HasExited) {
                Write-Error "Daemon exited unexpectedly"
                exit 1
            }
            if (Test-DaemonHealthy) {
                Write-Host "Daemon ready (pid $($daemonProcess.Id))"
                $healthy = $true
                break
            }
            Start-Sleep -Milliseconds 500
        }
        if (-not $healthy) {
            Write-Error "Restarted daemon did not become healthy within 10 seconds"
            Stop-Process -Id $daemonProcess.Id -Force -ErrorAction SilentlyContinue
            exit 1
        }
    }
} else {
    Write-Host "Starting daemon at $DaemonUrl ..."
    $daemonProcess = Start-Process -FilePath $DaemonBin `
        -ArgumentList "--api", "--project-root", $ProjectRoot `
        -PassThru -NoNewWindow

    # Wait up to 10 seconds for the daemon to become healthy.
    $healthy = $false
    for ($i = 0; $i -lt 20; $i++) {
        if ($daemonProcess.HasExited) {
            Write-Error "Daemon exited unexpectedly (exit code $($daemonProcess.ExitCode))"
            exit 1
        }
        if (Test-DaemonHealthy) {
            Write-Host "Daemon ready (pid $($daemonProcess.Id))"
            $healthy = $true
            break
        }
        Start-Sleep -Milliseconds 500
    }

    if (-not $healthy) {
        Write-Error "Daemon did not become healthy within 10 seconds"
        Stop-Process -Id $daemonProcess.Id -Force -ErrorAction SilentlyContinue
        exit 1
    }
}

# ── Launch the shell ─────────────────────────────────────────
& $TaBin shell --url $DaemonUrl @ShellArgs
