#!/bin/sh
# Real system-bus/Polkit acceptance for RouteLane runtime authority
# (docs/primary-failover-verification.md). ONLY for an authorized disposable
# VM: it installs the package and adds a test user and Polkit rule.
# Usage: ROUTELANE_DISPOSABLE_VM=$(hostname) sh system_bus_acceptance.sh PKG.deb [NEWER.deb]
# With NEWER.deb an armed runtime is also upgraded in place.
# Every change is VM-local and removed by the EXIT trap: test user, Polkit
# rule and marker, a /run systemd drop-in that confines the real helper
# service to a run-owned network namespace, the namespaces, and the package.
set -eu
[ "$(id -u)" = 0 ]
[ "${ROUTELANE_DISPOSABLE_VM:-}" = "$(hostname)" ] || { echo "refusing: set ROUTELANE_DISPOSABLE_VM to this disposable VM hostname" >&2; exit 2; }
DEB=$1
UPGRADE_DEB=${2:-}
TOK=sb$(od -An -N3 -tx1 /dev/urandom | tr -d ' \n')
NS_C=routelane-test-$TOK-client
NS_GA=routelane-test-$TOK-gwa
NS_GC=routelane-test-$TOK-gwc
NS_SA=routelane-test-$TOK-sinka
NS_SC=routelane-test-$TOK-sinkc
CA=rl${TOK}ca; GAC=rl${TOK}gac; GAS=rl${TOK}gas; SA=rl${TOK}sa
CC=rl${TOK}cc; GCC=rl${TOK}gcc; GCS=rl${TOK}gcs; SC=rl${TOK}sc
DROPIN=/run/systemd/system/routelane-helper.service.d
RULE=/etc/polkit-1/rules.d/49-routelane-test.rules
MARKER=/run/routelane-test-allow
# polkit 0.105 (Ubuntu 22.04) reads .pkla files instead of JavaScript rules.
PKLA=/etc/polkit-1/localauthority/50-local.d/49-routelane-test.pkla
TESTUSER=rltest
OTHERUSER=rlother
BUS="busctl --system --json=short"
DEST="io.github.routelane.Helper /io/github/routelane/Helper io.github.routelane.Helper2"
SHARED=198.51.100.77
PIDS=""

say() { echo "EVIDENCE: $*"; }
fail() { echo "FAIL: $*"; exit 1; }

teardown() {
    set +e
    for pid in $PIDS; do kill "$pid" 2>/dev/null; done
    systemctl stop routelane-helper.service 2>/dev/null
    rm -rf "$DROPIN"; rm -f "$RULE" "$MARKER" "$PKLA"
    systemctl daemon-reload
    for ns in $NS_C $NS_GA $NS_GC $NS_SA $NS_SC; do ip netns del "$ns" 2>/dev/null; done
    for user in $TESTUSER $OTHERUSER; do
        id "$user" >/dev/null 2>&1 || continue
        pkill -KILL -u "$user"; loginctl terminate-user "$user" 2>/dev/null; sleep 0.5
        userdel -r -f "$user" 2>/dev/null
    done
    rm -rf /run/routelane
    DEBIAN_FRONTEND=noninteractive apt-get remove -y routelane >/dev/null 2>&1
    echo "TEARDOWN: namespaces=$(ip netns list | grep -c "$TOK") package=$(dpkg -l routelane 2>/dev/null | grep -c '^ii') users=$(getent passwd $TESTUSER $OTHERUSER | grep -c .)"
}
trap teardown EXIT

DEBIAN_FRONTEND=noninteractive apt-get install -y "$DEB" >/tmp/routelane-apt.log 2>&1 || { tail /tmp/routelane-apt.log; fail "package install"; }
say "installed $(dpkg-query -W -f='${Package} ${Version}' routelane); systemd $(systemctl --version | head -1 | cut -d' ' -f2); polkit $(pkaction --version | awk '{print $NF}'); nft $(nft --version | cut -d' ' -f2); kernel $(uname -r)"

