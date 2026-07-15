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
#   .\install.ps1 -Uninstall   # remove compiled claw binaries
#   .\install.ps1 -Help        # print usage
#
# If script execution is blocked, run once:
#   Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass

[CmdletBinding()]
param(
    [switch]$Release,
    [switch]$NoVerify,
    [switch]$Uninstall,
    [switch]$Yes,
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
  -Uninstall    Remove the compiled claw binaries (rust\target\debug and
                rust\target\release). Asks before deleting; optionally offers
                to delete the user settings directory (~\.claw). Never touches
                the source repository.
  -Yes          With -Uninstall: skip the binary-removal confirmation.
                The settings directory is still only deleted after an
                explicit interactive "yes".
  -Help         Show this help text and exit.

Environment overrides:
  CLAW_BUILD_PROFILE   debug | release
  CLAW_SKIP_VERIFY     set to 1 to skip verification
  CLAW_CONFIG_HOME     settings directory considered by -Uninstall (default ~\.claw)
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

# Ask a yes/no question; default is No. Returns $true only on an explicit yes.
function Confirm-NoDefault([string]$prompt) {
    try {
        $answer = Read-Host "  ?   $prompt [y/N]"
    } catch {
        Write-Warn2 "no interactive input available — assuming 'no'"
        return $false
    }
    return $answer -match '^(y|yes)$'
}

if ($Uninstall) {
    Write-Host "uninstall mode" -ForegroundColor DarkGray
    $total = 3

    # -- Step 1: locate compiled binaries ----------------------------------
    Write-Step 1 $total "Locating compiled claw binaries"
    $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    $rustDir = Join-Path $scriptDir 'rust'
    $foundBins = @()
    foreach ($p in @('debug', 'release')) {
        $candidate = Join-Path $rustDir "target\$p\claw.exe"
        if (Test-Path $candidate -PathType Leaf) {
            Write-Info "found: $candidate"
            $foundBins += $candidate
        }
    }
    if ($foundBins.Count -eq 0) {
        Write-Ok "no compiled claw binaries found under $rustDir\target — nothing to remove"
    }

    # -- Step 2: remove binaries (with confirmation) ------------------------
    Write-Step 2 $total "Removing compiled binaries"
    if ($foundBins.Count -eq 0) {
        Write-Info "skipped (nothing found)"
    } else {
        $doRemove = $false
        if ($Yes) {
            Write-Info "-Yes given, skipping confirmation"
            $doRemove = $true
        } elseif (Confirm-NoDefault "Delete the $($foundBins.Count) binary file(s) listed above?") {
            $doRemove = $true
        }
        if ($doRemove) {
            foreach ($bin in $foundBins) {
                Remove-Item -LiteralPath $bin -Force
                Write-Ok "removed $bin"
            }
        } else {
            Write-Warn2 "binary removal declined — binaries kept"
        }
    }

    # -- Step 3: user settings directory (optional, default NO) -------------
    Write-Step 3 $total "User settings directory (optional)"
    $configHome = if ($env:CLAW_CONFIG_HOME) { $env:CLAW_CONFIG_HOME }
             elseif ($env:HOME) { Join-Path $env:HOME '.claw' }
             else { Join-Path $env:USERPROFILE '.claw' }
    if (-not $configHome -or $configHome -eq '\' -or $configHome -eq $env:USERPROFILE) {
        Write-Warn2 "refusing to consider suspicious settings path: '$configHome'"
    } elseif (-not (Test-Path $configHome -PathType Container)) {
        Write-Info "no settings directory found at $configHome — nothing to do"
    } else {
        Write-Info "settings directory: $configHome"
        Write-Info "it holds settings.json (provider API keys), skills, commands and agents"
        if (Confirm-NoDefault "Also delete $configHome? This removes saved API keys and settings") {
            Remove-Item -LiteralPath $configHome -Recurse -Force
            Write-Ok "removed $configHome"
        } else {
            Write-Ok "settings directory kept (default)"
        }
    }

    Write-Host ""
    Write-Host "Claw Code uninstall finished." -ForegroundColor Green
    Write-Host "The source repository itself was not touched — delete the checkout manually if you no longer want it." -ForegroundColor DarkGray
    Write-Host "To reinstall later: .\install.ps1" -ForegroundColor DarkGray
    exit 0
}

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
