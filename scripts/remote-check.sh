#!/usr/bin/env bash
# Checks a client machine against a ruro daemon on another machine: reach it,
# deploy a graph, watch it run, stream a port, record on the robot, engage and
# release the e-stop, and stop the graph. Run it on the client (Linux, macOS,
# or Git Bash on Windows) with the daemon's endpoint and the options the
# daemon needs (TLS, token):
#
#   scripts/remote-check.sh tls/robot.local:7447 --tls-ca ca.pem --token-file token
#   RURO=./ruro.exe scripts/remote-check.sh tcp/192.168.1.20:7447
#
# The daemon must have no graph deployed: the check deploys
# examples/pid.yaml (simulated joints only, nothing moves) and stops it at
# the end. It prints PASS or FAIL for each step and exits non-zero if any
# failed.
set -u

if [ $# -lt 1 ] || [ "$1" = -h ] || [ "$1" = --help ]; then
    sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi
endpoint=$1
shift
RURO=${RURO:-ruro}
graph=${GRAPH:-$(cd "$(dirname "$0")/.." && pwd)/examples/pid.yaml}
failed=0
out=
opts=("$@")

r() { "$RURO" --connect "$endpoint" ${opts[@]+"${opts[@]}"} "$@"; }

# step NAME COMMAND...: runs the command and keeps its output in $out.
step() {
    local name=$1
    shift
    if out=$("$@" 2>&1); then
        printf 'PASS  %s\n' "$name"
    else
        printf 'FAIL  %s\n' "$name"
        printf '%s\n' "$out" | sed 's/^/      /'
        failed=1
    fi
}

# expect NAME PATTERN: the last step's output matches PATTERN (grep -E).
expect() {
    if printf '%s\n' "$out" | grep -Eq "$2"; then
        printf 'PASS  %s\n' "$1"
    else
        printf 'FAIL  %s (expected /%s/)\n' "$1" "$2"
        printf '%s\n' "$out" | sed 's/^/      /'
        failed=1
    fi
}

echo "client: $("$RURO" --version) on $(uname -sm 2>/dev/null || echo unknown)"
echo "daemon: $endpoint"
echo

start=$(date +%s)
step "reach the daemon" r status
[ "$failed" = 0 ] || exit 1
expect "it reports its version and target" '^daemon .* v[0-9]+\.[0-9]+\.[0-9]+ \('
printf '%s\n' "$out" | head -1 | sed 's/^/      /'
expect "no graph is deployed yet" 'no graph deployed'
if [ "$failed" != 0 ]; then
    echo
    echo "Stopping here: the daemon must be reachable and idle."
    exit 1
fi

# Round trips: ten status requests.
t0=$(date +%s%N 2>/dev/null)
for _ in 1 2 3 4 5 6 7 8 9 10; do r status >/dev/null 2>&1; done
t1=$(date +%s%N 2>/dev/null)
case "$t0$t1" in
*N*) ;; # no nanoseconds (macOS date)
*) printf 'INFO  a status request takes %d ms on average (connecting included)\n' $(((t1 - t0) / 10000000)) ;;
esac

step "deploy $(basename "$graph")" r deploy "$graph"
expect "both groups deployed" 'deployed 2 group'

running=
for _ in $(seq 1 30); do
    out=$(r status 2>&1)
    if printf '%s\n' "$out" | grep -Eq '^ +arm +running +[1-9]'; then
        running=1
        break
    fi
    sleep 1
done
if [ -n "$running" ]; then
    printf 'PASS  the graph runs (live metrics arrive)\n'
    printf '%s\n' "$out" | grep -E '^(group| +[a-z])' | sed 's/^/      /'
else
    printf 'FAIL  the graph did not report running within 30 s\n'
    printf '%s\n' "$out" | sed 's/^/      /'
    failed=1
fi

# Live values over the network for three seconds.
values=$(mktemp)
r echo arm.position >"$values" 2>&1 &
echo_pid=$!
sleep 3
kill "$echo_pid" 2>/dev/null
wait "$echo_pid" 2>/dev/null
n=$(grep -c . "$values")
if [ "$n" -gt 10 ]; then
    printf 'PASS  stream a port (%d values of arm.position in 3 s)\n' "$n"
else
    printf 'FAIL  stream a port (%d lines of arm.position in 3 s)\n' "$n"
    sed 's/^/      /' "$values"
    failed=1
fi
rm -f "$values"

file=/tmp/ruro-remote-check-$$.mcap
step "start a recording on the daemon's machine" r record "$file"
sleep 2
step "finish it" r record --stop
expect "messages were recorded" 'recorded [1-9][0-9]* message'

step "engage the e-stop" r estop
step "the daemon reports it" r status
expect "status shows the stop" 'EMERGENCY STOP ENGAGED'
step "release it" r estop --release
expect "released" 'e-stop released'

step "stop the graph" r stop
step "the daemon is idle again" r status
expect "no graph left" 'no graph deployed'

echo
echo "took $(($(date +%s) - start)) s; the recording is $file on the daemon's machine"
if [ "$failed" = 0 ]; then
    echo "all steps passed"
else
    echo "some steps FAILED"
fi
exit "$failed"
