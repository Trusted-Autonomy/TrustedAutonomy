# diagnose-windows-stack-overflow.ps1
#
# Gathers diagnostic data for the Windows-only stack overflow crash in
# apps/ta-cli/tests/verb_alias_cli.rs (PR #537, v0.17.0.12.16).
#
# Two invocations crash on Windows CI with "thread 'main' has overflowed its
# stack":
#   ta.exe --project-root <tempdir> --no-version-check goal list
#   ta.exe --project-root <tempdir> --no-version-check create team
# ...but the same commands never reproduce locally on macOS/Linux, and
# neither invocation's code path can recurse (verified by reading
# dispatch_raw/resolve() -- both are single, non-recursive dispatches).
#
# This script:
#   1. Builds ta.exe fresh (debug profile, matches CI).
#   2. Runs both failing invocations directly (bypassing cargo test's harness)
#      several times each, to check if the crash is deterministic or flaky
#      even outside CI.
#   3. Uses `editbin /STACK` (ships with the MSVC Build Tools already
#      required to build Rust on Windows) to try a much larger *main thread*
#      stack on a COPY of ta.exe -- RUST_MIN_STACK only affects
#      std::thread::spawn'd threads, not main, so this is the only way to
#      test "would a bigger stack fix it" without a code change.
#      - If the larger-stack copy succeeds where the original crashes: this
#        is deep-but-finite recursion/large stack frames, not a true
#        infinite loop -- much easier follow-up fix.
#      - If it STILL crashes even at 64MB: points at genuine infinite
#        recursion or corruption, not just "needs more stack."
#   4. Captures everything to diagnostic-output.log for sharing back.
#
# Run from the repo root in a "Developer PowerShell for VS" (so editbin is
# on PATH) after checking out:
#   git fetch origin
#   git checkout feature/ecddba86-implement-v0-17-0-12-16-cli-verb-set-consolidation
#   .\scripts\diagnose-windows-stack-overflow.ps1

$ErrorActionPreference = "Continue"
$logFile = "diagnostic-output.log"
Remove-Item $logFile -ErrorAction SilentlyContinue

function Log($msg) {
    Write-Host $msg
    Add-Content -Path $logFile -Value $msg
}

Log "=== Windows stack-overflow diagnostic run: $(Get-Date -Format o) ==="
Log ""
Log "--- Building ta-cli (debug, matches CI) ---"
cargo build -p ta-cli 2>&1 | Tee-Object -Append -FilePath $logFile | Out-Null

$taExe = "target\debug\ta.exe"
if (-not (Test-Path $taExe)) {
    Log "ERROR: $taExe not found after build. Aborting."
    exit 1
}
Log "Found: $taExe"

function Invoke-TaCommand($label, $args) {
    $tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP ("ta-diag-" + [guid]::NewGuid())) -Force
    Log ""
    Log "--- $label ---"
    Log "cwd: $tmp"
    Log "args: --project-root $tmp --no-version-check $($args -join ' ')"
    $env:RUST_BACKTRACE = "full"
    $proc = Start-Process -FilePath (Resolve-Path $taExe) `
        -ArgumentList (@("--project-root", "$tmp", "--no-version-check") + $args) `
        -NoNewWindow -Wait -PassThru `
        -RedirectStandardOutput "$tmp\stdout.txt" `
        -RedirectStandardError "$tmp\stderr.txt"
    Log "Exit code: $($proc.ExitCode)"
    Log "STDOUT:"
    Get-Content "$tmp\stdout.txt" | ForEach-Object { Log "  $_" }
    Log "STDERR:"
    Get-Content "$tmp\stderr.txt" | ForEach-Object { Log "  $_" }
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    return $proc.ExitCode
}

Log ""
Log "=== Phase 1: run each failing invocation 5x with the ORIGINAL binary ==="
Log "(checking whether it's deterministic or itself flaky outside CI)"
for ($i = 1; $i -le 5; $i++) {
    Invoke-TaCommand "goal list, attempt $i (original stack)" @("goal", "list")
}
for ($i = 1; $i -le 5; $i++) {
    Invoke-TaCommand "create team, attempt $i (original stack)" @("create", "team")
}

Log ""
Log "=== Phase 2: try a much larger MAIN-THREAD stack via editbin ==="
$editbin = Get-Command editbin.exe -ErrorAction SilentlyContinue
if (-not $editbin) {
    Log "editbin.exe not found on PATH -- re-run this script from a 'Developer PowerShell for VS'"
    Log "(Start Menu -> Visual Studio 2022 -> Developer PowerShell for VS 2022)."
    Log "Skipping Phase 2."
} else {
    $bigStackExe = "target\debug\ta-bigstack.exe"
    Copy-Item $taExe $bigStackExe -Force
    # 64MB reserve, matching Linux/macOS's much larger default main-thread stack (usually 8MB
    # already, but Windows' MSVC default is 1MB -- 64x that rules out "just needed more headroom".
    & editbin /STACK:67108864 $bigStackExe 2>&1 | Tee-Object -Append -FilePath $logFile | Out-Null
    $taExe = $bigStackExe
    Log "Patched $bigStackExe to a 64MB main-thread stack reservation."

    for ($i = 1; $i -le 3; $i++) {
        Invoke-TaCommand "goal list, attempt $i (64MB stack)" @("goal", "list")
    }
    for ($i = 1; $i -le 3; $i++) {
        Invoke-TaCommand "create team, attempt $i (64MB stack)" @("create", "team")
    }
}

Log ""
Log "=== Done. Share $logFile back. ==="
Log "Key question to answer from this log: did the 64MB-stack copy succeed where"
Log "the original crashed? If yes -> deep-but-finite recursion/large frames, an"
Log "easier fix. If it crashed even at 64MB -> genuine infinite recursion or"
Log "something else entirely, needs a debugger (windbg/Visual Studio) attached"
Log "to the crashing process to get a real call stack at the moment of overflow."
