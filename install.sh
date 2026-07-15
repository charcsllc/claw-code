#!/usr/bin/env bash
# Claw Code installer
#
# Detects the host OS, verifies the Rust toolchain (rustc + cargo),
# builds the `claw` binary from the `rust/` workspace, and runs a
# post-install verification step. Supports Linux, macOS, and WSL.
#
# Usage:
#   ./install.sh                # debug build (fast, default)
#   ./install.sh --release      # optimized release build
#   ./install.sh --no-verify    # skip post-install verification
#   ./install.sh --uninstall    # remove compiled claw binaries
#   ./install.sh --help         # print usage
#
# Environment overrides:
#   CLAW_BUILD_PROFILE=debug|release   same as --release toggle
#   CLAW_SKIP_VERIFY=1                 same as --no-verify

set -euo pipefail

# ---------------------------------------------------------------------------
# Pretty printing
# ---------------------------------------------------------------------------

if [ -t 1 ] && command -v tput >/dev/null 2>&1 && [ "$(tput colors 2>/dev/null || echo 0)" -ge 8 ]; then
    COLOR_RESET="$(tput sgr0)"
    COLOR_BOLD="$(tput bold)"
    COLOR_DIM="$(tput dim)"
    COLOR_RED="$(tput setaf 1)"
    COLOR_GREEN="$(tput setaf 2)"
    COLOR_YELLOW="$(tput setaf 3)"
    COLOR_BLUE="$(tput setaf 4)"
    COLOR_CYAN="$(tput setaf 6)"
else
    COLOR_RESET=""
    COLOR_BOLD=""
    COLOR_DIM=""
    COLOR_RED=""
    COLOR_GREEN=""
    COLOR_YELLOW=""
    COLOR_BLUE=""
    COLOR_CYAN=""
fi

CURRENT_STEP=0
TOTAL_STEPS=6

step() {
    CURRENT_STEP=$((CURRENT_STEP + 1))
    printf '\n%s[%d/%d]%s %s%s%s\n' \
        "${COLOR_BLUE}" "${CURRENT_STEP}" "${TOTAL_STEPS}" "${COLOR_RESET}" \
        "${COLOR_BOLD}" "$1" "${COLOR_RESET}"
}

info()  { printf '%s  ->%s %s\n' "${COLOR_CYAN}" "${COLOR_RESET}" "$1"; }
ok()    { printf '%s  ok%s %s\n' "${COLOR_GREEN}" "${COLOR_RESET}" "$1"; }
warn()  { printf '%s  warn%s %s\n' "${COLOR_YELLOW}" "${COLOR_RESET}" "$1"; }
error() { printf '%s  error%s %s\n' "${COLOR_RED}" "${COLOR_RESET}" "$1" 1>&2; }

print_banner() {
    printf '%s' "${COLOR_BOLD}"
    cat <<'EOF'
   ____  _                   ____          _
  / ___|| |  __ _ __      __ / ___|___   __| | ___
 | |    | | / _` |\ \ /\ / /| |   / _ \ / _` |/ _ \
 | |___ | || (_| | \ V  V / | |__| (_) | (_| |  __/
  \____||_| \__,_|  \_/\_/   \____\___/ \__,_|\___|
EOF
    printf '%s\n' "${COLOR_RESET}"
    printf '%sClaw Code installer%s\n' "${COLOR_DIM}" "${COLOR_RESET}"
}

print_usage() {
    cat <<'EOF'
Usage: ./install.sh [options]

Options:
  --release       Build the optimized release profile (slower, smaller binary).
  --debug         Build the debug profile (default, faster compile).
  --no-verify     Skip the post-install verification step.
  --uninstall     Remove the compiled claw binaries (rust/target/debug and
                  rust/target/release). Asks before deleting; optionally offers
                  to delete the user settings directory (~/.claw). Never touches
                  the source repository.
  -y, --yes       With --uninstall: skip the binary-removal confirmation.
                  The settings directory is still only deleted after an
                  explicit interactive "yes".
  -h, --help      Show this help text and exit.

Environment overrides:
  CLAW_BUILD_PROFILE   debug | release
  CLAW_SKIP_VERIFY     set to 1 to skip verification
  CLAW_CONFIG_HOME     settings directory considered by --uninstall (default ~/.claw)
EOF
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

BUILD_PROFILE="${CLAW_BUILD_PROFILE:-debug}"
SKIP_VERIFY="${CLAW_SKIP_VERIFY:-0}"
UNINSTALL="0"
ASSUME_YES="0"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --release)
            BUILD_PROFILE="release"
            ;;
        --debug)
            BUILD_PROFILE="debug"
            ;;
        --no-verify)
            SKIP_VERIFY="1"
            ;;
        --uninstall)
            UNINSTALL="1"
            ;;
        -y|--yes)
            ASSUME_YES="1"
            ;;
        -h|--help)
            print_usage
            exit 0
            ;;
        *)
            error "unknown argument: $1"
            print_usage
            exit 2
            ;;
    esac
    shift
