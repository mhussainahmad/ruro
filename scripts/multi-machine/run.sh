#!/bin/bash
# Runs one graph across two containers, "robot" and "laptop", on their own
# podman network, with TLS and a token, and checks it under 50 ms delay and
# 2% loss (tc netem), a 20 s network partition and daemon restarts. Prints
# PASS/FAIL/SKIP per check and exits non-zero if any check failed.
#
#   cargo build --release -p ruro-cli -p ruro-host
#   scripts/multi-machine/run.sh
#
# The binaries must run on Ubuntu 24.04 (glibc 2.39 or older at build time).
# Environment:
#   PODMAN     command to run podman (default podman), e.g. a wrapper that
#              adds storage options
#   RURO_BIN   directory with ruro and ruro-host (default target/release)
#   APT_OPTS   extra apt options for the image build; rootless podman without
#              subuid ranges needs "-o APT::Sandbox::User=root"
#   MM_SUBNET  network to create (default 10.89.77.0/24)
#   KEEP=1     leave the containers and network running at the end
#
# The containers need --cap-add NET_ADMIN for tc and ip link; netem checks
# are skipped when the kernel has no sch_netem. netem shapes only the robot's
# egress: the delay applies to robot -> laptop traffic.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
podman=${PODMAN:-podman}
bin=${RURO_BIN:-$root/target/release}
image=ruro-mm
net=ruro-mm
subnet=${MM_SUBNET:-10.89.77.0/24}
prefix=${subnet%.*}
work=$(mktemp -d)
failed=0

pass() { printf 'PASS  %s\n' "$*"; }
fail() { printf 'FAIL  %s\n' "$*"; failed=$((failed + 1)); }
skip() { printf 'SKIP  %s\n' "$*"; }
info() { printf '      %s\n' "$*" >&2; }
check() { # check "description" command...: PASS if the command succeeds.
    local what=$1; shift
    if "$@" >/dev/null; then pass "$what"; else fail "$what"; fi
}
now_ms() { echo $(($(date +%s%N) / 1000000)); }

cleanup() {
    if [ "${KEEP:-0}" != 1 ]; then
        $podman rm -f -t 0 robot laptop >/dev/null 2>&1
        $podman network rm -f "$net" >/dev/null 2>&1
    fi
    rm -rf "$work"
}
trap cleanup EXIT INT TERM

# Clients: lr talks to the laptop's daemon (on its loopback listener), rr to
# the robot's over TLS.
lr() { $podman exec -e RURO_TOKEN_FILE=/pki/token -e RURO_DAEMON=laptop laptop ruro "$@"; }
rr() {
    $podman exec -e RURO_TOKEN_FILE=/pki/token -e RURO_TLS_CA=/pki/ca.pem \
        -e RURO_CONNECT=tls/robot:7447 -e RURO_DAEMON=robot robot ruro "$@"
}
robot_daemon() {
    $podman exec -d robot >/dev/null bash -c "RURO_LOG=info,zenoh=warn exec ruro daemon --name robot \
        --listen tls/0.0.0.0:7447 --tls-cert /pki/robot.pem --tls-key /pki/robot-key.pem \
        --token-file /pki/token >>/tmp/daemon.log 2>&1"
}
laptop_daemon() {
    $podman exec -d laptop >/dev/null bash -c "RURO_LOG=info,zenoh=warn exec ruro daemon --name laptop \
        --peer tls/robot:7447 --tls-ca /pki/ca.pem --token-file /pki/token \
        >>/tmp/daemon.log 2>&1"
}
# Rate (HZ column) of a node in a status table.
hz() { awk -v n="$2" '$1 == n && $2 == "running" { print int($3) }' <<<"$1"; }
# Waits up to $1 s for `cmd` to succeed; sets WAITED to the seconds it took.
WAITED=0
wait_for() {
    local limit=$1 t0; shift
    t0=$(now_ms)
    while :; do
        if "$@" >/dev/null 2>&1; then
            WAITED=$((($(now_ms) - t0) / 1000)); return 0
        fi
        [ $(($(now_ms) - t0)) -ge $((limit * 1000)) ] && return 1
        sleep 0.5
    done
}
flowing() { # both cross-machine wires deliver at more than 50 Hz
    local a b
    a=$(hz "$(lr status 2>/dev/null)" double); b=$(hz "$(rr status 2>/dev/null)" back)
    [ "${a:-0}" -gt 50 ] && [ "${b:-0}" -gt 50 ]
}
both_estopped() { lr status | grep -q "^EMERGENCY STOP" && rr status | grep -q "^EMERGENCY STOP"; }
none_estopped() { ! lr status | grep -q "^EMERGENCY STOP" && ! rr status | grep -q "^EMERGENCY STOP"; }
# Runs a command, prints its duration, fails if it errs or takes over $1 s.
timed() {
    local limit=$1 t0 rc out; shift
    t0=$(now_ms); out=$("$@" 2>&1); rc=$?
    local ms=$(($(now_ms) - t0))
    info "$* -> rc=$rc in ${ms} ms: $(head -1 <<<"$out")"
    [ $rc -eq 0 ] && [ $ms -le $((limit * 1000)) ]
}
# Message count per channel of a recording on the laptop: "channel count".
counts() { lr info "$1" | awk 'NF == 5 && $1 ~ /\./ { print $1, $4, $5 }'; }

