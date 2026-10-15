[CmdletBinding()]
param([string]$Target, [string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
if ($PSVersionTable.PSVersion.Major -lt 6) {
    # A caller running pwsh may pass incompatible PS7 module paths to Desktop.
    $env:PSModulePath = (($env:PSModulePath -split ';' | Where-Object { $_ -notmatch '\\PowerShell\\(?:7(?:\\|$)|Modules(?:\\|$))' }) -join ';')
}
Set-StrictMode -Version Latest
$root = Split-Path $PSScriptRoot -Parent
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $root 'bin' }
$temporary = $null
Push-Location $root
try {
    & (Join-Path $PSScriptRoot 'check-toolchain.ps1') | Out-Host
    & (Join-Path $root 'vendor/btls-sys/verify.ps1') | Out-Host
    $arguments = @('build', '--release', '--locked', '--message-format=json-render-diagnostics')
    if ($Target) { $arguments += @('--target', $Target) }
    # Only Cargo's events from this successful invocation may identify the exe.
    # This also works with CARGO_TARGET_DIR and .cargo/config target settings.
    $events = @(& cargo @arguments)
    if ($LASTEXITCODE -ne 0) { throw "Cargo build failed (exit $LASTEXITCODE)." }
    $artifacts = @($events | ForEach-Object {
        $event = $_ | ConvertFrom-Json
        if ($event.reason -eq 'compiler-artifact' -and $event.target.name -eq 'proxpilot' -and
            $event.target.kind -contains 'bin' -and $event.executable) { $event.executable }
    } | Select-Object -Unique)
    if ($artifacts.Count -ne 1 -or -not (Test-Path -LiteralPath $artifacts[0] -PathType Leaf)) {
        throw 'Cargo did not report exactly one existing proxpilot binary; no fallback to old artifacts.'
    }
    $source = Get-Item -LiteralPath $artifacts[0]
    if ($source.Extension -ne '.exe') { throw 'Expected a Windows executable.' }
    $output = [IO.Path]::GetFullPath($OutputDirectory)
    New-Item -ItemType Directory -Force -Path $output | Out-Null
    $temporary = Join-Path $output ('.proxpilot-' + [guid]::NewGuid().ToString('N') + '.tmp')
    Copy-Item -LiteralPath $source.FullName -Destination $temporary
    $hash = (Get-FileHash -LiteralPath $source.FullName -Algorithm SHA256).Hash
    if ((Get-FileHash -LiteralPath $temporary -Algorithm SHA256).Hash -ne $hash) { throw 'Staged exe hash mismatch.' }
    $destination = Join-Path $output 'proxpilot.exe'
    Move-Item -LiteralPath $temporary -Destination $destination -Force
    $temporary = $null
    if ((Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash -ne $hash) { throw 'Copied exe hash mismatch.' }
    Write-Host "Output: $destination"
    Write-Host "SHA256: $hash"
} catch {
    [Console]::Error.WriteLine("Build failed: $($_.Exception.Message)")
    exit 1
} finally {
    if ($temporary -and (Test-Path -LiteralPath $temporary)) { Remove-Item -LiteralPath $temporary -Force }
    Pop-Location
}