done

case "${BUILD_PROFILE}" in
    debug|release) ;;
    *)
        error "invalid build profile: ${BUILD_PROFILE} (expected debug or release)"
        exit 2
        ;;
esac

# ---------------------------------------------------------------------------
# Uninstall mode
# ---------------------------------------------------------------------------

# Ask a yes/no question; default is No. Returns 0 only on an explicit yes.
confirm_no_default() {
    prompt="$1"
    if [ ! -t 0 ]; then
        warn "stdin is not a terminal — assuming 'no' for: ${prompt}"
        return 1
    fi
    printf '%s  ?%s  %s [y/N] ' "${COLOR_YELLOW}" "${COLOR_RESET}" "${prompt}"
    read -r answer || answer=""
    case "${answer}" in
        y|Y|yes|YES|Yes) return 0 ;;
        *) return 1 ;;
    esac
}

run_uninstall() {
    TOTAL_STEPS=3
    print_banner
    printf '%suninstall mode%s\n' "${COLOR_DIM}" "${COLOR_RESET}"

    # -- Step 1: locate compiled binaries -----------------------------------
    step "Locating compiled claw binaries"

    SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
    RUST_DIR="${SCRIPT_DIR}/rust"

    FOUND_BINS=()
    for profile in debug release; do
        candidate="${RUST_DIR}/target/${profile}/claw"
        if [ -f "${candidate}" ]; then
            info "found: ${candidate}"
            FOUND_BINS+=("${candidate}")
        fi
    done

    if [ "${#FOUND_BINS[@]}" -eq 0 ]; then
        ok "no compiled claw binaries found under ${RUST_DIR}/target — nothing to remove"
    fi

    # -- Step 2: remove binaries (with confirmation) ------------------------
    step "Removing compiled binaries"

    if [ "${#FOUND_BINS[@]}" -eq 0 ]; then
        info "skipped (nothing found)"
    else
        DO_REMOVE="0"
        if [ "${ASSUME_YES}" = "1" ]; then
            info "--yes given, skipping confirmation"
            DO_REMOVE="1"
        elif confirm_no_default "Delete the ${#FOUND_BINS[@]} binary file(s) listed above?"; then
            DO_REMOVE="1"
        fi

        if [ "${DO_REMOVE}" = "1" ]; then
            for bin in "${FOUND_BINS[@]}"; do
                rm -f -- "${bin}"
                ok "removed ${bin}"
            done
        else
            warn "binary removal declined — binaries kept"
        fi
    fi

    # -- Step 3: user settings directory (optional, default NO) -------------
    step "User settings directory (optional)"

    CONFIG_HOME="${CLAW_CONFIG_HOME:-${HOME:-}/.claw}"

    if [ -z "${CONFIG_HOME}" ] || [ "${CONFIG_HOME}" = "/" ] || [ "${CONFIG_HOME}" = "${HOME:-}" ]; then
        warn "refusing to consider suspicious settings path: '${CONFIG_HOME}'"
    elif [ ! -d "${CONFIG_HOME}" ]; then
        info "no settings directory found at ${CONFIG_HOME} — nothing to do"
    elif [ "${ASSUME_YES}" = "1" ] && [ ! -t 0 ]; then
        info "settings directory ${CONFIG_HOME} kept (deletion requires an explicit interactive yes)"
    else
        info "settings directory: ${CONFIG_HOME}"
        info "it holds settings.json (provider API keys), skills, commands and agents"
        if confirm_no_default "Also delete ${CONFIG_HOME}? This removes saved API keys and settings"; then
            rm -rf -- "${CONFIG_HOME}"
            ok "removed ${CONFIG_HOME}"
        else
            ok "settings directory kept (default)"
        fi
    fi

    printf '\n%sClaw Code uninstall finished.%s\n' "${COLOR_GREEN}" "${COLOR_RESET}"
    printf '%sThe source repository itself was not touched — delete the checkout manually if you no longer want it.%s\n' \
        "${COLOR_DIM}" "${COLOR_RESET}"
    printf '%sTo reinstall later: ./install.sh%s\n' "${COLOR_DIM}" "${COLOR_RESET}"
}

