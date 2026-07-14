# Claw Code installer (Windows PowerShell)
#
# Native-Windows counterpart of install.sh: verifies the Rust toolchain
# (rustc + cargo), builds the `claw` binary from the local rust/ workspace
# (no remote download — everything builds from this checkout), and runs a
# post-install verification step.
#
# Usage (from the repository root):
#   .\install.ps1                # debug build (fast, default)
#   .\install.ps1 -Release      # optimized release build
#   .\install.ps1 -NoVerify    # skip post-install verification
#   .\install.ps1 -Help        # print usage
#
# If script execution is blocked, run once:
#   Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass

[CmdletBinding()]
param(
    [switch]$Release,
    [switch]$NoVerify,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'

function Write-Step([int]$n, [int]$total, [string]$text) {
    Write-Host ""
    Write-Host "[$n/$total] " -ForegroundColor Blue -NoNewline
    Write-Host $text -ForegroundColor White
}
function Write-Info([string]$text) { Write-Host "  ->  $text" -ForegroundColor Cyan }
function Write-Ok([string]$text)   { Write-Host "  ok  $text" -ForegroundColor Green }
function Write-Warn2([string]$text) { Write-Host "  warn $text" -ForegroundColor Yellow }
function Write-Err([string]$text)  { Write-Host "  error $text" -ForegroundColor Red }

function Show-Usage {
    @"
Usage: .\install.ps1 [options]

Options:
  -Release      Build the optimized release profile (slower, smaller binary).
  -NoVerify     Skip the post-install verification step.
  -Help         Show this help text and exit.

Environment overrides:
  CLAW_BUILD_PROFILE   debug | release
  CLAW_SKIP_VERIFY     set to 1 to skip verification
"@ | Write-Host
}

function Show-Troubleshooting([string]$buildProfile) {
    @"

Troubleshooting
---------------

  1. Rust toolchain missing
     Install Rust from https://rustup.rs/ (run rustup-init.exe).
     Close and reopen the terminal afterwards so PATH updates apply.

  2. Linker errors (link.exe not found)
     Install "Visual Studio Build Tools" with the
     "Desktop development with C++" workload, or let rustup-init
     install them for you.

  3. Build fails partway through
     Try a clean build:
       cd rust; cargo clean; cargo build --workspace

  4. 'claw' not found after install
     The binary lives at:
       rust\target\$buildProfile\claw.exe
     Add that directory to PATH or invoke it with the full path.

  5. Script execution blocked
     Run once in this terminal:
       Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass

"@ | Write-Host
}

if ($Help) { Show-Usage; exit 0 }

# Banner
Write-Host @'
   ____  _                   ____          _
  / ___|| |  __ _ __      __ / ___|___   __| | ___
 | |    | | / _` |\ \ /\ / /| |   / _ \ / _` |/ _ \
 | |___ | || (_| | \ V  V / | |__| (_) | (_| |  __/
  \____||_| \__,_|  \_/\_/   \____\___/ \__,_|\___|
'@ -ForegroundColor White
Write-Host "Claw Code installer (Windows)" -ForegroundColor DarkGray

$total = 6

# Profile resolution: flag first, then env var, then debug.
$buildProfile = if ($Release) { 'release' }
           elseif ($env:CLAW_BUILD_PROFILE -in @('debug', 'release')) { $env:CLAW_BUILD_PROFILE }
           else { 'debug' }
$skipVerify = $NoVerify -or ($env:CLAW_SKIP_VERIFY -eq '1')

# --------------------------------------------------------------------------
Write-Step 1 $total "Detecting host environment"
$osVersion = [System.Environment]::OSVersion.VersionString
$arch = $env:PROCESSOR_ARCHITECTURE
Write-Info "os:      $osVersion"
Write-Info "arch:    $arch"
Write-Info "profile: $buildProfile"
Write-Ok "native Windows PowerShell detected"

# --------------------------------------------------------------------------
Write-Step 2 $total "Locating the Rust workspace"
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$rustDir = Join-Path $scriptDir 'rust'
if (-not (Test-Path (Join-Path $rustDir 'Cargo.toml'))) {
    Write-Err "Could not find rust\Cargo.toml next to install.ps1 (expected: $rustDir)"
    Show-Troubleshooting $buildProfile
    exit 1
}
Write-Ok "workspace at $rustDir"

# --------------------------------------------------------------------------
Write-Step 3 $total "Checking prerequisites"
$missing = $false
foreach ($tool in @('rustc', 'cargo')) {
    $cmd = Get-Command $tool -ErrorAction SilentlyContinue
    if ($cmd) {
        $version = & $tool --version 2>$null
        Write-Ok "$tool found: $version"
    } else {
        Write-Err "$tool not found in PATH"
        $missing = $true
    }
}
if (Get-Command git -ErrorAction SilentlyContinue) {
    Write-Ok ("git found: " + (git --version 2>$null))
} else {
    Write-Warn2 "git not found - some workflows (login, session export) may degrade"
}
if ($missing) {
    Write-Err "Missing required tools. See troubleshooting below."
    Show-Troubleshooting $buildProfile
    exit 1
}

# --------------------------------------------------------------------------
Write-Step 4 $total "Building the claw workspace ($buildProfile)"
$freeGB = try { [math]::Round((Get-PSDrive -Name (Get-Location).Drive.Name).Free / 1GB, 1) } catch { $null }
if ($freeGB -ne $null -and $freeGB -lt 3) {
    Write-Warning "less than 3GB free on this drive (${freeGB}GB) — the build may fail; free space or run 'cargo clean' in rust\ first"
}
$cargoArgs = @('build', '--workspace')
if ($buildProfile -eq 'release') { $cargoArgs += '--release' }
Write-Info "running: cargo $($cargoArgs -join ' ')"
Write-Info "this may take a few minutes on the first build"
Push-Location $rustDir
try {
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        Write-Err "cargo build failed (exit $LASTEXITCODE)"
        Show-Troubleshooting $buildProfile
        exit $LASTEXITCODE
    }
} finally {
    Pop-Location
}

$clawBin = Join-Path $rustDir "target\$buildProfile\claw.exe"
if (-not (Test-Path $clawBin)) {
    Write-Err "Expected binary not found at $clawBin"
    Show-Troubleshooting $buildProfile
    exit 1
}
Write-Ok "built $clawBin"

# --------------------------------------------------------------------------
Write-Step 5 $total "Verifying the installed binary"
if ($skipVerify) {
    Write-Warn2 "verification skipped (-NoVerify or CLAW_SKIP_VERIFY=1)"
} else {
    Write-Info "running: claw --version"
    $versionOut = & $clawBin --version 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Err "claw --version failed:`n$versionOut"
        Show-Troubleshooting $buildProfile
        exit 1
    }
    Write-Ok "claw --version -> $versionOut"

    Write-Info "running: claw --help (smoke test)"
    & $clawBin --help > $null 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Err "claw --help failed"
        Show-Troubleshooting $buildProfile
        exit 1
    }
    Write-Ok "claw --help responded"
}

# --------------------------------------------------------------------------
Write-Step 6 $total "Next steps"
$installedVersion = try { (& $clawBin --version 2>$null | Select-Object -First 1) } catch { "unknown" }
@"

Claw Code is built and ready.

  Binary:  $clawBin
  Version: $installedVersion
  Profile: $buildProfile

Try it out:

  # interactive REPL
  $clawBin

  # one-shot prompt
  $clawBin prompt "summarize this repository"

  # health check (run /doctor inside the REPL)
  $clawBin
  /doctor

Authentication — easiest inside the REPL (configures, verifies and persists):

  /provider use zhipu <token>       # Z.ai GLM (coding plan)
  /provider use kimi <api-key>      # Moonshot Kimi
  /provider use deepseek <api-key>  # DeepSeek
  /provider test                    # live 1-token connectivity check

Or via environment variables (they take priority over saved settings):

  `$env:ANTHROPIC_API_KEY = "sk-ant-..."
  `$env:ANTHROPIC_AUTH_TOKEN = "..."
  `$env:OPENAI_API_KEY = "sk-..."

To update later: git pull; .\install.ps1   (check with /upgrade inside the REPL)

For deeper docs, see USAGE.md and rust\README.md.
"@ | Write-Host