# --- topology: client with lanes A (primary) and C (backup) -------------------
for ns in $NS_C $NS_GA $NS_GC $NS_SA $NS_SC; do ip netns add "$ns"; ip -n "$ns" link set lo up; done
lane() { # client_if gw_client_if gw_sink_if sink_if gw_ns sink_ns n
    ip link add "$1" type veth peer name "$2"; ip link add "$3" type veth peer name "$4"
    ip link set "$1" netns "$NS_C"; ip link set "$2" netns "$5"; ip link set "$3" netns "$5"; ip link set "$4" netns "$6"
    ip -n "$NS_C" addr add "10.10.$7.2/24" dev "$1"; ip -n "$NS_C" link set "$1" up
    ip -n "$5" addr add "10.10.$7.1/24" dev "$2"; ip -n "$5" link set "$2" up
    ip -n "$5" addr add "10.20.$7.1/24" dev "$3"; ip -n "$5" link set "$3" up
    ip -n "$6" addr add "10.20.$7.2/24" dev "$4"; ip -n "$6" link set "$4" up
    ip netns exec "$5" sysctl -q -w net.ipv4.ip_forward=1
    ip -n "$6" addr add "$SHARED/32" dev lo
    ip -n "$5" route add "$SHARED/32" via "10.20.$7.2" dev "$3"
    ip -n "$6" route add "10.10.$7.0/24" via "10.20.$7.1" dev "$4"
}
lane "$CA" "$GAC" "$GAS" "$SA" "$NS_GA" "$NS_SA" 1
lane "$CC" "$GCC" "$GCS" "$SC" "$NS_GC" "$NS_SC" 3
ip -n "$NS_C" route add default via 10.10.1.1 dev "$CA" metric 100
ip -n "$NS_C" route add default via 10.10.3.1 dev "$CC" metric 200
# Strict reverse-path filtering inside the run-owned client namespace only.
for i in all "$CA" "$CC"; do ip netns exec "$NS_C" sysctl -q -w "net.ipv4.conf.$i.rp_filter=1" "net.ipv4.conf.$i.src_valid_mark=0"; done

ECHO='import socket,sys
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1); s.bind((sys.argv[1],7000)); s.listen(8)
while True:
    c,a=s.accept(); c.sendall(("peer=%s\n"%a[0]).encode()); c.close()'
