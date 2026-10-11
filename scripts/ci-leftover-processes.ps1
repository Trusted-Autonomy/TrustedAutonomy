# ci-leftover-processes.ps1: regression guard for processes a CI test run
# leaves behind (Windows twin of scripts/ci-leftover-processes.sh).
#
# Usage:
#   scripts/ci-leftover-processes.ps1              list and check leftovers
#   scripts/ci-leftover-processes.ps1 -SelfTest    prove the limits with fake data
#
# Lists every running gh, ta-daemon and ta process, prints them, and exits 1
# when there is any ta-daemon, or more than MaxGh gh or MaxTa ta processes.
# Exit status: 0 within limits, 1 over a limit.
param(
    [switch]$SelfTest,
    [int]$MaxGh = 5,
    [int]$MaxTa = 5
)

function Test-Leftovers {
    param($Procs, [int]$MaxGh, [int]$MaxTa)
    $gh = @($Procs | Where-Object { $_.Name -eq 'gh' }).Count
    $ta = @($Procs | Where-Object { $_.Name -eq 'ta' }).Count
    $daemon = @($Procs | Where-Object { $_.Name -eq 'ta-daemon' }).Count
    Write-Host "Leftover processes: gh=$gh (limit $MaxGh), ta=$ta (limit $MaxTa), ta-daemon=$daemon (limit 0)"
    if ($Procs.Count -gt 0) {
        Write-Host "Process list (pid name path):"
        $Procs | Select-Object -First 50 | ForEach-Object { Write-Host ("  {0} {1} {2}" -f $_.Id, $_.Name, $_.Path) }
        if ($Procs.Count -gt 50) { Write-Host "... $($Procs.Count - 50) more not shown" }
    }
    $bad = $false
    if ($daemon -gt 0) {
        Write-Host "ERROR: $daemon ta-daemon process(es) left running. A test started a daemon and did not shut it down."
        $bad = $true
    }
    if ($gh -gt $MaxGh) {
        Write-Host "ERROR: $gh gh processes left running (limit $MaxGh). A test is reaching the real gh or a gh wrapper is re-executing itself."
        $bad = $true
    }
    if ($ta -gt $MaxTa) {
        Write-Host "ERROR: $ta ta processes left running (limit $MaxTa). A test spawned ta and did not wait on or kill it."
        $bad = $true
    }
    if ($bad) {
        Write-Host "Fix: find the test that spawns the process (see CONTRIBUTING.md, 'Hermetic tests and the leftover-process check'), or raise -MaxGh / -MaxTa only with a reason."
    }
    return (-not $bad)
}

function New-Fake([string]$Name, [int]$Count) {
    1..$Count | ForEach-Object { [pscustomobject]@{ Id = $_; Name = $Name; Path = "fake\$Name.exe" } }
}

if ($SelfTest) {
    $failed = $false
    if (-not (Test-Leftovers -Procs @(New-Fake 'gh' 2) -MaxGh $MaxGh -MaxTa $MaxTa)) { Write-Host 'self-test FAIL: 2 gh must pass'; $failed = $true }
    if (Test-Leftovers -Procs @(New-Fake 'ta-daemon' 1) -MaxGh $MaxGh -MaxTa $MaxTa) { Write-Host 'self-test FAIL: ta-daemon must fail'; $failed = $true }
    if (Test-Leftovers -Procs @(New-Fake 'gh' 6) -MaxGh $MaxGh -MaxTa $MaxTa) { Write-Host 'self-test FAIL: 6 gh must fail'; $failed = $true }
    if ($failed) { exit 1 }
    Write-Host 'self-test OK'
    exit 0
}

Write-Host "Checking for leftover gh, ta-daemon and ta processes (this host: Windows)"
$procs = @(Get-Process -Name gh, ta-daemon, ta -ErrorAction SilentlyContinue)
if (Test-Leftovers -Procs $procs -MaxGh $MaxGh -MaxTa $MaxTa) { exit 0 } else { exit 1 }
