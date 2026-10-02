# Build/run the Slint UI on native Windows (MSVC).
# Daily profile release-dev: opt-level 3 without fat LTO (debug pegs a core on PTY flood).
# Do not set RUSTFLAGS here; link search lives in crates/noviewlog-slint/build.rs.
# Do not use the Linux run-slint.sh / .deps fontconfig helpers on Windows.
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Args
)

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

    # vcvars64.bat prints `set` output when invoked via `cmd /c "... && set"`.
    $lines = cmd /c "`"$VcVarsPath`" >nul && set"
    foreach ($line in $lines) {
        $eq = $line.IndexOf('=')
        if ($eq -le 0) { continue }
        $name = $line.Substring(0, $eq)
        $value = $line.Substring($eq + 1)
        Set-Item -Path "Env:$name" -Value $value
    }
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

$cargoArgs = @('run', '--profile', 'release-dev', '-p', 'noviewlog-slint')
if ($Args.Count -gt 0) {
    $cargoArgs += '--'
    $cargoArgs += $Args
}

Write-Host "==> cargo $($cargoArgs -join ' ')"
& cargo @cargoArgs
exit $LASTEXITCODE
