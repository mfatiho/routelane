#!/usr/bin/env bash
# Builds the Fedora rpm. Run it in the target build environment (a Fedora
# host or VM with rpm-build); the rpm is not installed.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"

PACKAGE_NAME="routelane"

cd "${REPO_ROOT}"

for tool in cargo rpmbuild rpm; do
    if ! command -v "${tool}" >/dev/null 2>&1; then
        echo "error: ${tool} is required" >&2
        exit 1
    fi
done

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

RPM_ARCH="$(uname -m)"
case "${RPM_ARCH}" in
    x86_64 | aarch64) ;;
    *)
        echo "error: unsupported architecture ${RPM_ARCH}" >&2
        exit 1
        ;;
esac

OUTPUT_DIR="${REPO_ROOT}/dist"
mkdir -p "${OUTPUT_DIR}"
BUILD_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/routelane-rpm.XXXXXX")"
trap 'rm -rf -- "${BUILD_ROOT}"' EXIT

echo "Building release binaries..."
cargo build --release --bins --locked

echo "Building rpm in ${BUILD_ROOT}"
rpmbuild -bb \
    --define "_topdir ${BUILD_ROOT}" \
    --define "routelane_version ${VERSION}" \
    --define "routelane_root ${REPO_ROOT}" \
    --target "${RPM_ARCH}" \
    "${SCRIPT_DIR}/routelane.spec"

# Exactly one artifact, selected by name, version and architecture.
mapfile -t BUILT < <(find "${BUILD_ROOT}/RPMS/${RPM_ARCH}" -maxdepth 1 -type f \
    -name "${PACKAGE_NAME}-${VERSION}-1*.${RPM_ARCH}.rpm")
if [[ "${#BUILT[@]}" -ne 1 ]]; then
    echo "error: expected one ${PACKAGE_NAME}-${VERSION} rpm, found ${#BUILT[@]}" >&2
    exit 1
fi
BUILT_RPM="${BUILT[0]}"

REQUIRED_FILES=(
    "-rwxr-xr-x root/root /usr/bin/routelane"
    "-rwxr-xr-x root/root /usr/bin/routelane-daemon"
    "-rwxr-xr-x root/root /usr/bin/routelanectl"
    "drwxr-xr-x root/root /usr/lib/routelane"
    "-rwxr-xr-x root/root /usr/lib/routelane/routelane-helper"
    "-rw-r--r-- root/root /usr/lib/systemd/user/routelane-daemon.service"
    "-rw-r--r-- root/root /usr/lib/systemd/system/routelane-helper.service"
    "-rw-r--r-- root/root /usr/lib/tmpfiles.d/routelane.conf"
    "-rw-r--r-- root/root /usr/share/dbus-1/system-services/io.github.routelane.Helper.service"
    "-rw-r--r-- root/root /usr/share/dbus-1/system.d/io.github.routelane.Helper.conf"
    "-rw-r--r-- root/root /usr/share/applications/io.github.routelane.desktop"
    "-rw-r--r-- root/root /usr/share/polkit-1/actions/io.github.routelane.policy"
)

PACKAGE_CONTENTS="$(rpm -qp --qf '[%{FILEMODES:perms} %{FILEUSERNAME}/%{FILEGROUPNAME} %{FILENAMES}\n]' "${BUILT_RPM}")"
for required in "${REQUIRED_FILES[@]}"; do
    if [[ "$(grep -Fxc -- "${required}" <<<"${PACKAGE_CONTENTS}")" -ne 1 ]]; then
        echo "error: package does not contain exactly one '${required}'" >&2
        exit 1
    fi
done

SCRIPTS="$(rpm -qp --scripts "${BUILT_RPM}")"
for scriptlet in "postinstall scriptlet" "preuninstall scriptlet" "postuninstall scriptlet"; do
    if ! grep -Fq "${scriptlet}" <<<"${SCRIPTS}"; then
        echo "error: package is missing its ${scriptlet}" >&2
        exit 1
    fi
done

OUTPUT_RPM="$(mktemp -p "${OUTPUT_DIR}" "${PACKAGE_NAME}-${VERSION}-1.${RPM_ARCH}.XXXXXX.rpm")"
cp -- "${BUILT_RPM}" "${OUTPUT_RPM}"
echo "Created ${OUTPUT_RPM}"
