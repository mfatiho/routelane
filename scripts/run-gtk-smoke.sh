#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
if [[ "${ROUTELANE_GTK_TEST_SESSION:-}" != 1 ]]; then
    exec dbus-run-session --config-file scripts/gtk-smoke-bus.conf -- env ROUTELANE_GTK_TEST_SESSION=1 bash "$0"
fi
if ! command -v gtk4-broadwayd >/dev/null; then
    echo "NOT VERIFIED: gtk4-broadwayd is required for the isolated GTK smoke test" >&2
    exit 77
fi
smoke_root="$(mktemp -d /tmp/routelane-gtk-smoke.XXXXXX)"
export XDG_RUNTIME_DIR="$smoke_root"
export GDK_BACKEND=broadway
export BROADWAY_DISPLAY=:1
export GIO_USE_VFS=local
export GSETTINGS_BACKEND=memory
export GTK_USE_PORTAL=0
# A GTK test must never observe or activate the host's NetworkManager/Polkit.
export DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"
export G_DEBUG=fatal-warnings
gtk4-broadwayd --port=0 :1 >"$smoke_root/broadway.log" 2>&1 &
broadway_pid=$!
cleanup() {
    kill "$broadway_pid" 2>/dev/null || true
    wait "$broadway_pid" 2>/dev/null || true
    # Keep the small log directory available for diagnosis.
}
trap cleanup EXIT
for _ in {1..50}; do
    [[ -S "$smoke_root/broadway1.socket" ]] && break
    kill -0 "$broadway_pid" 2>/dev/null || { echo "Broadway failed: $smoke_root/broadway.log" >&2; exit 1; }
    sleep 0.1
done
cargo test --bin routelane gtk_smoke_ -- --ignored --test-threads=1 --nocapture
cargo test --lib nm_private_bus_write_contract -- --ignored --test-threads=1 --nocapture
