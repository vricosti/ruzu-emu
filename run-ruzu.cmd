@echo off
setlocal EnableExtensions

set "RUZU_ROOT=%~dp0"
set "RUZU_EXE=%RUZU_ROOT%target\release\ruzu.exe"
set "RUZU_TRIPLET=x64-windows-ruzu"

if not defined VCPKG_ROOT (
    for /f "tokens=2,*" %%A in ('reg query HKCU\Environment /v VCPKG_ROOT 2^>nul') do (
        set "VCPKG_ROOT=%%B"
    )
)
if not defined VCPKG_ROOT set "VCPKG_ROOT=%LOCALAPPDATA%\Ruzu\vcpkg"

set "RUZU_DLL_DIR=%VCPKG_ROOT%\installed\%RUZU_TRIPLET%\bin"
if not exist "%RUZU_DLL_DIR%\gtk-4-1.dll" (
    echo Ruzu's vcpkg runtime DLL directory was not found:
    echo   %RUZU_DLL_DIR%
    echo.
    echo Run scripts\build.ps1 first, or set VCPKG_ROOT to the vcpkg installation.
    pause
    exit /b 1
)

set "PATH=%RUZU_DLL_DIR%;%PATH%"
set "GSETTINGS_SCHEMA_DIR=%VCPKG_ROOT%\installed\%RUZU_TRIPLET%\share\glib-2.0\schemas"
set "RUZU_SCHEMA_COMPILER=%VCPKG_ROOT%\installed\%RUZU_TRIPLET%\tools\glib\glib-compile-schemas.exe"
if not exist "%GSETTINGS_SCHEMA_DIR%\gschemas.compiled" (
    if not exist "%RUZU_SCHEMA_COMPILER%" (
        echo GLib's schema compiler was not found:
        echo   %RUZU_SCHEMA_COMPILER%
        pause
        exit /b 1
    )
    "%RUZU_SCHEMA_COMPILER%" "%GSETTINGS_SCHEMA_DIR%"
    if errorlevel 1 (
        echo Failed to compile GTK's GSettings schemas.
        pause
        exit /b 1
    )
)

if not exist "%RUZU_EXE%" (
    echo The release executable was not found:
    echo   %RUZU_EXE%
    echo.
    echo Build it with: cargo build --release --bin ruzu
    pause
    exit /b 1
)

"%RUZU_EXE%" %*
set "RUZU_EXIT_CODE=%ERRORLEVEL%"

if not "%RUZU_EXIT_CODE%"=="0" (
    echo.
    echo Ruzu exited with code %RUZU_EXIT_CODE%.
    pause
)
exit /b %RUZU_EXIT_CODE%