if [ "${UNINSTALL}" = "1" ]; then
    run_uninstall
    exit 0
fi

# ---------------------------------------------------------------------------
# Troubleshooting hints
# ---------------------------------------------------------------------------

print_troubleshooting() {
    cat <<EOF

${COLOR_BOLD}Troubleshooting${COLOR_RESET}
${COLOR_DIM}---------------${COLOR_RESET}

  ${COLOR_BOLD}1. Rust toolchain missing${COLOR_RESET}
     Install Rust via rustup:
       curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
     Then reload your shell or run:
       source "\$HOME/.cargo/env"

  ${COLOR_BOLD}2. Linux: missing system packages${COLOR_RESET}
     The build needs git, pkg-config, and OpenSSL headers.
     Debian/Ubuntu:
       sudo apt-get update && sudo apt-get install -y \\
         git pkg-config libssl-dev ca-certificates build-essential
     Fedora/RHEL:
       sudo dnf install -y git pkgconf-pkg-config openssl-devel gcc
     Arch:
       sudo pacman -S --needed git pkgconf openssl base-devel

  ${COLOR_BOLD}3. macOS: missing Xcode CLT${COLOR_RESET}
     Install the command line tools:
       xcode-select --install

  ${COLOR_BOLD}4. Windows users${COLOR_RESET}
     Native Windows: use the PowerShell installer from the repo root:
       .\\install.ps1
     Or run this script from inside a WSL distro (Ubuntu/Debian recommended).

  ${COLOR_BOLD}5. Build fails partway through${COLOR_RESET}
     Try a clean build:
       cd rust && cargo clean && cargo build --workspace
     If the failure mentions ring/openssl, double check step 2.

  ${COLOR_BOLD}6. 'claw' not found after install${COLOR_RESET}
     The binary lives at:
       rust/target/${BUILD_PROFILE}/claw
     Add it to your PATH or invoke it with the full path.

EOF
}

on_exit() {
    local rc=$?
    if [ "$rc" -ne 0 ]; then
        error "installation failed (exit ${rc})"
        print_troubleshooting
    fi
}
trap on_exit EXIT

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

require_cmd() {
    command -v "$1" >/dev/null 2>&1
}

# ---------------------------------------------------------------------------
# Step 1: detect OS / arch / WSL
# ---------------------------------------------------------------------------

print_banner
step "Detecting host environment"

UNAME_S="$(uname -s 2>/dev/null || echo unknown)"
UNAME_M="$(uname -m 2>/dev/null || echo unknown)"
OS_FAMILY="unknown"
IS_WSL="0"

