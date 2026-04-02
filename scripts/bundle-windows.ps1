#!/usr/bin/env pwsh
# bundle-windows.ps1 — Build TunnelDesk and package it as a Windows ZIP.
#
# Usage:
#   ./scripts/bundle-windows.ps1 [-Version X.Y.Z]
#
# Options:
#   -Version X.Y.Z    Version string used in the output filename (default: 0.1.0)
#
# Prerequisites (on the build machine):
#   - Rust toolchain with x86_64-pc-windows-msvc target:
#       rustup target add x86_64-pc-windows-msvc
#   - Node.js 24

param(
    [string]$Version = "0.1.0"
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Root = Split-Path -Parent $ScriptDir

$AppName   = "TunnelDesk"
$BinName   = "tunneldesk"
$Target    = "x86_64-pc-windows-msvc"
$ZipName   = "$AppName-$Version.zip"

Write-Host "==> Building frontend"
Push-Location "$Root\frontend"
npm ci
npm run build
Pop-Location

Write-Host "==> Building Rust binary ($Target)"
rustup target add $Target
cargo build --release --target $Target

$BinPath = "$Root\target\$Target\release\$BinName.exe"
if (-not (Test-Path $BinPath)) {
    Write-Error "Binary not found: $BinPath"
    exit 1
}

Write-Host "==> Assembling $AppName-$Version"
$StageDir = "$Root\$AppName-$Version"
if (Test-Path $StageDir) { Remove-Item -Recurse -Force $StageDir }
New-Item -ItemType Directory -Path $StageDir | Out-Null

Copy-Item $BinPath "$StageDir\$BinName.exe"

if (Test-Path "$Root\config.toml.example") {
    Copy-Item "$Root\config.toml.example" "$StageDir\config.toml.example"
}

Write-Host "==> Creating $ZipName"
$ZipPath = "$Root\$ZipName"
if (Test-Path $ZipPath) { Remove-Item $ZipPath }
Compress-Archive -Path "$StageDir\*" -DestinationPath $ZipPath
Remove-Item -Recurse -Force $StageDir

Write-Host ""
Write-Host "Done: $ZipPath"
Write-Host ""
Write-Host "Requirements:"
Write-Host "  - WebView2 runtime (pre-installed on Windows 11; Windows 10 users may need"
Write-Host "    to install it from https://developer.microsoft.com/microsoft-edge/webview2/)"
Write-Host "  - cloudflared must be installed separately"
Write-Host "    (https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/)"
