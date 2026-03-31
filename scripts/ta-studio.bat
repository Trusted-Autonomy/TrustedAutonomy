@echo off
:: ta-studio.bat — Launch TA Studio in the default browser.
:: Starts ta-daemon if not running, then opens http://localhost:7700.

set PORT=7700
set MAX_WAIT=5

:check_daemon
curl -sf http://localhost:%PORT%/health >nul 2>&1
if %errorlevel% equ 0 goto open_browser

echo Starting TA daemon...
start /b ta daemon start --background >nul 2>&1

set WAIT=0
:wait_loop
timeout /t 1 /nobreak >nul
curl -sf http://localhost:%PORT%/health >nul 2>&1
if %errorlevel% equ 0 goto open_browser
set /a WAIT+=1
if %WAIT% lss %MAX_WAIT% goto wait_loop

powershell -Command "Add-Type -AssemblyName PresentationFramework; [System.Windows.MessageBox]::Show('TA daemon did not start within %MAX_WAIT% seconds. Run ''ta daemon start'' in a terminal to diagnose.', 'TA Studio', 'OK', 'Error')" >nul 2>&1
exit /b 1

:open_browser
start http://localhost:%PORT%
exit /b 0
