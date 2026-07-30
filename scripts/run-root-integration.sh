#!/bin/sh

set -u

IP=/usr/sbin/ip
NFT=/usr/sbin/nft
PING=/usr/bin/ping
SYSCTL=/usr/sbin/sysctl
REGISTRY=
TOKEN=

cleanup() {
    [ -n "$REGISTRY" ] || return 0
    case "$REGISTRY" in
        /tmp/routelane-test-registry.??????) ;;
        *) return 0 ;;
    esac
    [ -f "$REGISTRY" ] && [ ! -L "$REGISTRY" ] || return 0
    [ "$(/usr/bin/stat -c '%u:%a' "$REGISTRY" 2>/dev/null)" = "0:600" ] || return 0

    while IFS=' ' read -r kind name extra; do
        [ -z "${extra:-}" ] || continue
        case "$kind:$name" in
            namespace:routelane-test-"$TOKEN"-*)
                "$IP" netns del "$name" >/dev/null 2>&1 || true
                ;;
            veth:rl"$TOKEN"*)
                [ "${#name}" -le 15 ] || continue
                "$IP" link del "$name" >/dev/null 2>&1 || true
                ;;
        esac
    done <"$REGISTRY"
    : >"$REGISTRY"
    /usr/bin/rm -f -- "$REGISTRY"
}

skip() {
    echo "SKIP: $1" >&2
    exit 77
}

trap cleanup EXIT HUP INT TERM

[ "$(id -u)" -eq 0 ] || skip "root network integration requires UID 0"
command -v cargo >/dev/null 2>&1 || skip "cargo is unavailable"

for binary in "$IP" "$NFT" "$PING" "$SYSCTL" /usr/bin/mktemp /usr/bin/stat /usr/bin/rm; do
    [ -x "$binary" ] || skip "required executable is unavailable: $binary"
done

REGISTRY=$(/usr/bin/mktemp /tmp/routelane-test-registry.XXXXXX) ||
    skip "could not create an exclusive root integration registry"
TOKEN=${REGISTRY##*.}
case "$TOKEN" in
    ??????) ;;
    *) echo "ERROR: invalid root integration token" >&2; exit 1 ;;
esac
case "$TOKEN" in
    *[!A-Za-z0-9]*) echo "ERROR: unsafe root integration token" >&2; exit 1 ;;
esac
chmod 0600 "$REGISTRY"
export ROUTELANE_TEST_REGISTRY="$REGISTRY"
export ROUTELANE_TEST_TOKEN="$TOKEN"

PREFLIGHT_NS="routelane-test-$TOKEN-preflight"
PREFLIGHT_LEFT="rl${TOKEN}pa"
PREFLIGHT_RIGHT="rl${TOKEN}pb"

for existing in $("$IP" netns list); do
    if [ "$existing" = "$PREFLIGHT_NS" ]; then
        echo "ERROR: root integration namespace collision: $PREFLIGHT_NS" >&2
        exit 1
    fi
done
for veth in "$PREFLIGHT_LEFT" "$PREFLIGHT_RIGHT"; do
    if "$IP" link show dev "$veth" >/dev/null 2>&1; then
        echo "ERROR: root integration veth collision: $veth" >&2
        exit 1
    fi
done

if reason=$("$IP" netns add "$PREFLIGHT_NS" 2>&1); then
    echo "namespace $PREFLIGHT_NS" >>"$REGISTRY"
    :
else
    skip "network namespace capability unavailable: ${reason:-unknown error}"
fi
if ! "$IP" netns exec "$PREFLIGHT_NS" "$NFT" list ruleset >/dev/null 2>&1; then
    skip "nftables capability unavailable inside a network namespace"
fi
if ! "$IP" netns exec "$PREFLIGHT_NS" "$SYSCTL" -q -w net.ipv4.ip_forward=1 >/dev/null 2>&1; then
    skip "IPv4 forwarding is unavailable inside a network namespace"
fi
if ! "$IP" link add "$PREFLIGHT_LEFT" type veth peer name "$PREFLIGHT_RIGHT" >/dev/null 2>&1; then
    skip "veth capability unavailable"
fi
echo "veth $PREFLIGHT_LEFT" >>"$REGISTRY"
echo "veth $PREFLIGHT_RIGHT" >>"$REGISTRY"
"$IP" link del "$PREFLIGHT_LEFT"
"$IP" netns del "$PREFLIGHT_NS"
: >"$REGISTRY"

cargo test --test root --features root-integration -- --test-threads=1 --nocapture
