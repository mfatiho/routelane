#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/install-from-source.sh [options]
  --yes         Accept dependency and package installation prompts
  --dry-run     Print build/install/check commands without executing them
  --skip-deps   Skip build dependency detection and installation
  --build-only  Build the native package without installing or running doctor
  --help        Show this help
EOF
}

YES=false
DRY_RUN=false
SKIP_DEPS=false
BUILD_ONLY=false
for option in "$@"; do
    case "$option" in
        --yes) YES=true ;;
        --dry-run) DRY_RUN=true ;;
        --skip-deps) SKIP_DEPS=true ;;
        --build-only) BUILD_ONLY=true ;;
        --help) usage; exit 0 ;;
        *) usage >&2; echo "error: unknown option: $option" >&2; exit 2 ;;
    esac
done

if [[ "$(id -u)" == 0 ]]; then
    echo "error: run as your user; sudo is used only for the package manager" >&2
    exit 2
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
cd "$REPO_ROOT"

# Parse data only: never execute the contents of an os-release override.
ID=""
ID_LIKE=""
VERSION_ID=""
OS_RELEASE="${ROUTELANE_OS_RELEASE:-/etc/os-release}"
if [[ ! -r "$OS_RELEASE" ]]; then
    echo "error: cannot read $OS_RELEASE; see README manual build instructions" >&2
    exit 2
fi
while IFS='=' read -r key value || [[ -n "$key" ]]; do
    value="${value%\"}"
    value="${value#\"}"
    value="${value%\'}"
    value="${value#\'}"
    case "$key" in
        ID) ID="$value" ;;
        ID_LIKE) ID_LIKE="$value" ;;
        VERSION_ID) VERSION_ID="$value" ;;
    esac
done < "$OS_RELEASE"

case " $ID $ID_LIKE " in
    *" ubuntu "*|*" debian "*)
        FAMILY=deb
        DEPS=(build-essential pkg-config libgtk-4-dev libadwaita-1-dev)
        ;;
    *" fedora "*)
        FAMILY=rpm
        DEPS=(gcc pkgconf-pkg-config gtk4-devel libadwaita-devel rpm-build)
        ;;
    *)
        echo "error: unsupported distribution '$ID'; see README manual build instructions" >&2
        exit 2
        ;;
esac

case "$ID:$VERSION_ID" in
    ubuntu:22.04|ubuntu:24.04|ubuntu:26.04|debian:12|debian:13|fedora:43|fedora:44) ;;
    *) echo "warning: $ID $VERSION_ID is not a validated release; continuing" >&2 ;;
esac

confirm() {
    local answer
    if "$YES" || "$DRY_RUN"; then
        return
    fi
    read -r -p "$1 [y/N] " answer || answer=""
    case "$answer" in
        y|Y|yes|YES) ;;
        *) echo "Cancelled." >&2; exit 1 ;;
    esac
}

run_command() {
    if "$DRY_RUN"; then
        printf '[dry-run]'
        printf ' %q' "$@"
        printf '\n'
    else
        "$@"
    fi
}

if ! "$SKIP_DEPS"; then
    missing=()
    for package in "${DEPS[@]}"; do
        if [[ "$FAMILY" == deb ]]; then
            installed="$(dpkg-query -W -f='${Status}' "$package" 2>/dev/null || true)"
            [[ "$installed" == *"install ok installed"* ]] || missing+=("$package")
        elif ! rpm -q "$package" >/dev/null 2>&1; then
            missing+=("$package")
        fi
    done
    if [[ "${#missing[@]}" -gt 0 ]]; then
        echo "Missing build dependencies: ${missing[*]}"
        confirm "Install missing build dependencies?"
        if [[ "$FAMILY" == deb ]]; then
            run_command sudo apt-get update
            run_command sudo apt-get install -y "${missing[@]}"
        else
            run_command sudo dnf install -y "${missing[@]}"
        fi
    fi
fi

rust_help() {
    cat >&2 <<'EOF'
Install Rust with rustup, or update your toolchain, then retry:
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  rustup update stable
EOF
    exit 3
}

MSRV="$(awk -F '"' '
    /^\[package\]/ { package_section = 1; next }
    /^\[/ { package_section = 0 }
    package_section && /^[[:space:]]*rust-version[[:space:]]*=/ { print $2; exit }
' Cargo.toml)"
if ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
    echo "error: cargo and rustc (Rust >= $MSRV) must be on PATH" >&2
    rust_help
fi
RUST_VERSION="$(rustc --version 2>/dev/null || true)"
RUST_VERSION="${RUST_VERSION#rustc }"
RUST_VERSION="${RUST_VERSION%% *}"
# Compare numeric components (including multi-digit minor/patch versions).
version_at_least() {
    local actual_major actual_minor actual_patch minimum_major minimum_minor minimum_patch
    [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || return 1
    [[ "$2" =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]] || return 1
    IFS=. read -r actual_major actual_minor actual_patch <<< "$1"
    IFS=. read -r minimum_major minimum_minor minimum_patch <<< "$2"
    (( actual_major > minimum_major ||
       (actual_major == minimum_major && actual_minor > minimum_minor) ||
       (actual_major == minimum_major && actual_minor == minimum_minor &&
        actual_patch >= ${minimum_patch:-0}) ))
}
if ! version_at_least "$RUST_VERSION" "$MSRV"; then
    echo "error: Rust >= $MSRV is required; found '${RUST_VERSION:-unknown}'" >&2
    rust_help
fi

BUILD_SCRIPT="packaging/$FAMILY/build-$FAMILY.sh"
if "$DRY_RUN"; then
    run_command bash "$BUILD_SCRIPT"
    # The actual name is only known after the packaging script finishes.
    PACKAGE_PATH="$REPO_ROOT/dist/PACKAGE_FROM_BUILD_OUTPUT.$FAMILY"
else
    if build_output="$(bash "$BUILD_SCRIPT")"; then
        printf '%s\n' "$build_output"
    else
        build_code=$?
        printf '%s\n' "$build_output" >&2
        exit "$build_code"
    fi
    PACKAGE_PATH="$(awk '/^Created / { path = substr($0, 9) } END { print path }' <<< "$build_output")"
    if [[ -z "$PACKAGE_PATH" ]]; then
        echo "error: build produced no Created <path> line" >&2
        exit 1
    fi
    if [[ "$PACKAGE_PATH" != /* ]]; then
        PACKAGE_PATH="$REPO_ROOT/$PACKAGE_PATH"
    fi
fi

if "$BUILD_ONLY"; then
    exit 0
fi
confirm "Install $PACKAGE_PATH?"
if [[ "$FAMILY" == deb ]]; then
    run_command sudo apt install -y "$PACKAGE_PATH"
else
    run_command sudo dnf install -y "$PACKAGE_PATH"
fi
if run_command routelanectl doctor; then
    exit 0
else
    doctor_code=$?
    echo "The package is installed, but doctor checks failed (exit $doctor_code)." >&2
    exit "$doctor_code"
fi
