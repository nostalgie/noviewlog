# Native Windows MSVC publish for noviewlog-slint → dist/noviewlog-slint-win-x64/
# Preferred from PowerShell (bash is often not on PATH).
# Git Bash equivalent: scripts/publish-slint-windows.sh
# Do not set RUSTFLAGS here; link search lives in crates/noviewlog-slint/build.rs.
# Do not use the Linux run-slint.sh / .deps fontconfig helpers on Windows.
$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

function Test-MsvcLinkInPath {
    return [bool](Get-Command link.exe -ErrorAction SilentlyContinue)
}

function Find-VcVars64 {
    $candidates = @(
        "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat",
        "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat",
        "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\Professional\VC\Auxiliary\Build\vcvars64.bat",
        "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\Enterprise\VC\Auxiliary\Build\vcvars64.bat",
        "${env:ProgramFiles}\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat",
        "${env:ProgramFiles}\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
    )
    foreach ($path in $candidates) {
        if (Test-Path -LiteralPath $path) {
            return $path
        }
    }
    return $null
}

function Import-VcVars64 {
    param([string]$VcVarsPath)

    $lines = cmd /c "`"$VcVarsPath`" >nul && set"
    foreach ($line in $lines) {
        $eq = $line.IndexOf('=')
        if ($eq -le 0) { continue }
        $name = $line.Substring(0, $eq)
        $value = $line.Substring($eq + 1)
        Set-Item -Path "Env:$name" -Value $value
    }
}

if ($env:OS -ne 'Windows_NT') {
    Write-Error "publish-slint-windows.ps1 requires a native Windows host (MSVC Rust toolchain)."
}

if (-not (Test-MsvcLinkInPath)) {
    $vcvars = Find-VcVars64
    if (-not $vcvars) {
        Write-Error @"
MSVC link.exe not found in PATH and vcvars64.bat was not located.
Install Visual Studio Build Tools 2022 with the Desktop development with C++ workload,
or run from an x64 Native Tools Command Prompt.
"@
    }
    Write-Host "==> Importing MSVC environment from $vcvars"
    Import-VcVars64 -VcVarsPath $vcvars
    if (-not (Test-MsvcLinkInPath)) {
        Write-Error "MSVC environment import failed: link.exe still not in PATH."
    }
}

# Static CRT: the dist exe must run on a pristine Windows install with no
# VC Redist. Publish-only flag; never set this for release-dev (fingerprint churn).
$env:RUSTFLAGS = '-C target-feature=+crt-static'

Write-Host "==> Building noviewlog-slint (release, static CRT)..."
& cargo build --release -p noviewlog-slint
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$srcExe = Join-Path $Root "target\release\noviewlog-slint.exe"
if (-not (Test-Path -LiteralPath $srcExe)) {
    Write-Error "expected $srcExe not found after cargo build"
}

$outDir = Join-Path $Root "dist\noviewlog-slint-win-x64"
Write-Host "==> Staging $outDir ..."
if (Test-Path -LiteralPath $outDir) {
    Remove-Item -LiteralPath $outDir -Recurse -Force
}
New-Item -ItemType Directory -Path $outDir | Out-Null
Copy-Item -LiteralPath $srcExe -Destination (Join-Path $outDir "NoViewLog.exe")

@'
NoViewLog (Slint) — Windows x64

Requirements: Windows 10 or later, x64.
Self-contained: the CRT is statically linked — no VC Redist or other installs.

Run:
  NoViewLog.exe
  NoViewLog.exe -- app.log
  NoViewLog.exe -- npm run dev

Copy this entire folder if you move the app; the engine is linked into NoViewLog.exe
(no separate noviewlog_core.dll).
'@ | Set-Content -LiteralPath (Join-Path $outDir "README.txt") -Encoding ascii

$staged = Join-Path $outDir "NoViewLog.exe"
Write-Host "==> Headless smoke (process start) ..."
$proc = Start-Process -FilePath $staged -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 2
if ($proc.HasExited) {
    Write-Error "NoViewLog.exe exited immediately after launch (exit $($proc.ExitCode))"
}
Write-Host "  OK: NoViewLog.exe started (pid $($proc.Id))"
Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
Wait-Process -Id $proc.Id -ErrorAction SilentlyContinue

Write-Host ""
Write-Host "Slint Windows distribution ready."
Write-Host "  Output: $outDir"
Write-Host ""
Write-Host "Run:"
Write-Host "  NoViewLog.exe"
