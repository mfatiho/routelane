#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"

PACKAGE_NAME="routelane"
MAINTAINER="RouteLane Project <mfatiho@users.noreply.github.com>"

cd "${REPO_ROOT}"

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo is required" >&2
    exit 1
fi

if ! command -v dpkg-deb >/dev/null 2>&1; then
    echo "error: dpkg-deb is required" >&2
    exit 1
fi

VERSION="$(
    awk -F '"' '
        $1 ~ /^version[[:space:]]*=/ {
            print $2
            exit
        }
    ' Cargo.toml
)"

if [[ -z "${VERSION}" ]]; then
    echo "error: could not read package version from Cargo.toml" >&2
    exit 1
fi

HOST_ARCH="$(uname -m)"
case "${HOST_ARCH}" in
    x86_64)
        DEB_ARCH="amd64"
        ;;
    aarch64)
        DEB_ARCH="arm64"
        ;;
    *)
        DEB_ARCH="$(dpkg --print-architecture)"
        ;;
esac

OUTPUT_DIR="${REPO_ROOT}/dist"
mkdir -p "${OUTPUT_DIR}"
BUILD_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/routelane-deb.XXXXXX")"
trap 'rm -rf -- "${BUILD_ROOT}"' EXIT
PACKAGE_ROOT="${BUILD_ROOT}/root"
OUTPUT_DEB="$(mktemp -p "${OUTPUT_DIR}" "${PACKAGE_NAME}_${VERSION}_${DEB_ARCH}.XXXXXX.deb")"

echo "Building release binaries..."
cargo build --release --bins

echo "Assembling package root: ${PACKAGE_ROOT}"
install -d -m 0755 \
    "${PACKAGE_ROOT}/DEBIAN" \
    "${PACKAGE_ROOT}/usr/bin" \
    "${PACKAGE_ROOT}/usr/lib/routelane" \
    "${PACKAGE_ROOT}/usr/lib/systemd/user" \
    "${PACKAGE_ROOT}/usr/lib/systemd/system" \
    "${PACKAGE_ROOT}/usr/share/applications" \
    "${PACKAGE_ROOT}/usr/share/dbus-1/system.d" \
    "${PACKAGE_ROOT}/usr/share/dbus-1/system-services" \
    "${PACKAGE_ROOT}/usr/share/polkit-1/actions"

install -m 0755 "${REPO_ROOT}/target/release/routelane" \
    "${PACKAGE_ROOT}/usr/bin/routelane"
install -m 0755 "${REPO_ROOT}/target/release/routelane-daemon" \
    "${PACKAGE_ROOT}/usr/bin/routelane-daemon"
install -m 0755 "${REPO_ROOT}/target/release/routelanectl" \
    "${PACKAGE_ROOT}/usr/bin/routelanectl"
install -m 0755 "${REPO_ROOT}/target/release/routelane-helper" \
    "${PACKAGE_ROOT}/usr/lib/routelane/routelane-helper"
install -m 0644 "${REPO_ROOT}/data/routelane-daemon.service" \
    "${PACKAGE_ROOT}/usr/lib/systemd/user/routelane-daemon.service"
install -m 0644 "${REPO_ROOT}/data/routelane-helper.service" \
    "${PACKAGE_ROOT}/usr/lib/systemd/system/routelane-helper.service"
install -m 0644 "${REPO_ROOT}/data/io.github.routelane.Helper.service" \
    "${PACKAGE_ROOT}/usr/share/dbus-1/system-services/io.github.routelane.Helper.service"
install -m 0644 "${REPO_ROOT}/data/io.github.routelane.Helper.conf" \
    "${PACKAGE_ROOT}/usr/share/dbus-1/system.d/io.github.routelane.Helper.conf"
install -m 0644 "${REPO_ROOT}/data/io.github.routelane.desktop" \
    "${PACKAGE_ROOT}/usr/share/applications/io.github.routelane.desktop"
install -m 0644 "${REPO_ROOT}/data/io.github.routelane.policy" \
    "${PACKAGE_ROOT}/usr/share/polkit-1/actions/io.github.routelane.policy"

cat > "${PACKAGE_ROOT}/DEBIAN/control" <<CONTROL
Package: ${PACKAGE_NAME}
Version: ${VERSION}
Section: net
Priority: optional
Architecture: ${DEB_ARCH}
Maintainer: ${MAINTAINER}
Depends: libgtk-4-1, libadwaita-1-0, nftables, iproute2, policykit-1, dbus, systemd
Description: Policy-based routing desktop application
 RouteLane routes selected domains or IP/CIDR targets through a chosen network interface.
CONTROL

cat > "${PACKAGE_ROOT}/DEBIAN/prerm" <<'PRERM'
#!/bin/sh
set -e