for ns in $NS_SA $NS_SC; do ip netns exec "$ns" python3 -c "$ECHO" "$SHARED" & PIDS="$PIDS $!"; done
sleep 1
probe() { ip netns exec "$NS_C" python3 -c 'import socket,sys
s=socket.create_connection((sys.argv[1],7000),timeout=3); print(s.recv(64).decode().strip())' "$SHARED"; }
[ "$(probe)" = "peer=10.10.1.2" ] || fail "pre-activation probe"

# --- confine the real helper service to the client namespace -----------------
mkdir -p "$DROPIN"
printf '[Service]\nNetworkNamespacePath=/run/netns/%s\n' "$NS_C" > "$DROPIN/50-test-netns.conf"
systemctl daemon-reload
useradd -m "$TESTUSER"
useradd -m "$OTHERUSER"
if pkaction --version | grep -q ' 0\.105'; then
    allow() {
        printf '[routelane test]\nIdentity=unix-user:%s\nAction=io.github.routelane.apply\nResultAny=yes\nResultInactive=yes\nResultActive=yes\n' "$TESTUSER" > "$PKLA"
        sleep 2
    }
    withdraw() { rm -f "$PKLA"; sleep 2; }
else
    cat > "$RULE" <<EOF
polkit.addRule(function(action, subject) {
    if (action.id == "io.github.routelane.apply" && subject.user == "$TESTUSER") {
        try { polkit.spawn(["/usr/bin/test", "-e", "$MARKER"]); return polkit.Result.YES; }
        catch (error) { return polkit.Result.NO; }
    }
});
EOF
    allow() { touch "$MARKER"; }
    withdraw() { rm -f "$MARKER"; }
fi
sleep 1
as_user() { sudo -u "$TESTUSER" "$@"; }
activation() { # backups-json [extra-interface]
    python3 - "$CA" "$CC" "$1" "${2:-}" <<'PY'
import json,sys
ca,cc,backups,extra=sys.argv[1],sys.argv[2],json.loads(sys.argv[3]),sys.argv[4]
lane=lambda i,n:{"id":i,"name":i,"interface":n,"fallback_lane":None,"failure_policy":"Block"}
lanes=[lane("lane-a",ca),lane("lane-c",cc)]+([lane("lane-x",extra)] if extra else [])
# Broad failover of unmatched traffic is the explicit UsePrimary opt-in.
print(json.dumps({"protocol_version":3,"profile":{"id":"sysbus","name":"SysBus","enabled":True,
  "lanes":lanes,"rules":[],
  "routing":{"unmatched_traffic":"UsePrimary","primary":{"Lane":"lane-a"},"backups":backups,"automatic_failover":True}},
  "resolutions":{"rules":{}},"expected_generation":0}))
PY
}
field() { python3 -c 'import json,sys
d=json.loads(json.loads(sys.stdin.read())["data"][0])
for k in sys.argv[1].split("."): d=d[k] if not isinstance(d,list) else d[int(k)]
print(json.dumps(d))' "$1"; }

# The read-only protocol query needs no authorization.
PROTOCOL=$(as_user $BUS call $DEST GetRuntimeProtocol | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"][0])') || fail "GetRuntimeProtocol"
[ "$PROTOCOL" = 3 ] || fail "runtime protocol is $PROTOCOL, expected 3"
say "non-root $TESTUSER read GetRuntimeProtocol=3 without authorization"
allow
ARMED=$(as_user $BUS call $DEST ActivateRuntime s "$(activation '["lane-c"]')") || fail "authorized activation"
[ "$(echo "$ARMED" | field status.phase)" = '"Armed"' ] || fail "activation not armed: $ARMED"
[ "$(echo "$ARMED" | field status.unmatched_traffic)" = '"UsePrimary"' ] || fail "armed status does not report its mode: $ARMED"
say "activation as $TESTUSER armed: effective $(echo "$ARMED" | field status.decision.effective.v4)"
HELPER=$(systemctl show -p MainPID --value routelane-helper.service)
[ "$(stat -L -c %i /proc/$HELPER/ns/net)" = "$(stat -L -c %i /run/netns/$NS_C)" ] || fail "helper is not confined to the test namespace"
say "real service pid $HELPER runs in run-owned netns $NS_C; sandbox wrote src_valid_mark ca=$(ip netns exec "$NS_C" cat /proc/sys/net/ipv4/conf/$CA/src_valid_mark) cc=$(ip netns exec "$NS_C" cat /proc/sys/net/ipv4/conf/$CC/src_valid_mark) all=$(ip netns exec "$NS_C" cat /proc/sys/net/ipv4/conf/all/src_valid_mark)"
[ "$(probe)" = "peer=10.10.1.2" ] || fail "primary probe"

# --- authorization expires; the outage must still fail over -----------------
withdraw
if OUT=$(as_user $BUS call $DEST Cleanup 2>&1); then fail "cleanup allowed without authorization"; fi
echo "$OUT" | grep -q "Access denied" || fail "cleanup failed for another reason: $OUT"
say "authorization withdrawn: Cleanup by $TESTUSER is denied"
command -v gdbus >/dev/null || fail "gdbus unavailable for the non-root signal check"
# gdbus monitor subscribes with an ordinary match rule (no eavesdropping).
as_user timeout 20 gdbus monitor --system --dest io.github.routelane.Helper --object-path /io/github/routelane/Helper > /tmp/routelane-signal.json 2>&1 & WAITER=$!
sleep 1
ip -n "$NS_GA" link set "$GAC" down   # carrier loss on lane A
for _ in $(seq 1 50); do
    EFFECTIVE=$(as_user $BUS call $DEST Inspect | field runtime.decision.effective.v4)
    [ "$EFFECTIVE" = '{"Lane": "lane-c"}' ] && break; sleep 0.1
done
[ "$EFFECTIVE" = '{"Lane": "lane-c"}' ] || fail "no automatic failover: $EFFECTIVE"
[ "$(probe)" = "peer=10.10.3.2" ] || fail "new flow did not use the backup"
say "carrier loss failed over to lane-c with no authorization; new flow source 10.10.3.2"
sleep 1; kill $WAITER 2>/dev/null; wait $WAITER 2>/dev/null || true
grep 'RuntimeChanged' /tmp/routelane-signal.json | grep -q 'lane-c' && say "non-root $TESTUSER received RuntimeChanged: $(grep RuntimeChanged /tmp/routelane-signal.json | head -1 | cut -c1-160)" || fail "no RuntimeChanged delivered to $TESTUSER: $(cat /tmp/routelane-signal.json)"
denied() { # label, command...
    label=$1; shift
    if OUT=$("$@" 2>&1); then fail "$label accepted without authorization"; fi
    echo "$OUT" | grep -q "Access denied" || fail "$label failed for another reason: $OUT"
}
denied "policy change" as_user $BUS call $DEST ActivateRuntime s "$(activation '[]')"
ip -n "$NS_C" link add "rl${TOK}x" type dummy
ip -n "$NS_C" link set "rl${TOK}x" up
denied "lane expansion" as_user $BUS call $DEST ActivateRuntime s "$(activation '["lane-c"]' "rl${TOK}x")"
denied "foreign UID" sudo -u "$OTHERUSER" $BUS call $DEST ActivateRuntime s "$(activation '["lane-c"]')"
denied "foreign UID cleanup" sudo -u "$OTHERUSER" $BUS call $DEST Cleanup
EFFECTIVE=$(as_user $BUS call $DEST Inspect | field runtime.decision.effective.v4)
[ "$EFFECTIVE" = '{"Lane": "lane-c"}' ] || fail "a denied request changed the runtime: $EFFECTIVE"
say "without fresh authorization: policy change, lane expansion and foreign UID $OTHERUSER are denied (org.freedesktop.DBus.Error.AccessDenied); runtime unchanged"

# --- helper crash: systemd restarts it and the journaled runtime resumes -----
kill -9 "$HELPER"
for _ in $(seq 1 50); do NEW=$(systemctl show -p MainPID --value routelane-helper.service); [ "$NEW" != 0 ] && [ "$NEW" != "$HELPER" ] && break; sleep 0.1; done
[ "$NEW" != "$HELPER" ] && [ "$NEW" != 0 ] || fail "helper not restarted"
sleep 1
PHASE=$(as_user $BUS call $DEST Inspect | field runtime.phase)
[ "$PHASE" = '"Armed"' ] || fail "restarted helper did not resume: $PHASE"
say "kill -9 $HELPER -> Restart=on-failure pid $NEW resumed phase Armed"

# --- carrier returns: failback after five seconds, still unauthorized ---------
ip -n "$NS_GA" link set "$GAC" up
for _ in $(seq 1 100); do
    EFFECTIVE=$(as_user $BUS call $DEST Inspect | field runtime.decision.effective.v4)
    [ "$EFFECTIVE" = '{"Lane": "lane-a"}' ] && break; sleep 0.1
done
[ "$EFFECTIVE" = '{"Lane": "lane-a"}' ] || fail "no failback: $EFFECTIVE"
[ "$(probe)" = "peer=10.10.1.2" ] || fail "new flow did not return to the primary"
say "carrier returned: failback to lane-a; new flow source 10.10.1.2"

# --- package upgrade while armed: journal and private config survive --------
if [ -n "$UPGRADE_DEB" ]; then
    as_user sh -c 'mkdir -p ~/.config/routelane && echo "{\"sentinel\":1}" > ~/.config/routelane/config.json'
    CONFIG_SUM=$(as_user sh -c 'sha256sum < ~/.config/routelane/config.json')
    OLD_VERSION=$(dpkg-query -W -f='${Version}' routelane)
    OLD_PID=$(systemctl show -p MainPID --value routelane-helper.service)
    DEBIAN_FRONTEND=noninteractive apt-get install -y "$UPGRADE_DEB" >/tmp/routelane-apt.log 2>&1 || { tail /tmp/routelane-apt.log; fail "package upgrade"; }
    NEW_VERSION=$(dpkg-query -W -f='${Version}' routelane)
    dpkg --compare-versions "$NEW_VERSION" gt "$OLD_VERSION" || fail "not an upgrade: $OLD_VERSION -> $NEW_VERSION"
    NEW_PID=$(systemctl show -p MainPID --value routelane-helper.service)
    [ "$NEW_PID" != "$OLD_PID" ] || fail "postinst did not try-restart the running helper"
    [ -f /run/routelane/applied-plan.json ] || fail "upgrade removed the ownership journal"
    PHASE=$(as_user $BUS call $DEST Inspect | field runtime.phase)
    [ "$PHASE" = '"Armed"' ] || fail "upgraded helper did not resume: $PHASE"
    [ "$(probe)" = "peer=10.10.1.2" ] || fail "primary flow broken after upgrade"
    [ "$(as_user sh -c 'sha256sum < ~/.config/routelane/config.json')" = "$CONFIG_SUM" ] || fail "upgrade changed private config"
    say "upgrade $OLD_VERSION -> $NEW_VERSION while armed: helper $OLD_PID -> $NEW_PID resumed Armed, journal and private config unchanged"
fi

# --- authorized cleanup restores the interface setting -----------------------
allow
as_user $BUS call $DEST Cleanup >/dev/null || fail "authorized cleanup"
[ "$(ip netns exec "$NS_C" cat /proc/sys/net/ipv4/conf/$CA/src_valid_mark)" = 0 ] || fail "src_valid_mark not restored"
ip netns exec "$NS_C" nft list table inet routelane >/dev/null 2>&1 && fail "routelane table left behind"
say "authorized cleanup removed RouteLane state and restored src_valid_mark=0"
echo "RESULT: PASS"
