@echo off
setlocal
echo This restores or removes Adobe integration files installed by FastCull.
echo Photos, ratings and Adobe preferences are not removed.
choice /C YN /N /M "Continue? [Y/N] "
if errorlevel 2 exit /b 0
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\register-integrations.ps1" -Mode Uninstall
pause