case "${UNAME_S}" in
    Linux*)
        OS_FAMILY="linux"
        if grep -qiE 'microsoft|wsl' /proc/version 2>/dev/null; then
            IS_WSL="1"
        fi
        ;;
    Darwin*)
        OS_FAMILY="macos"
        ;;
    MINGW*|MSYS*|CYGWIN*)
        OS_FAMILY="windows-shell"
        ;;
esac

info "uname:        ${UNAME_S} ${UNAME_M}"
info "os family:    ${OS_FAMILY}"
if [ "${IS_WSL}" = "1" ]; then
    info "wsl:          yes"
fi

case "${OS_FAMILY}" in
    linux|macos)
        ok "supported platform detected"
        ;;
    windows-shell)
        error "Detected a native Windows shell (MSYS/Cygwin/MinGW)."
        error "On native Windows, use the PowerShell installer instead:"
        error "    .\\install.ps1"
        error "Or re-run this script from inside a WSL distribution."
        exit 1
        ;;
    *)
        error "Unsupported or unknown OS: ${UNAME_S}"
        error "Supported: Linux, macOS, Windows (native via install.ps1, or WSL)."
        exit 1
        ;;
esac

# ---------------------------------------------------------------------------
# Step 2: locate the Rust workspace
# ---------------------------------------------------------------------------

step "Locating the Rust workspace"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RUST_DIR="${SCRIPT_DIR}/rust"

if [ ! -d "${RUST_DIR}" ]; then
    error "Could not find rust/ workspace next to install.sh"
    error "Expected: ${RUST_DIR}"
    exit 1
fi

if [ ! -f "${RUST_DIR}/Cargo.toml" ]; then
    error "Missing ${RUST_DIR}/Cargo.toml — repository layout looks unexpected."
    exit 1
fi

ok "workspace at ${RUST_DIR}"

# ---------------------------------------------------------------------------
# Step 3: prerequisite checks
# ---------------------------------------------------------------------------

step "Checking prerequisites"

MISSING_PREREQS=0

if require_cmd rustc; then
    RUSTC_VERSION="$(rustc --version 2>/dev/null || echo 'unknown')"
    ok "rustc found: ${RUSTC_VERSION}"
else
    error "rustc not found in PATH"
    MISSING_PREREQS=1
fi

if require_cmd cargo; then
    CARGO_VERSION="$(cargo --version 2>/dev/null || echo 'unknown')"
    ok "cargo found: ${CARGO_VERSION}"
else
    error "cargo not found in PATH"
    MISSING_PREREQS=1
fi

if require_cmd git; then
    ok "git found:  $(git --version 2>/dev/null || echo 'unknown')"
else
    warn "git not found — some workflows (login, session export) may degrade"
fi

if [ "${OS_FAMILY}" = "linux" ]; then
    if require_cmd pkg-config; then
        ok "pkg-config found"
    else
        warn "pkg-config not found — may be required for OpenSSL-linked crates"
    fi
fi

if [ "${OS_FAMILY}" = "macos" ]; then
    if ! require_cmd cc && ! xcode-select -p >/dev/null 2>&1; then
        warn "Xcode command line tools not detected — run: xcode-select --install"
    fi
fi

if [ "${MISSING_PREREQS}" -ne 0 ]; then
    error "Missing required tools. See troubleshooting below."
    exit 1
fi

# ---------------------------------------------------------------------------
# Step 4: build the workspace
# ---------------------------------------------------------------------------

step "Building the claw workspace (${BUILD_PROFILE})"

# A full workspace build needs headroom for the linker; running out of disk
# mid-link fails with confusing SIGBUS/linker errors, not a clear message.
FREE_KB="$(df -Pk . 2>/dev/null | awk 'NR==2 {print $4}')"
if [ -n "${FREE_KB}" ] && [ "${FREE_KB}" -lt 3145728 ]; then
    warn "less than 3GB free on this disk ($((FREE_KB / 1024 / 1024))GB) — the build may fail; free space or run 'cargo clean' in rust/ first"
