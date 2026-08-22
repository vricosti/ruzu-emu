@echo off
rem IN PROGRESS: install and configure the tools required to build Ruzu on Windows.

set "RUZU_ENV_FILE=%TEMP%\ruzu-windows-env.bat"

powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\build.ps1" %*
if errorlevel 1 exit /b %errorlevel%

if exist "%RUZU_ENV_FILE%" call "%RUZU_ENV_FILE%"

echo.
echo The current Command Prompt is ready to build Ruzu:
echo.
echo   cargo build --locked --bin ruzu
echo   target\debug\ruzu.exe
