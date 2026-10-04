#Requires -Version 5.1
<#
.SYNOPSIS
Downloads Microsoft ProcDump and captures two full dumps of a running Ruzu.
.DESCRIPTION
Run while Ruzu shows the black screen. No compilation or debugger installation
is required. Dumps stay local and may contain sensitive data, including keys
and guest memory. Never upload them publicly. ProcDump may briefly pause Ruzu.
.EXAMPLE
powershell -NoProfile -ExecutionPolicy Bypass -File .\capture-windows-hang.ps1
.EXAMPLE
.\capture-windows-hang.ps1 -ProcessId 1234
.EXAMPLE
.\capture-windows-hang.ps1 -PrepareOnly
#>
[CmdletBinding()]
param(
    [ValidateRange(1, 2147483647)]
    [int]$ProcessId,

    [ValidateRange(1, 60)]
    [int]$DelaySeconds = 10,

    [string]$OutputDirectory,

    # Download and verify ProcDump without inspecting or dumping any process.
    [switch]$PrepareOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

try {
    if ($env:OS -ne 'Windows_NT') {
        throw 'This script requires Windows.'
    }

    if (-not $OutputDirectory) {
        $desktopDirectory = [Environment]::GetFolderPath('Desktop');
        if (-not $desktopDirectory) { $desktopDirectory = [IO.Path]::GetTempPath() }
        $OutputDirectory = Join-Path $desktopDirectory 'Ruzu-Diagnostics'
    }
    $sessionName = 'ruzu-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + [Guid]::NewGuid().ToString('N').Substring(0, 8)
    $sessionDirectory = Join-Path ([IO.Path]::GetFullPath($OutputDirectory)) $sessionName
    New-Item -ItemType Directory -Path $sessionDirectory -ErrorAction Stop | Out-Null
    $toolsDirectory = Join-Path $sessionDirectory 'tools'
    New-Item -ItemType Directory -Path $toolsDirectory | Out-Null

    Write-Host "Output folder: $sessionDirectory"
    Write-Host 'Downloading ProcDump from Microsoft...'
    $archivePath = Join-Path $toolsDirectory 'Procdump.zip'
    $previousTls = [Net.ServicePointManager]::SecurityProtocol
    try {
        [Net.ServicePointManager]::SecurityProtocol = $previousTls -bor [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -Uri 'https://download.sysinternals.com/files/Procdump.zip' -OutFile $archivePath -UseBasicParsing
    }
    finally {
        [Net.ServicePointManager]::SecurityProtocol = $previousTls
    }
    Expand-Archive -LiteralPath $archivePath -DestinationPath $toolsDirectory

    $nativeArchitecture = $env:PROCESSOR_ARCHITEW6432
    if (-not $nativeArchitecture) { $nativeArchitecture = $env:PROCESSOR_ARCHITECTURE }
    $toolName = switch ($nativeArchitecture) {
        'ARM64' { 'procdump64a.exe' }
        'AMD64' { 'procdump64.exe' }
        'x86'   { 'procdump.exe' }
        default { throw "Unsupported Windows architecture: $nativeArchitecture" }
    }
    $toolPath = Join-Path $toolsDirectory $toolName
    $signature = Get-AuthenticodeSignature -LiteralPath $toolPath
    if ($signature.Status -ne 'Valid' -or
        $null -eq $signature.SignerCertificate -or
        $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation(?:,|$)') {
        throw 'The Microsoft signature on ProcDump could not be verified. No tool will be executed.'
    }
    Write-Host 'Microsoft signature verified.'
    if ($PrepareOnly) {
        Write-Host 'Preparation complete. No process was captured.'
        return
    }

    $candidates = @(Get-Process -ErrorAction Stop | Where-Object {
        $_.ProcessName -in @('ruzu', 'ruzu-cmd', 'ruzu-termination-fix')
    } | Sort-Object Id)
    if ($PSBoundParameters.ContainsKey('ProcessId')) {
        $selectedProcess = $candidates | Where-Object Id -EQ $ProcessId | Select-Object -First 1
        if ($null -eq $selectedProcess) { throw "PID $ProcessId does not belong to a running Ruzu instance." }
    }
    elseif ($candidates.Count -eq 1) {
        $selectedProcess = $candidates[0]
    }
    elseif ($candidates.Count -eq 0) {
        throw 'Ruzu is not running. Launch the game, reproduce the black screen, then run this script again.'
    }
    else {
        Write-Host 'Multiple Ruzu processes are running:'
        $candidates | Select-Object Id, ProcessName, MainWindowTitle | Format-Table -AutoSize | Out-Host
        $selection = Read-Host 'Enter the PID of the window showing the black screen'
        $selectedId = 0
        if (-not [int]::TryParse($selection, [ref]$selectedId)) { throw 'Invalid PID.' }
        $selectedProcess = $candidates | Where-Object Id -EQ $selectedId | Select-Object -First 1
        if ($null -eq $selectedProcess) { throw 'The PID is not in the Ruzu process list.' }
    }

    $selectedId = $selectedProcess.Id
    $selectedStartTime = $selectedProcess.StartTime
    Write-Host "Process: $($selectedProcess.ProcessName), PID $selectedId"
    Write-Host 'Keep Ruzu open on the black screen. Each capture may temporarily pause it.'
    Write-Host 'The dumps may require several GB of disk space. No files will be uploaded.'
    Write-Host 'By continuing, you accept the Microsoft Sysinternals license (Eula.txt in the tools folder).'
    $confirmation = Read-Host 'Capture both dumps now? [y/N]'
    if ($confirmation -notin @('o', 'oui', 'y', 'yes')) {
        Write-Host 'Capture cancelled. No dump was created.'
        return
    }

    for ($captureIndex = 1; $captureIndex -le 2; $captureIndex++) {
        # Avoid capturing an unrelated process if Windows reuses the PID.
        $currentProcess = Get-Process -Id $selectedId -ErrorAction Stop
        if ($currentProcess.StartTime -ne $selectedStartTime) { throw 'Ruzu has restarted. Run this script again.' }
        $dumpPath = Join-Path $sessionDirectory "ruzu-black-screen-$captureIndex.dmp"
        Write-Host "Capturing dump $captureIndex/2..."
        & $toolPath -accepteula -ma $selectedId $dumpPath
        $captureExitCode = $LASTEXITCODE
        if ($captureExitCode -ne 0 -or -not (Test-Path -LiteralPath $dumpPath) -or
            (Get-Item -LiteralPath $dumpPath).Length -eq 0) {
            throw "ProcDump failed (exit code $captureExitCode). Check free disk space and access permissions; if necessary, run PowerShell as administrator."
        }
        if ($captureIndex -eq 1) {
            Write-Host "Waiting $DelaySeconds seconds before the second capture..."
            Start-Sleep -Seconds $DelaySeconds
        }
    }
    Write-Host "Done: both dumps are in $sessionDirectory"
    Write-Host 'Compress the two .dmp files with 7-Zip or ZIP and share them privately only.'
    Write-Host 'Ruzu remains open. No configuration, key, or game files were modified.'
}
catch {
    Write-Error -ErrorAction Continue "Capture stopped: $($_.Exception.Message)"
    Write-Host 'Any files already created are retained in the output folder.'
    exit 1
}