case "$1" in
    remove)
        systemctl stop routelane-helper.service >/dev/null 2>&1 || true
        systemctl disable routelane-helper.service >/dev/null 2>&1 || true

        if [ -d /run/user ]; then
            for run_dir in /run/user/*; do
                [ -d "$run_dir" ] || continue
                uid="$(basename "$run_dir")"
                user="$(getent passwd "$uid" | cut -d: -f1)" || continue
                [ -n "$user" ] || continue
                runuser -u "$user" -- env XDG_RUNTIME_DIR="$run_dir" \
                    systemctl --user stop routelane-daemon.service >/dev/null 2>&1 || true
            done
        fi
        ;;
esac

exit 0
PRERM
chmod 0755 "${PACKAGE_ROOT}/DEBIAN/prerm"

cat > "${PACKAGE_ROOT}/DEBIAN/postrm" <<'POSTRM'
#!/bin/sh
set -e

case "$1" in
    remove|purge)
        if [ -d /run/systemd/system ]; then
            systemctl daemon-reload >/dev/null 2>&1 || true
            systemctl reset-failed routelane-helper.service >/dev/null 2>&1 || true
        fi

        LEFTOVER=""
        if command -v ip >/dev/null 2>&1 \
            && ip rule show 2>/dev/null | grep -Eq '^100[0-9][0-9]:'; then
            LEFTOVER="1"
        fi
        if command -v nft >/dev/null 2>&1 \
            && nft list table inet routelane >/dev/null 2>&1; then
            LEFTOVER="1"
        fi

        echo "RouteLane has been removed. Each user's ~/.config/routelane/config.json was left in place." >&2

        if [ -n "$LEFTOVER" ]; then
            cat >&2 <<'EOF'
RouteLane was removed while its routing state was still active in the
kernel. This cannot be cleaned up automatically from a package removal
script -- RouteLane's privileged action requires an interactive Polkit
prompt that a non-interactive removal script cannot satisfy. Check for
and remove any leftover state manually:

    ip rule show | grep -E '^100[0-9][0-9]:'
    ip route show table 20000
    nft list table inet routelane

Remove what you find with 'sudo ip rule del ...', 'sudo ip route flush
table <n>', and 'sudo nft delete table inet routelane'.
EOF
        fi
        ;;
esac

exit 0
POSTRM
chmod 0755 "${PACKAGE_ROOT}/DEBIAN/postrm"

echo "Building package: ${OUTPUT_DEB}"
dpkg-deb --build --root-owner-group "${PACKAGE_ROOT}" "${OUTPUT_DEB}"

REQUIRED_PATHS=(
    "./usr/bin/routelane"
    "./usr/bin/routelane-daemon"
    "./usr/bin/routelanectl"
    "./usr/lib/routelane/routelane-helper"
    "./usr/lib/systemd/user/routelane-daemon.service"
    "./usr/lib/systemd/system/routelane-helper.service"
    "./usr/share/dbus-1/system-services/io.github.routelane.Helper.service"
    "./usr/share/dbus-1/system.d/io.github.routelane.Helper.conf"
    "./usr/share/polkit-1/actions/io.github.routelane.policy"
)

PACKAGE_CONTENTS="$(dpkg-deb --contents "${OUTPUT_DEB}")"
ACTUAL_MAINTAINER="$(dpkg-deb --field "${OUTPUT_DEB}" Maintainer)"
if [[ "${ACTUAL_MAINTAINER}" != "${MAINTAINER}" ]]; then
    echo "error: package Maintainer field is '${ACTUAL_MAINTAINER}'" >&2
    exit 1
fi
for required_path in "${REQUIRED_PATHS[@]}"; do
    if ! grep -Fq " ${required_path}" <<<"${PACKAGE_CONTENTS}"; then
        echo "error: package is missing ${required_path#./}" >&2
        exit 1
    fi
done

require_mode() {
    local mode="$1"
    local path="$2"
    if ! grep -Eq "^${mode} root/root +[0-9]+ .* ${path//./\\.}$" <<<"${PACKAGE_CONTENTS}"; then
        echo "error: package path ${path#./} does not have mode ${mode} and owner root/root" >&2
        exit 1
    fi
}

require_mode 'drwxr-xr-x' './usr/lib/routelane/'
require_mode '-rwxr-xr-x' './usr/bin/routelane'
require_mode '-rwxr-xr-x' './usr/bin/routelane-daemon'
require_mode '-rwxr-xr-x' './usr/bin/routelanectl'
require_mode '-rwxr-xr-x' './usr/lib/routelane/routelane-helper'
require_mode '-rw-r--r--' './usr/lib/systemd/user/routelane-daemon.service'
require_mode '-rw-r--r--' './usr/lib/systemd/system/routelane-helper.service'
require_mode '-rw-r--r--' './usr/share/dbus-1/system-services/io.github.routelane.Helper.service'
require_mode '-rw-r--r--' './usr/share/dbus-1/system.d/io.github.routelane.Helper.conf'
require_mode '-rw-r--r--' './usr/share/polkit-1/actions/io.github.routelane.policy'

CONTROL_EXTRACT_DIR="${BUILD_ROOT}/control"
dpkg-deb -e "${OUTPUT_DEB}" "${CONTROL_EXTRACT_DIR}"

require_control_mode() {
    local expected_mode="$1"
    local name="$2"
    local actual_mode
    actual_mode="$(stat -c '%a' "${CONTROL_EXTRACT_DIR}/${name}" 2>/dev/null || true)"
    if [[ "${actual_mode}" != "${expected_mode}" ]]; then
        echo "error: control script ${name} has mode '${actual_mode:-missing}', expected ${expected_mode}" >&2
        exit 1
    fi
}

require_control_mode '755' 'prerm'
require_control_mode '755' 'postrm'

echo "Created ${OUTPUT_DEB}"
