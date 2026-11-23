<#
.SYNOPSIS
Builds Spectra's Microsoft Store package (.msix) for one architecture, or
bundles several .msix files into one .msixbundle for upload.

.EXAMPLE
# Package for this PC's architecture, shipping ffmpeg inside:
packaging\windows\build-msix.ps1 -FFmpeg C:\tools\ffmpeg\bin\ffmpeg.exe

.EXAMPLE
# Bundle the x64 and ARM64 packages for Partner Center:
packaging\windows\build-msix.ps1 -Bundle target\msix\Spectra_0.1.0.0_x64.msix, target\msix\Spectra_0.1.0.0_arm64.msix
#>
param(
    # x64 or arm64; defaults to this PC's architecture.
    [ValidateSet('x64', 'arm64')]
    [string] $Arch = $(if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }),
    # Partner Center → Product identity. The defaults only suit local tests.
    [string] $IdentityName = $(if ($env:MSIX_IDENTITY_NAME) { $env:MSIX_IDENTITY_NAME } else { 'Spectra' }),
    [string] $Publisher = $(if ($env:MSIX_PUBLISHER) { $env:MSIX_PUBLISHER } else { 'CN=Spectra' }),
    [string] $PublisherDisplayName = $(if ($env:MSIX_PUBLISHER_DISPLAY_NAME) { $env:MSIX_PUBLISHER_DISPLAY_NAME } else { 'f+e' }),
    # An ffmpeg.exe to ship in the package (recording, capture cards and cameras need it).
    [string] $FFmpeg,
    # Use the executable already built instead of running cargo.
    [switch] $SkipBuild,
    # .msix files to bundle, instead of building a package.
    [string[]] $Bundle,
    [string] $OutDir = 'target\msix'
)

$ErrorActionPreference = 'Stop'
Set-Location (Resolve-Path "$PSScriptRoot\..\..")

function Find-SdkTool([string] $Name) {
    $hostArch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
    foreach ($dir in $hostArch, 'x64') {
        $tool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\$dir\$Name" -ErrorAction SilentlyContinue |
            Sort-Object { [version] $_.Directory.Parent.Name } | Select-Object -Last 1
        if ($tool) { return $tool.FullName }
    }
    throw "$Name wasn't found: install the Windows SDK."
}

function Reset-Directory([string] $Path) {
    if (Test-Path $Path) { Remove-Item -Recurse -Force $Path }
    New-Item -ItemType Directory -Force $Path | Out-Null
}

$makeappx = Find-SdkTool 'makeappx.exe'
New-Item -ItemType Directory -Force $OutDir | Out-Null

# The package version is Cargo.toml's plus a fourth field, which the Store requires to be 0.
$match = (Select-String -Path Cargo.toml -Pattern '^version = "(\d+)\.(\d+)\.(\d+)').Matches[0]
$version = '{0}.{1}.{2}.0' -f $match.Groups[1].Value, $match.Groups[2].Value, $match.Groups[3].Value

if ($Bundle) {
    $dir = Join-Path $OutDir 'bundle'
    Reset-Directory $dir
    $Bundle | ForEach-Object { Copy-Item $_ $dir }
    $output = Join-Path $OutDir "Spectra_$version.msixbundle"
    & $makeappx bundle /o /d $dir /p $output /bv $version
    if ($LASTEXITCODE) { throw 'makeappx bundle failed.' }
    Write-Host "Wrote $output"
    exit
}

# Build for the package's architecture; cargo's own target folder when it's this PC's.
$triple = @{ x64 = 'x86_64-pc-windows-msvc'; arm64 = 'aarch64-pc-windows-msvc' }[$Arch]
$hostArch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
$exe = if ($Arch -eq $hostArch) { 'target\release\spectra.exe' } else { "target\$triple\release\spectra.exe" }
if (-not $SkipBuild) {
    if ($Arch -eq $hostArch) { cargo build --release --locked } else { cargo build --release --locked --target $triple }
    if ($LASTEXITCODE) { throw 'cargo build failed.' }
}
if (-not (Test-Path $exe)) { throw "$exe doesn't exist: build it first, or drop -SkipBuild." }

$layout = Join-Path $OutDir "layout-$Arch"
Reset-Directory $layout
Copy-Item $exe $layout
Copy-Item packaging\windows\Assets (Join-Path $layout 'Assets') -Recurse
if ($FFmpeg) {
    Copy-Item $FFmpeg (Join-Path $layout 'ffmpeg.exe')
} else {
    Write-Warning 'No -FFmpeg: the package relies on an ffmpeg on PATH for recording, capture cards and cameras.'
}

$escape = { param($value) [System.Security.SecurityElement]::Escape($value) }
$manifest = (Get-Content packaging\windows\AppxManifest.xml -Raw).
    Replace('{{IDENTITY_NAME}}', (& $escape $IdentityName)).
    Replace('{{PUBLISHER}}', (& $escape $Publisher)).
    Replace('{{PUBLISHER_DISPLAY_NAME}}', (& $escape $PublisherDisplayName)).
    Replace('{{VERSION}}', $version).
    Replace('{{ARCH}}', $Arch)
[System.IO.File]::WriteAllText((Join-Path (Resolve-Path $layout) 'AppxManifest.xml'), $manifest, (New-Object System.Text.UTF8Encoding $false))

$output = Join-Path $OutDir "Spectra_${version}_$Arch.msix"
& $makeappx pack /o /d $layout /p $output
if ($LASTEXITCODE) { throw 'makeappx pack failed.' }
Write-Host "Wrote $output"
