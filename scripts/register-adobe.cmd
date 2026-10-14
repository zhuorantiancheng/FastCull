@echo off
setlocal
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\register-integrations.ps1" -Bridge -Lightroom
if errorlevel 1 echo Registration failed. See QUICKSTART.txt.
pause
