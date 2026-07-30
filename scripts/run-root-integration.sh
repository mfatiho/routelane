#!/bin/sh

set -u

IP=/usr/sbin/ip
NFT=/usr/sbin/nft
PING=/usr/bin/ping
SYSCTL=/usr/sbin/sysctl

NAMESPACES="
routelane-test-client
routelane-test-gateway-a
routelane-test-gateway-b
routelane-test-gateway-c
routelane-test-sink-a
routelane-test-sink-b
routelane-test-sink-c
routelane-test-preflight
"

VETHS="
rlt-ca
rlt-ga-c
rlt-ga-s
rlt-sa
rlt-cb
rlt-gb-c
rlt-gb-s
rlt-sb
rlt-cc
rlt-gc-c
rlt-gc-s
rlt-sc
rlt-pre-a
rlt-pre-b
"

cleanup() {
    for namespace in $NAMESPACES; do
        "$IP" netns del "$namespace" >/dev/null 2>&1 || true
    done
    for veth in $VETHS; do
        "$IP" link del "$veth" >/dev/null 2>&1 || true
    done
}

skip() {
    echo "SKIP: $1" >&2
    exit 77
}

trap cleanup EXIT HUP INT TERM

[ "$(id -u)" -eq 0 ] || skip "root network integration requires UID 0"
command -v cargo >/dev/null 2>&1 || skip "cargo is unavailable"

for binary in "$IP" "$NFT" "$PING" "$SYSCTL"; do
    [ -x "$binary" ] || skip "required executable is unavailable: $binary"
done

cleanup
if reason=$("$IP" netns add routelane-test-preflight 2>&1); then
    :
else
    skip "network namespace capability unavailable: ${reason:-unknown error}"
fi
if ! "$IP" netns exec routelane-test-preflight "$NFT" list ruleset >/dev/null 2>&1; then
    skip "nftables capability unavailable inside a network namespace"
fi
if ! "$IP" netns exec routelane-test-preflight "$SYSCTL" -q -w net.ipv4.ip_forward=1 >/dev/null 2>&1; then
    skip "IPv4 forwarding is unavailable inside a network namespace"
fi
if ! "$IP" link add rlt-pre-a type veth peer name rlt-pre-b >/dev/null 2>&1; then
    skip "veth capability unavailable"
fi
"$IP" link del rlt-pre-a
"$IP" netns del routelane-test-preflight

cargo test --test root --features root-integration -- --test-threads=1 --nocapture
