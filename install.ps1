<#
.SYNOPSIS
    Install deepseek-web-mcp from GitHub releases on Windows.

.DESCRIPTION
    Downloads the x86_64 Windows archive of the requested release, verifies its
    sha256 checksum and unpacks deepseek-web-mcp.exe into -InstallDir.
    PATH is never modified unless -AddToPath is given.

.EXAMPLE
    .\install.ps1

.EXAMPLE
    .\install.ps1 -Version 0.3.0 -AddToPath

.EXAMPLE
    irm https://raw.githubusercontent.com/zhuzhda/deepseek-web-mcp/main/install.ps1 | iex
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'deepseek-web-mcp'),
    [switch]$AddToPath
)

$ErrorActionPreference = 'Stop'
$repo = 'zhuzhda/deepseek-web-mcp'
$bin = 'deepseek-web-mcp'

function Get-Target {
    switch ($env:PROCESSOR_ARCHITECTURE) {
        'AMD64' { return 'x86_64-pc-windows-msvc' }
        'ARM64' { throw "no prebuilt binary for Windows on ARM; use 'cargo binstall $bin' or 'cargo install $bin'" }
        default { throw "unsupported architecture '$env:PROCESSOR_ARCHITECTURE'; use 'cargo binstall $bin'" }
    }
}

$target = Get-Target

if (-not $Version) {
    $latest = Invoke-RestMethod -UseBasicParsing "https://api.github.com/repos/$repo/releases/latest"
    $Version = $latest.tag_name
}
$Version = $Version.TrimStart('v')

$archiveName = "$bin-$target.zip"
$base = "https://github.com/$repo/releases/download/v$Version"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("$bin-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
    $archive = Join-Path $tmp $archiveName
    Write-Host "Downloading $archiveName (v$Version)..."
    Invoke-WebRequest -UseBasicParsing "$base/$archiveName" -OutFile $archive

    # Read the checksum from a file: PowerShell 5.1 hands back a byte array
    # from Invoke-WebRequest when the response is small and untyped.
    $checksumFile = Join-Path $tmp "$bin-$target.sha256"
    Invoke-WebRequest -UseBasicParsing "$base/$bin-$target.sha256" -OutFile $checksumFile
    $expected = ((Get-Content -Path $checksumFile -Raw) -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Path $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if (-not $expected) { throw "empty checksum file for $archiveName" }
    if ($expected -ne $actual) {
        throw "checksum mismatch for $archiveName (expected $expected, got $actual)"
    }

    Expand-Archive -Path $archive -DestinationPath $tmp -Force
    $exe = Join-Path $tmp "$bin.exe"
    if (-not (Test-Path $exe)) { throw "archive $archiveName did not contain $bin.exe" }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $installed = Join-Path $InstallDir "$bin.exe"
    Copy-Item $exe $installed -Force

    Write-Host "Installed $(& $installed --version) to $installed"

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $inPath = ($userPath -split ';') -contains $InstallDir
    if ($inPath) {
        return
    }
    if ($AddToPath) {
        [Environment]::SetEnvironmentVariable('Path', "$userPath;$InstallDir", 'User')
        Write-Host "Added $InstallDir to the user PATH; restart your terminal."
    }
    else {
        Write-Host ''
        Write-Host "$InstallDir is not in your PATH. Re-run with -AddToPath, or add it manually:"
        Write-Host "  [Environment]::SetEnvironmentVariable('Path', `"`$env:Path;$InstallDir`", 'User')"
        Write-Host ''
        Write-Host 'Note: an unsigned .exe can trigger SmartScreen/antivirus warnings.'
    }
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
