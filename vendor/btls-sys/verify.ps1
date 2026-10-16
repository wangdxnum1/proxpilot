$ErrorActionPreference = 'Stop'
$packagePath = Join-Path $PSScriptRoot 'prebuilt/x86_64-pc-windows-msvc'
$manifest = Get-Content -LiteralPath (Join-Path $packagePath 'manifest.json') -Raw | ConvertFrom-Json
foreach ($artifact in $manifest.files.PSObject.Properties) {
    $path = Join-Path $packagePath $artifact.Name
    $file = Get-Item -LiteralPath $path
    if ($file.Length -ne $artifact.Value.size) { throw "Size mismatch: $($artifact.Name)" }
    $digest = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    if ($digest -ine $artifact.Value.sha256) { throw "SHA256 mismatch: $($artifact.Name)" }
}
Write-Output 'Verified: all bundled BoringSSL artifacts match the manifest.'
