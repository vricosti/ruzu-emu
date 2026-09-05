#Requires -Version 5.1

<#
.SYNOPSIS
Builds a self-contained Ruzu Windows directory and NSIS installer.

.DESCRIPTION
The script stages the Rust executables, the dynamic vcpkg runtime, GTK/GLib
data files, licenses and documentation under target\package. It then invokes
the Ruzu NSIS installer definition in this directory.

Run build.bat once before this script so the x64 MSVC and vcpkg environment is
available. Cargo builds SDL3 statically; GTK, FFmpeg, OpenSSL and their runtime
DLLs come from the x64-windows-ruzu vcpkg triplet.
#>

[CmdletBinding()]
param(
    [string]$Version,
    [ValidateSet("release", "release-lto")]
    [string]$Profile = "release",
    [switch]$SkipBuild,
    [switch]$StageOnly,
    [switch]$ValidateOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$DistDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Split-Path -Parent $DistDirectory
$Triplet = "x64-windows-ruzu"
$Architecture = "x64"
$Variant = "msvc"
$InstallerScript = Join-Path $DistDirectory "installer.nsi"
$Manifest = Join-Path $DistDirectory "ruzu.manifest"
$Icon = Join-Path $DistDirectory "ruzu.ico"

function Get-WorkspaceVersion {
    $cargoManifest = Get-Content -LiteralPath (Join-Path $ProjectRoot "Cargo.toml") -Raw
    $match = [regex]::Match(
        $cargoManifest,
        '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"'
    )
    if (-not $match.Success) {
        throw "Unable to read workspace.package.version from Cargo.toml."
    }
    return $match.Groups[1].Value
}

function Assert-PackagingSources {
    foreach ($path in @(
        $InstallerScript,
        $Manifest,
        $Icon,
        (Join-Path $DistDirectory "ruzu.rc"),
        (Join-Path $ProjectRoot "LICENSE"),
        (Join-Path $ProjectRoot "README.md")
    )) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Required packaging source is missing: $path"
        }
    }

    [void][xml](Get-Content -LiteralPath $Manifest -Raw)
    $iconHeader = [System.IO.File]::ReadAllBytes($Icon)
    if ($iconHeader.Length -lt 4 -or
        $iconHeader[0] -ne 0 -or $iconHeader[1] -ne 0 -or
        $iconHeader[2] -ne 1 -or $iconHeader[3] -ne 0) {
        throw "dist\ruzu.ico is not a Windows ICO file."
    }

    $packagingText = @(
        Get-Content -LiteralPath $InstallerScript -Raw
        Get-Content -LiteralPath $Manifest -Raw
        Get-Content -LiteralPath (Join-Path $DistDirectory "ruzu.rc") -Raw
    ) -join "`n"
    if ($packagingText -match '(?i)eden|yuzu') {
        throw "Windows packaging sources still contain an Eden or Yuzu product reference."
    }
    if ($packagingText -notmatch 'ruzu\.exe') {
        throw "The installer does not reference ruzu.exe."
    }
}

function Resolve-VcpkgRoot {
    $candidates = @()
    if ($env:VCPKG_ROOT) {
        $candidates += $env:VCPKG_ROOT
    }
    if ($env:LOCALAPPDATA) {
        $candidates += Join-Path $env:LOCALAPPDATA "Ruzu\vcpkg"
    }

    foreach ($candidate in $candidates | Select-Object -Unique) {
        if (Test-Path -LiteralPath (Join-Path $candidate "installed\$Triplet")) {
            return $candidate
        }
    }
    throw "The $Triplet vcpkg tree was not found. Run build.bat first."
}

function Resolve-MakeNsis {
    $command = Get-Command makensis.exe -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    $candidates = @()
    if (${env:ProgramFiles(x86)}) {
        $candidates += Join-Path ${env:ProgramFiles(x86)} "NSIS\makensis.exe"
    }
    if ($env:ProgramFiles) {
        $candidates += Join-Path $env:ProgramFiles "NSIS\makensis.exe"
    }
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return $candidate
        }
    }
    throw "makensis.exe was not found. Install NSIS 3 and retry."
}

