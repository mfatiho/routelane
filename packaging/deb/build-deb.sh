#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"

PACKAGE_NAME="routelane"
MAINTAINER="RouteLane Maintainers <maintainers@example.com>"

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

PACKAGE_ROOT="${REPO_ROOT}/target/deb/${PACKAGE_NAME}_${VERSION}_${DEB_ARCH}"
OUTPUT_DIR="${REPO_ROOT}/dist"
OUTPUT_DEB="${OUTPUT_DIR}/${PACKAGE_NAME}_${VERSION}_${DEB_ARCH}.deb"

echo "Building release binaries..."
cargo build --release --bins

echo "Assembling package root: ${PACKAGE_ROOT}"
rm -rf "${PACKAGE_ROOT}"
install -d -m 0755 \
    "${PACKAGE_ROOT}/DEBIAN" \
    "${PACKAGE_ROOT}/usr/bin" \
    "${PACKAGE_ROOT}/usr/lib/routelane" \
    "${PACKAGE_ROOT}/usr/share/applications" \
    "${PACKAGE_ROOT}/usr/share/polkit-1/actions"
mkdir -p "${OUTPUT_DIR}"

install -m 0755 "${REPO_ROOT}/target/release/routelane" \
    "${PACKAGE_ROOT}/usr/bin/routelane"
install -m 0755 "${REPO_ROOT}/target/release/routelane-helper" \
    "${PACKAGE_ROOT}/usr/lib/routelane/routelane-helper"
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
Depends: libgtk-4-1, libadwaita-1-0, policykit-1, iproute2
Description: Policy-based routing desktop application
 RouteLane routes selected domains or IP/CIDR targets through a chosen network interface.
CONTROL

echo "Building package: ${OUTPUT_DEB}"
dpkg-deb --build --root-owner-group "${PACKAGE_ROOT}" "${OUTPUT_DEB}"

echo "Created ${OUTPUT_DEB}"
