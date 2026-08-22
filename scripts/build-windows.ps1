#Requires -Version 5.1

<#
.SYNOPSIS
Installs and verifies the native Windows dependencies required to build Ruzu.

.DESCRIPTION
The script uses the native x64 MSVC toolchain. Rust is installed exclusively
through rustup. GTK4, FFmpeg, OpenSSL, Vulkan, glslang, and pkgconf are built
and managed by vcpkg. Cargo builds SDL3 statically from source.
#>

[CmdletBinding()]
param(
    [switch]$Yes
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$RustMinimum = [version]"1.85.0"
$RustToolchain = "stable-x86_64-pc-windows-msvc"
$VcpkgTriplet = "x64-windows-ruzu"
$ScriptDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Split-Path -Parent $ScriptDirectory
$EnvironmentBatch = Join-Path $env:TEMP "ruzu-windows-env.bat"
$VcpkgOverlayTriplets = Join-Path $ScriptDirectory "vcpkg-triplets"
$CmakeWrapper = Join-Path $ScriptDirectory "cmake-clean-env.cmd"
$RequestedVcpkgRoot = $env:VCPKG_ROOT

$VcpkgPackages = @(
    "gtk:$VcpkgTriplet"
    "ffmpeg[avcodec]:$VcpkgTriplet"
    "openssl:$VcpkgTriplet"
    "vulkan:$VcpkgTriplet"
    "glslang[tools]:$VcpkgTriplet"
    "pkgconf:$VcpkgTriplet"
)

function Confirm-Install {
    param([Parameter(Mandatory)][string]$Prompt)

    if ($Yes -or $env:RUZU_SETUP_ASSUME_YES -eq "1") {
        return $true
    }

    $answer = Read-Host "$Prompt [y/N]"
    return $answer -match "^(?i:y|yes)$"
}

function Invoke-Download {
    param(
        [Parameter(Mandatory)][string]$Uri,
        [Parameter(Mandatory)][string]$Destination
    )

    Write-Host "Downloading $Uri"
    Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $Destination
}

function Set-CurrentPath {
    param([Parameter(Mandatory)][string]$Value)

    $pathKeys = @(
        [Environment]::GetEnvironmentVariables("Process").Keys |
            Where-Object { $_ -ieq "Path" }
    )
    foreach ($key in $pathKeys) {
        [Environment]::SetEnvironmentVariable([string]$key, $null, "Process")
    }
    [Environment]::SetEnvironmentVariable("Path", $Value, "Process")
}

function Refresh-CommandPath {
    $machinePath = [Environment]::GetEnvironmentVariable("Path", "Machine")
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    Set-CurrentPath -Value "$machinePath;$userPath"
}

function Add-CurrentPath {
    param([Parameter(Mandatory)][string[]]$Entries)

    foreach ($entry in $Entries) {
        if ($entry -and (Test-Path $entry)) {
            $currentPath = [Environment]::GetEnvironmentVariable("Path", "Process")
            $parts = $currentPath -split ";" | Where-Object { $_ }
            if ($parts -notcontains $entry) {
                Set-CurrentPath -Value "$entry;$currentPath"
            }
        }
    }
}

function Add-UserPath {
    param([Parameter(Mandatory)][string[]]$Entries)

    $current = [Environment]::GetEnvironmentVariable("Path", "User")
    $parts = @($current -split ";" | Where-Object { $_ })
    foreach ($entry in $Entries) {
        if ($entry -and (Test-Path $entry) -and $parts -notcontains $entry) {
            $parts += $entry
        }
    }
    [Environment]::SetEnvironmentVariable("Path", ($parts -join ";"), "User")
}

function Get-VSWherePath {
    $candidates = @(
        (Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe")
        (Join-Path $env:ProgramFiles "Microsoft Visual Studio\Installer\vswhere.exe")
    )
    return $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
}

function Get-VSInstallation {
    $vswhere = Get-VSWherePath
    if (-not $vswhere) {
        return $null
    }

    $json = & $vswhere `
        -products "*" `
        -version "[17.0,)" `
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
        -format json `
        -utf8
    if ($LASTEXITCODE -ne 0 -or -not $json) {
        return $null
    }

    $installations = @($json | ConvertFrom-Json)
    return $installations |
        Where-Object { [version]$_.installationVersion -ge [version]"17.0" } |
        Sort-Object { [version]$_.installationVersion } -Descending |
        Select-Object -First 1
}

function Get-VSInstallPath {
    $installation = Get-VSInstallation
    if (-not $installation) {
        return $null
    }
    return $installation.installationPath
}

function Import-VSDevEnvironment {
    param([Parameter(Mandatory)][string]$VSInstallPath)

    $vsDevCmd = Join-Path $VSInstallPath "Common7\Tools\VsDevCmd.bat"
    if (-not (Test-Path $vsDevCmd)) {
        throw "Visual Studio developer environment was not found: $vsDevCmd"
    }

    $command = "`"$vsDevCmd`" -no_logo -arch=x64 -host_arch=x64 >nul && set"
    $environmentLines = & $env:ComSpec /s /c $command
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to initialize the Visual Studio developer environment."
    }

    foreach ($line in $environmentLines) {
        $separator = $line.IndexOf("=")
        if ($separator -gt 0) {
            $name = $line.Substring(0, $separator)
            $value = $line.Substring($separator + 1)
            if ($name -ieq "Path") {
                Set-CurrentPath -Value $value
            }
            else {
                [Environment]::SetEnvironmentVariable($name, $value, "Process")
            }
        }
    }
}

function Install-VisualStudioBuildTools {
    param(
        [Parameter(Mandatory)]
        [ValidateSet("2026", "2022")]
        [string]$Generation
    )

    $installerUri = if ($Generation -eq "2026") {
        # Keep this on the Visual Studio 2026 stable channel.
        "https://aka.ms/vs/18/stable/vs_buildtools.exe"
    }
    else {
        "https://aka.ms/vs/17/release/vs_BuildTools.exe"
    }
    $installer = Join-Path $env:TEMP "vs_BuildTools-$Generation.exe"
    Invoke-Download `
        -Uri $installerUri `
        -Destination $installer

    $arguments = @(
        "--quiet"
        "--wait"
        "--norestart"
        "--nocache"
        "--add"
        "Microsoft.VisualStudio.Workload.VCTools"
        "--add"
        "Microsoft.VisualStudio.Component.VC.CMake.Project"
        "--includeRecommended"
    )
    $process = Start-Process `
        -FilePath $installer `
        -ArgumentList $arguments `
        -Verb RunAs `
        -Wait `
        -PassThru
    if ($process.ExitCode -notin 0, 3010) {
        throw "Visual Studio $Generation Build Tools installation failed with exit code $($process.ExitCode)."
    }
}

function Install-Git {
    $winget = Get-Command winget.exe -ErrorAction SilentlyContinue
    if ($winget) {
        & $winget.Source install `
            --id Git.Git `
            --exact `
            --silent `
            --accept-package-agreements `
            --accept-source-agreements
        if ($LASTEXITCODE -ne 0) {
            throw "Git installation through winget failed."
        }
        return
    }

    Write-Host "winget is unavailable; downloading the current Git for Windows installer."
    $release = Invoke-RestMethod `
        -UseBasicParsing `
        -Uri "https://api.github.com/repos/git-for-windows/git/releases/latest"
    $asset = $release.assets |
        Where-Object { $_.name -match "64-bit\.exe$" -and $_.name -notmatch "PortableGit|MinGit" } |
        Select-Object -First 1
    if (-not $asset) {
        throw "Unable to locate the current 64-bit Git for Windows installer."
    }

    $installer = Join-Path $env:TEMP "Git-64-bit.exe"
    Invoke-Download -Uri $asset.browser_download_url -Destination $installer
    $process = Start-Process `
        -FilePath $installer `
        -ArgumentList "/VERYSILENT", "/NORESTART", "/NOCANCEL", "/SP-" `
        -Verb RunAs `
        -Wait `
        -PassThru
    if ($process.ExitCode -ne 0) {
        throw "Git installation failed with exit code $($process.ExitCode)."
    }
}

function Ensure-WindowsBuildTools {
    $installation = Get-VSInstallation
    if ($installation) {
        Write-Host (
            "[OK] Visual Studio $($installation.installationVersion) with C++ tools is installed."
        )
    }
    else {
        Write-Host "[MISSING] Visual Studio 2022 or newer with C++ tools is not installed."
        if (-not (Confirm-Install "Install Visual Studio 2026 Build Tools with the C++ workload?")) {
            throw "Visual Studio Build Tools installation was declined."
        }

        try {
            Install-VisualStudioBuildTools -Generation "2026"
        }
        catch {
            Write-Warning "Visual Studio 2026 installation failed: $($_.Exception.Message)"
            if (-not (Confirm-Install "Install Visual Studio 2022 Build Tools instead?")) {
                throw "Visual Studio 2022 fallback installation was declined."
            }
            Install-VisualStudioBuildTools -Generation "2022"
        }

        $installation = Get-VSInstallation
        if (-not $installation) {
            throw "Visual Studio 2022 or newer is unavailable after installation."
        }
        Write-Host (
            "[OK] Installed Visual Studio $($installation.installationVersion) with C++ tools."
        )
    }

    if (-not (Get-Command git.exe -ErrorAction SilentlyContinue)) {
        Write-Host "[MISSING] Git for Windows is not installed."
        if (-not (Confirm-Install "Install Git for Windows?")) {
            throw "Git installation was declined."
        }
        Install-Git
    }
    else {
        Write-Host "[OK] Git for Windows is installed."
    }

    Refresh-CommandPath
    Add-CurrentPath @(
        (Join-Path $env:ProgramFiles "Git\cmd")
        (Join-Path ${env:ProgramFiles(x86)} "Git\cmd")
    )

    if (-not (Get-Command git.exe -ErrorAction SilentlyContinue)) {
        throw "Git is unavailable after installation."
    }
}

function Get-RustVersion {
    $rustc = Get-Command rustc.exe -ErrorAction SilentlyContinue
    $cargo = Get-Command cargo.exe -ErrorAction SilentlyContinue
    if (-not $rustc -or -not $cargo) {
        return $null
    }

    $versionOutput = & $rustc.Source --version
    if ($versionOutput -match "^rustc ([0-9]+\.[0-9]+\.[0-9]+)") {
        return [version]$Matches[1]
    }
    return $null
}

function Install-Rustup {
    $installer = Join-Path $env:TEMP "rustup-init.exe"
    Invoke-Download -Uri "https://win.rustup.rs/x86_64" -Destination $installer
    & $installer -y --profile minimal --default-toolchain $RustToolchain
    if ($LASTEXITCODE -ne 0) {
        throw "rustup installation failed."
    }
}

function Ensure-Rust {
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    Add-CurrentPath @($cargoBin)

    $rustup = Get-Command rustup.exe -ErrorAction SilentlyContinue
    $version = Get-RustVersion
    $needsRustup = -not $rustup
    $needsToolchain = $true
    if ($rustup) {
        $toolchains = & $rustup.Source toolchain list
        $needsToolchain = -not ($toolchains -match [regex]::Escape($RustToolchain))
    }

    if (-not $needsRustup -and -not $needsToolchain -and
        $version -and $version -ge $RustMinimum) {
        Write-Host "[OK] Rust $version, Cargo, and rustup are installed."
    }
    else {
        if ($needsRustup) {
            Write-Host "[MISSING] rustup is not installed."
        }
        elseif ($needsToolchain) {
            Write-Host "[MISSING] The $RustToolchain toolchain is not installed."
        }
        elseif (-not $version -or $version -lt $RustMinimum) {
            Write-Host "[MISSING] Rust $version is older than $RustMinimum."
        }

        if (-not (Confirm-Install "Install the stable MSVC Rust toolchain with rustup?")) {
            throw "Rust installation was declined."
        }

        if ($needsRustup) {
            Install-Rustup
            Add-CurrentPath @($cargoBin)
            $rustup = Get-Command rustup.exe -ErrorAction Stop
        }
        & $rustup.Source toolchain install $RustToolchain --profile minimal
        if ($LASTEXITCODE -ne 0) {
            throw "The Rust MSVC toolchain installation failed."
        }
    }

    Push-Location $ProjectRoot
    try {
        & rustup.exe override set $RustToolchain
        if ($LASTEXITCODE -ne 0) {
            throw "Unable to select the Rust MSVC toolchain for this project."
        }
    }
    finally {
        Pop-Location
    }
}

function Get-MissingVcpkgPackages {
    param([Parameter(Mandatory)][string]$VcpkgExecutable)

    $installed = & $VcpkgExecutable list --triplet $VcpkgTriplet
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to query installed vcpkg packages."
    }

    $requiredNames = @("gtk", "ffmpeg", "openssl", "vulkan", "glslang", "pkgconf")
    return @($requiredNames | Where-Object {
        $pattern = "^$([regex]::Escape($_)):$([regex]::Escape($VcpkgTriplet))\s"
        -not ($installed -match $pattern)
    })
}

function Find-VcpkgRoot {
    $candidates = @()
    if ($RequestedVcpkgRoot) {
        $candidates += $RequestedVcpkgRoot
    }

    # Prefer Ruzu's standalone installation over vcpkg instances injected into
    # PATH by Visual Studio's developer environment.
    $candidates += Join-Path $env:LOCALAPPDATA "Ruzu\vcpkg"

    $command = Get-Command vcpkg.exe -ErrorAction SilentlyContinue
    if ($command) {
        $candidates += Split-Path -Parent $command.Source
    }

    foreach ($candidate in $candidates | Select-Object -Unique) {
        $executable = Join-Path $candidate "vcpkg.exe"
        $toolchain = Join-Path $candidate "scripts\buildsystems\vcpkg.cmake"
        $visualStudioBundle = Join-Path $candidate "vcpkg-bundle.json"
        if ((Test-Path $executable) -and
            (Test-Path $toolchain) -and
            -not (Test-Path $visualStudioBundle)) {
            return $candidate
        }
    }

    return $null
}

function Ensure-VcpkgDependencies {
    $vcpkgRoot = Find-VcpkgRoot
    if (-not $vcpkgRoot) {
        $vcpkgRoot = Join-Path $env:LOCALAPPDATA "Ruzu\vcpkg"
    }
    $vcpkgExecutable = Join-Path $vcpkgRoot "vcpkg.exe"

    $missingVcpkg = -not (Test-Path $vcpkgExecutable)
    $missingPackages = @()
    if (-not $missingVcpkg) {
        $env:VCPKG_ROOT = $vcpkgRoot
        Write-Host "[OK] Found an existing vcpkg installation in $vcpkgRoot."
        $missingPackages = @(Get-MissingVcpkgPackages -VcpkgExecutable $vcpkgExecutable)
    }

    if ($missingVcpkg -or $missingPackages.Count -gt 0) {
        if ($missingVcpkg) {
            Write-Host "[MISSING] vcpkg is not installed in $vcpkgRoot."
        }
        foreach ($package in $missingPackages) {
            Write-Host "  [MISSING] $package`:$VcpkgTriplet"
        }

        if (-not (Confirm-Install "Install the missing native libraries with vcpkg?")) {
            throw "Native library installation was declined."
        }

        if ($missingVcpkg) {
            $parent = Split-Path -Parent $vcpkgRoot
            New-Item -ItemType Directory -Path $parent -Force | Out-Null
            & git.exe clone --depth 1 https://github.com/microsoft/vcpkg.git $vcpkgRoot
            if ($LASTEXITCODE -ne 0) {
                throw "Unable to clone vcpkg."
            }

            & (Join-Path $vcpkgRoot "bootstrap-vcpkg.bat") -disableMetrics
            if ($LASTEXITCODE -ne 0) {
                throw "Unable to bootstrap vcpkg."
            }
        }

        $vcpkgExecutable = Join-Path $vcpkgRoot "vcpkg.exe"
        $env:VCPKG_ROOT = $vcpkgRoot
        $installArguments = @(
            "install"
        ) + $VcpkgPackages + @(
            "--host-triplet=$VcpkgTriplet"
            "--overlay-triplets=$VcpkgOverlayTriplets"
            "--disable-metrics"
        )
        & $vcpkgExecutable @installArguments
        if ($LASTEXITCODE -ne 0) {
            throw "vcpkg dependency installation failed."
        }
    }
    else {
        Write-Host "[OK] All required vcpkg libraries are installed."
    }

    return $vcpkgRoot
}

function Configure-NativeEnvironment {
    param(
        [Parameter(Mandatory)][string]$VcpkgRoot,
        [Parameter(Mandatory)][string]$VSInstallPath
    )

    $installed = Join-Path $VcpkgRoot "installed\$VcpkgTriplet"
    $pkgConfig = Get-ChildItem `
        -Path (Join-Path $installed "tools\pkgconf") `
        -Filter "pkgconf.exe" `
        -File `
        -Recurse |
        Select-Object -First 1
    if (-not $pkgConfig) {
        throw "The pkgconf executable installed by vcpkg was not found."
    }

    $pkgConfigPath = @(
        (Join-Path $installed "lib\pkgconfig")
        (Join-Path $installed "share\pkgconfig")
    ) -join ";"
    $pathEntries = @(
        (Join-Path $env:USERPROFILE ".cargo\bin")
        (Join-Path $installed "bin")
        $pkgConfig.DirectoryName
        (Join-Path $installed "tools\glslang")
    )
    Add-CurrentPath $pathEntries
    $cmakeExecutable = (Get-Command cmake.exe -ErrorAction Stop).Source
    $gsettingsSchemaDirectory = Join-Path $installed "share\glib-2.0\schemas"
    $glibCompileSchemas = Join-Path $installed "tools\glib\glib-compile-schemas.exe"
    if (-not (Test-Path $glibCompileSchemas)) {
        throw "The GLib schema compiler installed by vcpkg was not found."
    }
    & $glibCompileSchemas $gsettingsSchemaDirectory
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to compile GTK's GSettings schemas."
    }

    $env:CMAKE = $CmakeWrapper
    $env:RUZU_CMAKE_EXE = $cmakeExecutable
    $env:VCPKG_ROOT = $VcpkgRoot
    $env:VCPKG_DEFAULT_TRIPLET = $VcpkgTriplet
    $env:VCPKGRS_TRIPLET = $VcpkgTriplet
    $env:VCPKGRS_DYNAMIC = "1"
    $env:PKG_CONFIG = $pkgConfig.FullName
    $env:PKG_CONFIG_PATH = $pkgConfigPath
    $env:OPENSSL_DIR = $installed
    $env:GSETTINGS_SCHEMA_DIR = $gsettingsSchemaDirectory

    [Environment]::SetEnvironmentVariable("CMAKE", $CmakeWrapper, "User")
    [Environment]::SetEnvironmentVariable("RUZU_CMAKE_EXE", $cmakeExecutable, "User")
    [Environment]::SetEnvironmentVariable("VCPKG_ROOT", $VcpkgRoot, "User")
    [Environment]::SetEnvironmentVariable("VCPKG_DEFAULT_TRIPLET", $VcpkgTriplet, "User")
    [Environment]::SetEnvironmentVariable("VCPKGRS_TRIPLET", $VcpkgTriplet, "User")
    [Environment]::SetEnvironmentVariable("VCPKGRS_DYNAMIC", "1", "User")
    [Environment]::SetEnvironmentVariable("PKG_CONFIG", $pkgConfig.FullName, "User")
    [Environment]::SetEnvironmentVariable("PKG_CONFIG_PATH", $pkgConfigPath, "User")
    [Environment]::SetEnvironmentVariable("OPENSSL_DIR", $installed, "User")
    [Environment]::SetEnvironmentVariable(
        "GSETTINGS_SCHEMA_DIR",
        $gsettingsSchemaDirectory,
        "User"
    )
    Add-UserPath $pathEntries

    $vsDevCmd = Join-Path $VSInstallPath "Common7\Tools\VsDevCmd.bat"
    $batchLines = @(
        "@echo off"
        "call `"$vsDevCmd`" -no_logo -arch=x64 -host_arch=x64"
        "set `"CMAKE=$CmakeWrapper`""
        "set `"RUZU_CMAKE_EXE=$cmakeExecutable`""
        "set `"VCPKG_ROOT=$VcpkgRoot`""
        "set `"VCPKG_DEFAULT_TRIPLET=$VcpkgTriplet`""
        "set `"VCPKGRS_TRIPLET=$VcpkgTriplet`""
        "set `"VCPKGRS_DYNAMIC=1`""
        "set `"PKG_CONFIG=$($pkgConfig.FullName)`""
        "set `"PKG_CONFIG_PATH=$pkgConfigPath`""
        "set `"OPENSSL_DIR=$installed`""
        "set `"GSETTINGS_SCHEMA_DIR=$gsettingsSchemaDirectory`""
        "set `"PATH=$($pathEntries -join ';');%PATH%`""
    )
    Set-Content -Path $EnvironmentBatch -Value $batchLines -Encoding ASCII
}

function Verify-NativeDependencies {
    $checks = @(
        [pscustomobject]@{ Package = "gtk4"; Minimum = "4.6" }
        [pscustomobject]@{ Package = "libavcodec"; Minimum = $null }
        [pscustomobject]@{ Package = "libavutil"; Minimum = $null }
        [pscustomobject]@{ Package = "openssl"; Minimum = $null }
        [pscustomobject]@{ Package = "vulkan"; Minimum = $null }
    )

    foreach ($check in $checks) {
        $package = $check.Package
        $minimum = $check.Minimum
        if ($minimum) {
            & $env:PKG_CONFIG "--atleast-version=$minimum" $package
        }
        else {
            & $env:PKG_CONFIG --exists $package
        }
        if ($LASTEXITCODE -ne 0) {
            throw "pkgconf could not find the required package: $package"
        }
    }

    $gtkVersion = & $env:PKG_CONFIG --modversion gtk4
    Write-Host "[OK] GTK $gtkVersion is available through vcpkg; Cargo builds SDL3 from source."

    foreach ($command in @("cl.exe", "cmake.exe", "ninja.exe", "glslangValidator.exe")) {
        if (-not (Get-Command $command -ErrorAction SilentlyContinue)) {
            throw "Required build command is unavailable: $command"
        }
    }
}

if ($PSVersionTable.PSEdition -eq "Core" -and -not $IsWindows) {
    throw "This build script only supports Windows."
}
if (-not [Environment]::Is64BitOperatingSystem) {
    throw "Ruzu requires a 64-bit Windows installation."
}

$windows = Get-CimInstance Win32_OperatingSystem
Write-Host "Detected platform: $($windows.Caption) $($windows.Version)"
Write-Host "Using native MSVC libraries through vcpkg ($VcpkgTriplet)."

Ensure-WindowsBuildTools
$vsInstallPath = Get-VSInstallPath
Import-VSDevEnvironment -VSInstallPath $vsInstallPath
Ensure-Rust
$vcpkgRoot = Ensure-VcpkgDependencies
Configure-NativeEnvironment -VcpkgRoot $vcpkgRoot -VSInstallPath $vsInstallPath
Verify-NativeDependencies

Write-Host ""
Write-Host "Dependency check completed on $($windows.Caption)."
Write-Host "Rust  : $(& rustc.exe --version)"
Write-Host "Cargo : $(& cargo.exe --version)"
Write-Host "vcpkg : $vcpkgRoot"
Write-Host "All required dependencies are available."
Write-Host ""
Write-Host "Build Ruzu from this Command Prompt with:"
Write-Host ""
Write-Host "  cargo build --locked --bin ruzu"
Write-Host "  target\debug\ruzu.exe"
