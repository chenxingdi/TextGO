#
# Build the TextGO installer locally (PowerShell version).
# See scripts/build-local.sh for the full explanation.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts/build-local.ps1
#   pnpm build:local
#
$ErrorActionPreference = 'Stop'

# Rust toolchain locations (kept on E: to avoid filling up C:)
if (-not $env:RUSTUP_HOME) { $env:RUSTUP_HOME = 'E:/rustup' }
if (-not $env:CARGO_HOME) { $env:CARGO_HOME = 'E:/cargo' }
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = 'E:/textgo-target' }
$env:PATH = "$($env:CARGO_HOME)/bin;$env:PATH"

# Updater signing key and password (never hard-coded here)
if (-not $env:TAURI_SIGNING_PRIVATE_KEY) {
  $keyFile = Join-Path $env:USERPROFILE '.tauri\textgo.key'
  if (-not (Test-Path $keyFile)) {
    throw "Cannot find the updater private key: $keyFile (set TAURI_SIGNING_PRIVATE_KEY to override)"
  }
  $env:TAURI_SIGNING_PRIVATE_KEY = $keyFile
}

if (-not $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) {
  $passwordFile = Join-Path $env:USERPROFILE '.tauri\textgo.key.password.txt'
  if (-not (Test-Path $passwordFile)) {
    throw "Cannot find the updater key password file: $passwordFile (set TAURI_SIGNING_PRIVATE_KEY_PASSWORD to override)"
  }
  $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = (Get-Content -Raw $passwordFile).Trim()
}

Set-Location (Join-Path $PSScriptRoot '..')

Write-Host "RUSTUP_HOME          = $env:RUSTUP_HOME"
Write-Host "CARGO_HOME           = $env:CARGO_HOME"
Write-Host "CARGO_TARGET_DIR     = $env:CARGO_TARGET_DIR"
Write-Host "updater private key  = $env:TAURI_SIGNING_PRIVATE_KEY"
Write-Host "updater password     = loaded ($($env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD.Length) chars)"
Write-Host ""

& pnpm tauri build @args
exit $LASTEXITCODE