function Assert-MainBranch {
    param(
        [Parameter(Mandatory)][string]$Repository,
        [Parameter(Mandatory)][string]$DisplayName,
        [Parameter(Mandatory)][string]$GitExecutable
    )

    $branch = & $GitExecutable -C $Repository branch --show-current
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to determine the current Git branch for $DisplayName."
    }
    $branch = ($branch -join "`n").Trim()
    if ($branch -ne "main") {
        $actual = if ($branch) { $branch } else { "detached HEAD" }
        throw "$DisplayName must be checked out on branch main before packaging (current: $actual)."
    }
}

function Assert-ReleaseBranches {
    $git = Get-Command git.exe -ErrorAction SilentlyContinue
    if (-not $git) {
        throw "git.exe was not found; Git is required to verify release branches."
    }

    Assert-MainBranch `
        -Repository $ProjectRoot `
        -DisplayName "Ruzu" `
        -GitExecutable $git.Source

    $submoduleEntries = @(
        & $git.Source -C $ProjectRoot config --file .gitmodules --get-regexp '^submodule\..*\.path$'
    )
    if ($LASTEXITCODE -gt 1) {
        throw "Unable to read Ruzu's Git submodule configuration."
    }

    foreach ($entry in $submoduleEntries) {
        $parts = $entry -split '\s+', 2
        if ($parts.Count -ne 2) {
            throw "Invalid Git submodule entry: $entry"
        }
        $relativePath = $parts[1]
        $repository = Join-Path $ProjectRoot $relativePath
        $status = @(& $git.Source -C $ProjectRoot submodule status -- $relativePath)
        if ($LASTEXITCODE -ne 0 -or $status.Count -ne 1) {
            throw "Unable to inspect Git submodule $relativePath."
        }
        if ($status[0].StartsWith("-")) {
            throw "Git submodule $relativePath must be initialized before packaging."
        }
        if ($status[0].StartsWith("+") -or $status[0].StartsWith("U")) {
            throw "Git submodule $relativePath must match the commit recorded by Ruzu before packaging."
        }
        Assert-MainBranch `
            -Repository $repository `
            -DisplayName "Git submodule $relativePath" `
            -GitExecutable $git.Source
    }
}

function Copy-RuntimeTree {
    param(
        [Parameter(Mandatory)][string]$Source,
        [Parameter(Mandatory)][string]$Destination
    )

    if (-not (Test-Path -LiteralPath $Source -PathType Container)) {
        return
    }
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    Get-ChildItem -LiteralPath $Source -Force |
        Copy-Item -Destination $Destination -Recurse -Force
}

function Copy-RequiredFile {
    param(
        [Parameter(Mandatory)][string]$Source,
        [Parameter(Mandatory)][string]$Destination
    )
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        throw "Required build output is missing: $Source"
    }
    Copy-Item -LiteralPath $Source -Destination $Destination -Force
}

Assert-PackagingSources
if (-not $Version) {
    $Version = Get-WorkspaceVersion
}
if ($ValidateOnly) {
    Write-Host "Windows packaging sources are valid for Ruzu $Version."
    return
}

