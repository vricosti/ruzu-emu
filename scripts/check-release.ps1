#Requires -Version 5.1
# Windows counterpart of check-release.sh; usable independently for validation.
[CmdletBinding()]
param(
    [string]$Repository = (Split-Path -Parent $PSScriptRoot),
    [string]$Version,
    [switch]$ForcePackage
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-CleanCheckout {
    $status = @(& git -C $Repository status --porcelain --untracked-files=all --ignore-submodules=none)
    if ($LASTEXITCODE -ne 0) { throw "Unable to inspect the release checkout." }
    if ($status.Count -ne 0) {
        throw "Release packaging requires a clean checkout (including untracked files and submodules). Commit or stash your changes first."
    }
    $submodules = @(& git -C $Repository submodule status --recursive)
    if ($LASTEXITCODE -ne 0) { throw "Unable to inspect release submodules." }
    if ($submodules | Where-Object { $_ -match '^[-+U]' }) {
        throw "Release submodules must be initialized and match the recorded commits."
    }
}

if (-not $ForcePackage) { Assert-CleanCheckout }
Push-Location $Repository
try {
    $packageId = & cargo pkgid --offline -p ruzu
    if ($LASTEXITCODE -ne 0) { throw "Unable to resolve the Ruzu Cargo version." }
}
finally { Pop-Location }
$cargoVersion = ($packageId -split '#')[-1] -replace '^.*@', ''
if ($cargoVersion -notmatch '^\d+\.\d+\.\d+$') {
    throw "Release packaging requires a numeric major.minor.patch Cargo version: $cargoVersion"
}
if ($Version -and $Version -ne $cargoVersion) {
    throw "Requested version $Version does not match Cargo version $cargoVersion. Update Cargo.toml instead."
}
if ($ForcePackage) {
    Write-Warning "Release tag and clean-checkout checks were explicitly disabled with -ForcePackage. Cargo version remains authoritative."
}
else {
    # Missing tags are a validation failure, not a PowerShell native stderr error.
    $tag = & git -C $Repository describe --tags --exact-match HEAD 2>$null
    if ($LASTEXITCODE -ne 0) {
        throw "Release packaging requires HEAD to have the exact tag v$cargoVersion."
    }
    if ($tag -ne "v$cargoVersion") {
        throw "Release tag $tag does not match Cargo version $cargoVersion (expected v$cargoVersion). Update Cargo.toml and the release tag before packaging."
    }
    Assert-CleanCheckout
}
$cargoVersion