fi

CARGO_FLAGS=("build" "--workspace")
if [ "${BUILD_PROFILE}" = "release" ]; then
    CARGO_FLAGS+=("--release")
fi

info "running: cargo ${CARGO_FLAGS[*]}"
info "this may take a few minutes on the first build"

(
    cd "${RUST_DIR}"
    CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}" cargo "${CARGO_FLAGS[@]}"
)

CLAW_BIN="${RUST_DIR}/target/${BUILD_PROFILE}/claw"

if [ ! -x "${CLAW_BIN}" ]; then
    error "Expected binary not found at ${CLAW_BIN}"
    error "The build reported success but the binary is missing — check cargo output above."
    exit 1
fi

ok "built ${CLAW_BIN}"

# ---------------------------------------------------------------------------
# Step 5: post-install verification
# ---------------------------------------------------------------------------

step "Verifying the installed binary"

if [ "${SKIP_VERIFY}" = "1" ]; then
    warn "verification skipped (--no-verify or CLAW_SKIP_VERIFY=1)"
else
    info "running: claw --version"
    if VERSION_OUT="$("${CLAW_BIN}" --version 2>&1)"; then
        ok "claw --version -> ${VERSION_OUT}"
    else
        error "claw --version failed:"
        printf '%s\n' "${VERSION_OUT}" 1>&2
        exit 1
    fi

    info "running: claw --help (smoke test)"
    if "${CLAW_BIN}" --help >/dev/null 2>&1; then
        ok "claw --help responded"
    else
        error "claw --help failed"
        exit 1
    fi
fi

# ---------------------------------------------------------------------------
# Step 6: next steps
# ---------------------------------------------------------------------------

step "Next steps"

INSTALLED_VERSION="$("${CLAW_BIN}" --version 2>/dev/null | head -1 || echo "unknown")"

cat <<EOF
${COLOR_GREEN}Claw Code is built and ready.${COLOR_RESET}

  Binary:  ${COLOR_BOLD}${CLAW_BIN}${COLOR_RESET}
  Version: ${INSTALLED_VERSION}
  Profile: ${BUILD_PROFILE}

Try it out:

  ${COLOR_DIM}# interactive REPL${COLOR_RESET}
  ${CLAW_BIN}

  ${COLOR_DIM}# one-shot prompt${COLOR_RESET}
  ${CLAW_BIN} prompt "summarize this repository"

  ${COLOR_DIM}# health check (run /doctor inside the REPL)${COLOR_RESET}
  ${CLAW_BIN}
  /doctor

Authentication — easiest inside the REPL (configures, verifies and persists):

  /provider use zhipu <token>       ${COLOR_DIM}# Z.ai GLM (coding plan)${COLOR_RESET}
  /provider use kimi <api-key>      ${COLOR_DIM}# Moonshot Kimi${COLOR_RESET}
  /provider use deepseek <api-key>  ${COLOR_DIM}# DeepSeek${COLOR_RESET}
  /provider use ollama              ${COLOR_DIM}# local models, keyless${COLOR_RESET}
  /provider test                    ${COLOR_DIM}# live 1-token connectivity check${COLOR_RESET}

Or via environment variables (they take priority over saved settings):

  export ANTHROPIC_API_KEY="sk-ant-..."
  export ANTHROPIC_AUTH_TOKEN="..."      ${COLOR_DIM}# proxy/bearer auth${COLOR_RESET}
  export OPENAI_API_KEY="sk-..."         ${COLOR_DIM}# OpenAI-compatible models${COLOR_RESET}
  export OLLAMA_HOST="http://127.0.0.1:11434"  ${COLOR_DIM}# local models${COLOR_RESET}

To update later: git pull && ./install.sh   (check with /upgrade inside the REPL)

For deeper docs, see USAGE.md and rust/README.md.
EOF

# clear the failure trap on clean exit
trap - EXIT
