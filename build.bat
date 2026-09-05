@echo off
rem Install and configure the tools required to build Ruzu on Windows.

set "RUZU_ENV_FILE=%TEMP%\ruzu-windows-env-%RANDOM%-%RANDOM%.bat"
if exist "%RUZU_ENV_FILE%" del /q "%RUZU_ENV_FILE%"
if exist "%RUZU_ENV_FILE%" (
    echo Unable to remove the stale environment file:
    echo   %RUZU_ENV_FILE%
    exit /b 1
)

"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\build.ps1" -EnvironmentFile "%RUZU_ENV_FILE%" %*
set "RUZU_SETUP_EXIT=%ERRORLEVEL%"
if not "%RUZU_SETUP_EXIT%"=="0" (
    if exist "%RUZU_ENV_FILE%" del /q "%RUZU_ENV_FILE%" >nul 2>nul
    set "RUZU_ENV_FILE="
    exit /b %RUZU_SETUP_EXIT%
)

if not exist "%RUZU_ENV_FILE%" (
    echo The setup completed without writing its environment file:
    echo   %RUZU_ENV_FILE%
    exit /b 1
)

call "%RUZU_ENV_FILE%"
set "RUZU_SETUP_EXIT=%ERRORLEVEL%"
del /q "%RUZU_ENV_FILE%" >nul 2>nul
set "RUZU_ENV_FILE="
if not "%RUZU_SETUP_EXIT%"=="0" exit /b %RUZU_SETUP_EXIT%

cd /d "%~dp0"
set "RUZU_SETUP_EXIT="

if /i "%RUZU_BUILD_ACTION%"=="package" goto build_package

echo.
set "RUZU_PLATFORM=x86_64-pc-windows-msvc"
if /i "%RUZU_BUILD_PROFILE%"=="debug" (
    echo Building Ruzu with the debug profile...
    cargo build --locked --bin ruzu
    set "RUZU_CARGO_BINARY=target\debug\ruzu.exe"
    set "RUZU_OUTPUT_DIR=build\%RUZU_PLATFORM%\debug"
) else (
    echo Building Ruzu with the release profile...
    cargo build --locked --release --bin ruzu
    set "RUZU_CARGO_BINARY=target\release\ruzu.exe"
    set "RUZU_OUTPUT_DIR=build\%RUZU_PLATFORM%\release"
)
if errorlevel 1 exit /b %errorlevel%

if not exist "%RUZU_CARGO_BINARY%" (
    echo Cargo completed without producing the expected executable:
    echo   %RUZU_CARGO_BINARY%
    exit /b 1
)

set "RUZU_RUNTIME_BIN=%VCPKG_ROOT%\installed\%VCPKG_DEFAULT_TRIPLET%\bin"
if not exist "%RUZU_RUNTIME_BIN%\*.dll" (
    echo No vcpkg runtime DLL was found in:
    echo   %RUZU_RUNTIME_BIN%
    exit /b 1
)

tasklist /fi "IMAGENAME eq ruzu.exe" /nh 2>nul | find /i "ruzu.exe" >nul
if not errorlevel 1 (
    echo Ruzu is still running. Close it before refreshing:
    echo   %RUZU_OUTPUT_DIR%
    exit /b 1
)

echo Preparing the standalone Windows build...
if exist "%RUZU_OUTPUT_DIR%" rmdir /s /q "%RUZU_OUTPUT_DIR%"
if exist "%RUZU_OUTPUT_DIR%" (
    echo Unable to replace the existing standalone build:
    echo   %RUZU_OUTPUT_DIR%
    exit /b 1
)
mkdir "%RUZU_OUTPUT_DIR%"
if not exist "%RUZU_OUTPUT_DIR%" (
    echo Unable to create the standalone build directory:
    echo   %RUZU_OUTPUT_DIR%
    exit /b 1
)

copy /y "%RUZU_CARGO_BINARY%" "%RUZU_OUTPUT_DIR%\ruzu.exe" >nul
if errorlevel 1 exit /b %errorlevel%
copy /y "%RUZU_RUNTIME_BIN%\*.dll" "%RUZU_OUTPUT_DIR%\" >nul
if errorlevel 1 exit /b %errorlevel%

set "RUZU_BINARY=%RUZU_OUTPUT_DIR%\ruzu.exe"
echo.
echo Ruzu was built successfully:
echo   %RUZU_BINARY%
set "RUZU_BUILD_PROFILE="
set "RUZU_BUILD_ACTION="
set "RUZU_FORCE_PACKAGE="
set "RUZU_BINARY="
set "RUZU_CARGO_BINARY="
set "RUZU_OUTPUT_DIR="
set "RUZU_PLATFORM="
set "RUZU_RUNTIME_BIN="
exit /b 0

:build_package
if /i "%RUZU_BUILD_PROFILE%"=="debug" (
    echo.
    echo The NSIS package is only available for Release builds.
    echo Run build.bat package without -Debug.
    exit /b 1
)

echo.
echo Building the self-contained Windows package and NSIS installer...
if "%RUZU_FORCE_PACKAGE%"=="1" (
    echo WARNING: Git main-branch checks are disabled for this package.
    "%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0dist\package-windows.ps1" -Profile release -ForcePackage
) else (
    "%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0dist\package-windows.ps1" -Profile release
)
set "RUZU_PACKAGE_EXIT=%ERRORLEVEL%"
set "RUZU_BUILD_ACTION="
set "RUZU_BUILD_PROFILE="
set "RUZU_FORCE_PACKAGE="
exit /b %RUZU_PACKAGE_EXIT%
