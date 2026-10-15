$ErrorActionPreference = 'Stop'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) { throw 'Install Visual Studio 2026 C++ Build Tools and a Windows SDK.' }
$installation = (& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath) -join ''
if ($LASTEXITCODE -ne 0 -or -not $installation) { throw 'No MSVC x64 C++ installation found.' }
$tools = @(Get-ChildItem -LiteralPath (Join-Path $installation 'VC/Tools/MSVC') -Directory |
    Where-Object { $_.Name -match '^\d+\.\d+\.\d+' } | Sort-Object { [version]$_.Name } -Descending)
if (-not $tools -or [version]$tools[0].Name -lt [version]'14.51.36231') {
    throw 'Bundled native libraries require MSVC toolset 14.51.36231 or a compatible newer toolset.'
}
$bin = Join-Path $tools[0].FullName 'bin/Hostx64/x64'
foreach ($name in @('cl.exe', 'link.exe')) {
    $file = Get-Item -LiteralPath (Join-Path $bin $name)
    Write-Host "$name : $($file.VersionInfo.FileVersion) ($($file.FullName))"
}
$kits = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots'
$sdk = @(Get-ChildItem -LiteralPath (Join-Path $kits.KitsRoot10 'Lib') -Directory |
    Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'um/x64/kernel32.lib') } |
    Sort-Object { [version]$_.Name } -Descending)
if (-not $sdk) { throw 'A Windows SDK with x64 libraries is required.' }
Write-Host "Windows SDK: $($sdk[0].Name) ($($sdk[0].FullName))"
& rustc --version
if ($LASTEXITCODE -ne 0) { throw 'Rust compiler unavailable.' }
& cargo --version
if ($LASTEXITCODE -ne 0) { throw 'Cargo unavailable.' }