echo "== setup"
for b in ruro ruro-host; do
    [ -x "$bin/$b" ] || { echo "missing $bin/$b; build with cargo build --release -p ruro-cli -p ruro-host"; exit 2; }
    cp "$bin/$b" "$work/"
done
glibc=$(objdump -T "$work/ruro" "$work/ruro-host" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)
info "binaries need ${glibc:-unknown glibc} (Ubuntu 24.04 has GLIBC_2.39)"
$podman build -q -t "$image" ${APT_OPTS:+--build-arg "APT_OPTS=$APT_OPTS"} \
    -f "$here/Containerfile" "$work" >/dev/null || { echo "image build failed"; exit 2; }
rm -f "$work/ruro" "$work/ruro-host"

# A CA, certificates for both machines (server and client use) and a token.
mkdir -p "$work/pki"
$podman run --rm --init -v "$work/pki:/pki:z" "$image" bash -ec '
    cd /pki
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 2 \
        -subj /CN=ruro-test-ca -keyout ca-key.pem -out ca.pem 2>/dev/null
    for m in robot laptop; do
        openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj /CN=$m \
            -keyout $m-key.pem -out $m.csr 2>/dev/null
        printf "subjectAltName=DNS:$m,IP:127.0.0.1\nextendedKeyUsage=serverAuth,clientAuth\n" >$m.ext
        openssl x509 -req -in $m.csr -CA ca.pem -CAkey ca-key.pem -CAcreateserial -days 2 \
            -extfile $m.ext -out $m.pem 2>/dev/null
    done
    head -c 24 /dev/urandom | od -An -tx1 | tr -d " \n" >token
    chmod 600 token ./*-key.pem'

$podman rm -f -t 0 robot laptop >/dev/null 2>&1
$podman network rm -f "$net" >/dev/null 2>&1
$podman network create --subnet "$subnet" "$net" >/dev/null || exit 2
for m in robot:10 laptop:20; do
    $podman run -d --init --name "${m%:*}" --hostname "${m%:*}" --network "$net" \
        --ip "$prefix.${m#*:}" --cap-add NET_ADMIN --shm-size 256m \
        -v "$work/pki:/pki:ro,z" "$image" sleep infinity >/dev/null || exit 2
done
cat >"$work/span.yaml" <<'EOF'
# robot: sine -> laptop: gain x2 -> robot: gain x0.5
name: span
groups:
  sense: { machine: robot }
  act: { machine: robot }
  plan: { machine: laptop }
nodes:
  wave: { type: std/sine, group: sense, rate: 100, params: { frequency: 0.5 } }
  double: { type: std/gain, group: plan, params: { k: 2.0 } }
  back: { type: std/gain, group: act, params: { k: 0.5 } }
# Wires between machines get a queue: a lossy link delivers in bursts.
wires:
  - { from: wave.value, to: double.input, depth: 32 }
  - { from: double.output, to: back.input, depth: 32 }
EOF
$podman cp "$work/span.yaml" laptop:/tmp/span.yaml

echo "== daemons with TLS and a token"
robot_daemon; sleep 1; laptop_daemon
check "laptop daemon peers with the robot over TLS" \
    wait_for 15 bash -c "$(declare -f lr); podman=$podman; lr fleet | grep -q '^robot '"
check "a client with a wrong token is refused" \
    bash -c "! $podman exec laptop ruro --connect tls/robot:7447 --tls-ca /pki/ca.pem \
        --token wrong --daemon robot status >/dev/null 2>&1"
check "a client without the CA is refused" \
    bash -c "! $podman exec laptop ruro --connect tls/robot:7447 --token-file /pki/token \
        --daemon robot status >/dev/null 2>&1"
picked=$(for _ in 1 2 3 4 5 6; do
    $podman exec -e RURO_TOKEN_FILE=/pki/token -e RURO_TLS_CA=/pki/ca.pem robot \
        ruro --connect tls/robot:7447 status 2>/dev/null | awk 'NR == 1 { print $2 }'
done | sort -u | tr '\n' ' ')
check "without --daemon, a client of the robot's endpoint talks to the robot (got: $picked)" \
    test "$picked" = "robot "

echo "== graph across both machines"
check "ruro check accepts the graph" lr check /tmp/span.yaml
check "deploy from the laptop places groups on both machines" \
    bash -c "$(declare -f lr); podman=$podman; lr deploy /tmp/span.yaml | grep -q 'on robot'"
if wait_for 15 flowing; then pass "data flows robot -> laptop -> robot (after ${WAITED} s)"; else fail "data flows robot -> laptop -> robot"; fi
info "laptop: $(lr status | grep -E '^  double')"
info "robot:  $(rr status | grep -E '^  back')"
check "ruro echo on the laptop shows a node running on the robot" \
    bash -c "$podman exec -e RURO_TOKEN_FILE=/pki/token -e RURO_DAEMON=laptop laptop \
        timeout 3 ruro echo back.output | grep -q '^value='"

echo "== emergency stop"
for side in rr lr; do
    $side estop >/dev/null
    check "e-stop engaged on the $([ $side = rr ] && echo robot || echo laptop) stops both machines" \
        wait_for 5 both_estopped
    if [ $side = rr ]; then
        check "robot status does not tell the user to release a robot-engaged stop at the laptop" \
            bash -c "! { $(declare -f rr); podman=$podman; rr status; } | grep -q 'daemon laptop estop --release'"
    fi
    $side estop --release >/dev/null
    check "release where it was engaged releases both" wait_for 5 none_estopped
done

if $podman exec robot tc qdisc add dev eth0 root netem delay 50ms loss 2% 2>"$work/tc.err"; then
    echo "== 50 ms delay and 2% loss on the robot's egress"
    sleep 5
    if wait_for 15 flowing; then pass "data still flows under netem"; else fail "data still flows under netem"; fi
    status=$(lr status)
    age=$(awk '$1 == "double" { print $6 }' <<<"$status")
    info "laptop: $(grep -E '^  double' <<<"$status")   (AGE: mean send-to-receive)"
    check "robot -> laptop wire shows the added delay (AGE $age >= 50ms)" \
        bash -c "[[ '$age' == *ms ]] && awk 'BEGIN { exit !(${age%ms} >= 50) }'"
    check "ruro param from the laptop to a robot node" timed 10 lr param back k 0.5
    check "ruro param from the robot to a laptop node" timed 10 rr param double k 2
    check "ruro estop from the laptop" timed 10 lr estop
    check "e-stop reaches the robot under netem" wait_for 5 both_estopped
    check "ruro estop --release from the laptop" timed 10 lr estop --release
    check "release reaches the robot under netem" wait_for 5 none_estopped
    check "ruro record from the laptop" timed 10 lr record /tmp/netem.mcap
    sleep 10
    check "ruro record --stop collects the robot's part" timed 10 lr record --stop
    while read -r ch n rate; do info "recorded $ch: $n messages, $rate Hz"; done < <(counts /tmp/netem.mcap)
    dbl=$(counts /tmp/netem.mcap | awk '$1 == "double.output" { print int($3) }')
    check "laptop receives >= 90 Hz of the robot's 100 Hz under netem (got ${dbl:-?})" \
        test "${dbl:-0}" -ge 90
    $podman exec robot tc qdisc del dev eth0 root
else
    skip "netem unavailable ($(head -1 "$work/tc.err")): no delay/loss checks"
fi

echo "== 20 s partition (100% loss on the robot)"
part() {
    if $podman exec robot tc qdisc replace dev eth0 root netem loss 100% 2>/dev/null; then
        echo netem
    else
        $podman exec robot ip link set dev eth0 down && echo link
    fi
}
heal() {
    if [ "$1" = netem ]; then $podman exec robot tc qdisc del dev eth0 root
    else $podman exec robot ip link set dev eth0 up; fi
}
wait_for 10 flowing
how=$(part); t0=$(now_ms)
sleep 3
check "laptop status answers during the partition" timed 10 lr status
check "robot status answers during the partition" timed 10 rr status
check "laptop status reports the robot as not answering" \
    wait_for 10 bash -c "{ $(declare -f lr); podman=$podman; lr status; } | grep -q 'not answering: .robot'"
out=$(lr estop 2>&1); rc=$?
info "ruro estop on the laptop: rc=$rc: $out"
if [ $rc -ne 0 ] && grep -q robot <<<"$out" && ! grep -q 'daemon error: \[' <<<"$out"; then
    pass "e-stop during the partition fails with a readable error naming the robot"
else
    fail "e-stop during the partition fails with a readable error naming the robot"
fi
out=$(lr param back k 0.5 2>&1); rc=$?
check "ruro param to the robot fails quickly during the partition ($out)" test $rc -ne 0
sleep $((20 - ($(now_ms) - t0) / 1000))
heal "$how"
if wait_for 30 flowing; then pass "graph recovers by itself after healing (${WAITED} s)"; else fail "graph recovers by itself after healing"; fi
check "the e-stop engaged during the partition reaches the robot after healing" \
    wait_for 10 both_estopped
lr estop --release >/dev/null 2>&1; rr estop --release --all >/dev/null 2>&1
check "both released" wait_for 5 none_estopped

echo "== robot daemon restart"
$podman exec robot pkill -TERM -f '^ruro daemon'
sleep 2
check "graceful daemon stop ends its hosts" \
    bash -c "! $podman exec robot pgrep -f ruro-host >/dev/null"
robot_daemon
check "restarted robot daemon rejoins the fleet" \
    wait_for 15 bash -c "$(declare -f lr); podman=$podman; lr fleet | grep -q '^robot '"
sleep 3
info "robot after restart: $(rr status | sed -n 2p)"
check "data flows again by itself after the robot daemon restarted" wait_for 20 flowing
lr deploy /tmp/span.yaml >/dev/null
if wait_for 15 flowing; then pass "redeploying from the laptop restores the graph (${WAITED} s)"; else fail "redeploying from the laptop restores the graph"; fi

$podman exec robot pkill -KILL -f '^ruro daemon'
sleep 3
check "hosts exit when the robot daemon is killed (SIGKILL)" \
    bash -c "! $podman exec robot pgrep -f ruro-host >/dev/null"
robot_daemon
check "the killed robot daemon restarts and rejoins" \
    wait_for 15 bash -c "$(declare -f lr); podman=$podman; lr fleet | grep -q '^robot '"
check "the graph comes back by itself after SIGKILL" wait_for 20 flowing
check "ruro stop stops both machines" \
    bash -c "$(declare -f lr rr); podman=$podman; lr stop >/dev/null && sleep 1 && \
        lr fleet | awk '\$1 == \"robot\" || \$1 == \"laptop\" { if (\$3 != \"-\") exit 1 }'"

for m in robot laptop; do
    if $podman exec $m grep -q 'panicked' /tmp/daemon.log; then
        fail "$m daemon log has no panics"; $podman exec $m grep 'panicked' /tmp/daemon.log
    else
        pass "$m daemon log has no panics"
    fi
done

echo
if [ $failed -eq 0 ]; then echo "all checks passed"; else echo "$failed check(s) failed"; fi
[ $failed -eq 0 ]
