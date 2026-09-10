#Requires -Version 5.1
# Git-derived, filename-safe identity. Keep in sync with package-revision.sh.
[CmdletBinding()]
param([string]$Repository = (Split-Path -Parent $PSScriptRoot))
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Read-Git([string[]]$Arguments) {
    $result = @(& git -C $Repository @Arguments)
    if ($LASTEXITCODE -ne 0) { throw "Unable to resolve package Git identity: $Arguments" }
    return $result
}

$commit = Read-Git @('rev-parse', '--verify', 'HEAD')
$hash = ([string]$commit).Substring(0, 12)
$branch = [string](Read-Git @('rev-parse', '--abbrev-ref', 'HEAD'))
if ($branch -eq 'HEAD') { $branch = 'detached' }
$branch = [regex]::Replace($branch, '[^A-Za-z0-9._-]+', '-').Trim('.-')
if (-not $branch) { $branch = 'branch' }
$dirty = @(Read-Git @('status', '--porcelain', '--untracked-files=all', '--ignore-submodules=none')).Count -ne 0
$submodules = @(Read-Git @('submodule', 'status', '--recursive'))
if ($submodules | Where-Object { $_ -match '^[-+U]' }) { $dirty = $true }
$tags = @(Read-Git @('tag', '--points-at', 'HEAD', '--sort=refname'))
$tag = $tags | Where-Object { $_ -match '^v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9][A-Za-z0-9.-]*)?$' } | Select-Object -First 1
if ($tag -and -not $dirty) { return $tag }
$suffix = if ($dirty) { '-dirty' } else { '' }
"$branch-$hash$suffix"