$isWindowsPlatform = if ($PSVersionTable.PSEdition -eq "Core") {
    $IsWindows
}
else {
    [Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT
}
if (-not $isWindowsPlatform) {
    throw "Ruzu's Windows package must be built on Windows with the MSVC toolchain."
}

Assert-ReleaseBranches

$makeNsis = if (-not $StageOnly) {
    Resolve-MakeNsis
}
else {
    $null
}

$vcpkgRoot = Resolve-VcpkgRoot
$vcpkgInstalled = Join-Path $vcpkgRoot "installed\$Triplet"
if (-not $env:VCPKGRS_TRIPLET) {
    $env:VCPKGRS_TRIPLET = $Triplet
}
if (-not $env:VCPKGRS_DYNAMIC) {
    $env:VCPKGRS_DYNAMIC = "1"
}

if (-not $SkipBuild) {
    Push-Location $ProjectRoot
    try {
        & cargo.exe build --locked --profile $Profile -p ruzu -p ruzu_cmd
        if ($LASTEXITCODE -ne 0) {
            throw "Cargo failed to build the Ruzu Windows binaries."
        }
    }
    finally {
        Pop-Location
    }
}

$targetRoot = if ($env:CARGO_TARGET_DIR) {
    if ([System.IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) {
        $env:CARGO_TARGET_DIR
    }
    else {
        Join-Path $ProjectRoot $env:CARGO_TARGET_DIR
    }
}
else {
    Join-Path $ProjectRoot "target"
}
$binaryDirectory = Join-Path $targetRoot $Profile
$packageRoot = Join-Path $targetRoot "package"
$stageDirectory = Join-Path $packageRoot "Ruzu-Windows-$Version-$Architecture-$Variant"
$outputDirectory = $packageRoot

if (Test-Path -LiteralPath $stageDirectory) {
    Remove-Item -LiteralPath $stageDirectory -Recurse -Force
}
New-Item -ItemType Directory -Path $stageDirectory -Force | Out-Null

Copy-RequiredFile `
    -Source (Join-Path $binaryDirectory "ruzu.exe") `
    -Destination (Join-Path $stageDirectory "ruzu.exe")
Copy-RequiredFile `
    -Source (Join-Path $binaryDirectory "ruzu-cmd.exe") `
    -Destination (Join-Path $stageDirectory "ruzu-cmd.exe")
Copy-RequiredFile `
    -Source (Join-Path $ProjectRoot "LICENSE") `
    -Destination (Join-Path $stageDirectory "LICENSE.txt")
Copy-RequiredFile `
    -Source (Join-Path $ProjectRoot "README.md") `
    -Destination (Join-Path $stageDirectory "README.md")

$vcpkgBin = Join-Path $vcpkgInstalled "bin"
$runtimeDlls = @(Get-ChildItem -LiteralPath $vcpkgBin -Filter "*.dll" -File)
if ($runtimeDlls.Count -eq 0) {
    throw "No runtime DLLs were found in $vcpkgBin."
}
$runtimeDlls | Copy-Item -Destination $stageDirectory -Force

# Preserve DLLs emitted beside Cargo binaries by native Rust build scripts.
Get-ChildItem -LiteralPath $binaryDirectory -Filter "*.dll" -File -ErrorAction SilentlyContinue |
    Copy-Item -Destination $stageDirectory -Force

$runtimeTrees = @(
    @("share\glib-2.0\schemas", "share\glib-2.0\schemas"),
    @("share\gtk-4.0", "share\gtk-4.0"),
    @("share\icons\Adwaita", "share\icons\Adwaita"),
    @("share\mime", "share\mime"),
    @("share\themes", "share\themes"),
    @("lib\gdk-pixbuf-2.0", "lib\gdk-pixbuf-2.0"),
    @("lib\gtk-4.0", "lib\gtk-4.0")
)
foreach ($tree in $runtimeTrees) {
    Copy-RuntimeTree `
        -Source (Join-Path $vcpkgInstalled $tree[0]) `
        -Destination (Join-Path $stageDirectory $tree[1])
}

$pixbufQuery = Get-ChildItem `
    -LiteralPath (Join-Path $vcpkgInstalled "tools\gdk-pixbuf") `
    -Filter "gdk-pixbuf-query-loaders.exe" `
    -File `
    -Recurse `
    -ErrorAction SilentlyContinue |
    Select-Object -First 1
if ($pixbufQuery) {
    Copy-Item `
        -LiteralPath $pixbufQuery.FullName `
        -Destination (Join-Path $stageDirectory "gdk-pixbuf-query-loaders.exe") `
        -Force
}

$compiledSchemas = Join-Path $stageDirectory "share\glib-2.0\schemas\gschemas.compiled"
if (-not (Test-Path -LiteralPath $compiledSchemas -PathType Leaf)) {
    throw "The staged GTK runtime has no compiled GSettings schemas. Run build.bat again."
}

Write-Host "Staged Ruzu and $($runtimeDlls.Count) vcpkg DLLs in:"
Write-Host "  $stageDirectory"
if ($StageOnly) {
    return
}

New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
Push-Location $DistDirectory
try {
    & $makeNsis `
        "/DPRODUCT_VERSION=$Version" `
        "/DARCH=$Architecture" `
        "/DVARIANT=$Variant" `
        "/DBINARY_SOURCE_DIR=$stageDirectory" `
        "/DOUTPUT_DIR=$outputDirectory" `
        $InstallerScript
    if ($LASTEXITCODE -ne 0) {
        throw "NSIS failed to build the Ruzu installer."
    }
}
finally {
    Pop-Location
}

$installer = Join-Path $outputDirectory "Ruzu-Windows-$Version-$Architecture-$Variant-installer.exe"
if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) {
    throw "NSIS completed without producing the expected installer: $installer"
}
Write-Host "Created Windows installer:"
Write-Host "  $installer"
