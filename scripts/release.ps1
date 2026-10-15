[CmdletBinding()]
param([switch]$PublishDraft, [string]$Remote = 'origin', [string]$Target = 'x86_64-pc-windows-msvc')
$ErrorActionPreference = 'Stop'
if ($PSVersionTable.PSVersion.Major -lt 6) {
    $env:PSModulePath = (($env:PSModulePath -split ';' | Where-Object { $_ -notmatch '\\PowerShell\\(?:7(?:\\|$)|Modules(?:\\|$))' }) -join ';')
}
Set-StrictMode -Version Latest
$root = Split-Path $PSScriptRoot -Parent
$taskConsoleEncoding = [Console]::OutputEncoding
$taskPipeEncoding = $OutputEncoding
$utf8 = New-Object Text.UTF8Encoding($false)
function Write-Utf8([string]$Path, [string]$Content) { [IO.File]::WriteAllText($Path, $Content, $utf8) }
function Invoke-Checked([string]$Command, [string[]]$Arguments) {
    $result = @(& $Command @Arguments)
    if ($LASTEXITCODE -ne 0) { throw "$Command $($Arguments -join ' ') failed (exit $LASTEXITCODE)." }
    return $result
}
Push-Location $root
try {
    [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
    $OutputEncoding = [Console]::OutputEncoding
    $cargoToml = Get-Content -LiteralPath 'Cargo.toml' -Raw
    $match = [regex]::Match($cargoToml, '(?m)^version\s*=\s*"([^"\r\n]+)"')
    if (-not $match.Success) { throw 'Missing package version.' }
    $version = $match.Groups[1].Value
    $tag = "v$version"
    if (-not (Select-String -LiteralPath 'CHANGELOG.md' -Encoding UTF8 -Pattern "^## v$([regex]::Escape($version))(?=$|[^0-9A-Za-z.+-])" -Quiet)) {
        throw "CHANGELOG.md must contain a heading for $tag."
    }
    $status = @(Invoke-Checked git @('status', '--porcelain'))
    $dirty = ($status -join '').Trim().Length -gt 0
    if ($PublishDraft) {
        if ($dirty) { throw 'Commit reviewed changes first; publishing requires a clean tree.' }
        & git rev-parse --verify "refs/tags/$tag" *> $null
        if ($LASTEXITCODE -eq 0) { throw "Tag $tag already exists." }
        $remoteUrl = (Invoke-Checked git @('remote', 'get-url', $Remote)) -join ''
        if ($remoteUrl -notmatch '^(https://github\.com/|git@github\.com:)[^/]+/[^/]+?(?:\.git)?$') {
            throw 'Draft publishing requires an explicit github.com remote.'
        }
        Invoke-Checked gh @('auth', 'status') | Out-Host
    }
    $revision = ((Invoke-Checked git @('rev-parse', 'HEAD')) -join '').Trim()
    $rust = ((Invoke-Checked rustc @('--version')) -join '').Trim()
    & (Join-Path $root 'vendor/btls-sys/verify.ps1') | Out-Host
    Invoke-Checked cargo @('test', '--locked', '--quiet', '--target', $Target) | Out-Host
    $package = Join-Path $root ("dist/proxpilot-$tag-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $package | Out-Null
    # A separate PowerShell process makes the build script's exit code unambiguous.
    $shell = (Get-Process -Id $PID).Path
    Invoke-Checked $shell @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $PSScriptRoot 'build.ps1'), '-Target', $Target, '-OutputDirectory', $package) | Out-Host
    $licenses = Join-Path $package 'licenses'
    New-Item -ItemType Directory -Path $licenses | Out-Null
    Copy-Item -LiteralPath (Join-Path $root 'vendor/btls-sys/LICENSE-MIT') -Destination $licenses
    Copy-Item -LiteralPath (Join-Path $root 'vendor/btls-sys/prebuilt/x86_64-pc-windows-msvc/LICENSE-BORINGSSL') -Destination $licenses
    $metadata = (Invoke-Checked cargo @('metadata', '--locked', '--format-version', '1')) -join "`n" | ConvertFrom-Json
    $dependencyLicenses = @($metadata.packages | Select-Object name,version,license)
    Write-Utf8 (Join-Path $licenses 'dependency-licenses.json') (ConvertTo-Json -InputObject $dependencyLicenses -Depth 5)
    Copy-Item -LiteralPath (Join-Path $root 'vendor/btls-sys/prebuilt/x86_64-pc-windows-msvc/manifest.json') -Destination (Join-Path $package 'native-manifest.json')
    $provenance = [ordered]@{schema_version=1; version=$version; revision=$revision; dirty=$dirty; rust=$rust; target=$Target; tests='cargo test --locked --quiet'; native_integrity_verified=$true; exe_sha256=(Get-FileHash (Join-Path $package 'proxpilot.exe') -Algorithm SHA256).Hash.ToLowerInvariant()}
    Write-Utf8 (Join-Path $package 'build-metadata.json') ($provenance | ConvertTo-Json -Depth 5)
    $lines = @(Get-ChildItem -LiteralPath $package -File -Recurse | Sort-Object FullName | ForEach-Object {
        $relative = $_.FullName.Substring($package.Length + 1).Replace('\', '/')
        (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() + '  ' + $relative
    })
    Write-Utf8 (Join-Path $package 'SHA256SUMS.txt') (($lines -join "`n") + "`n")
    $archive = "$package.zip"
    Compress-Archive -Path (Join-Path $package '*') -DestinationPath $archive
    Write-Utf8 "$archive.sha256" ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() + '  ' + [IO.Path]::GetFileName($archive) + "`n")
    Write-Host "Package: $archive"
    if ($PublishDraft) {
        # Revalidate cleanliness and HEAD after the potentially long build.
        if (((Invoke-Checked git @('status', '--porcelain')) -join '').Trim() -or
            ((Invoke-Checked git @('rev-parse', 'HEAD')) -join '').Trim() -ne $revision) { throw 'Source changed during packaging.' }
        $repository = $remoteUrl -replace '^https://github\.com/|^git@github\.com:', '' -replace '\.git$', ''
        Invoke-Checked git @('tag', $tag, $revision) | Out-Host
        Invoke-Checked git @('push', $Remote, "refs/tags/$tag") | Out-Host
        Invoke-Checked gh @('release', 'create', $tag, $archive, "$archive.sha256", '--repo', $repository, '--draft', '--verify-tag', '--title', "ProxPilot $tag", '--notes-file', (Join-Path $root 'CHANGELOG.md')) | Out-Host
        Write-Host 'Draft release created. Review it before publishing.'
    } else { Write-Host 'Local package only. Use -PublishDraft explicitly to create a draft release.' }
} catch {
    [Console]::Error.WriteLine("Packaging failed: $($_.Exception.Message)")
    exit 1
} finally {
    [Console]::OutputEncoding = $taskConsoleEncoding
    $OutputEncoding = $taskPipeEncoding
    Pop-Location
}
