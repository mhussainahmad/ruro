#!/usr/bin/env bash
# Checks installed ruro and ruro-python packages: starts a daemon with the
# Python host, deploys the Python example graph from /usr/share/ruro, and
# waits for its Python nodes to run without errors.
#
#   sudo apt-get install ./ruro_*.deb ./ruro-python_*.deb
#   scripts/test-python-deb.sh
set -euo pipefail

examples=/usr/share/ruro/examples
port=${PORT:-7599}
ep="tcp/127.0.0.1:$port"
log=$(mktemp)
RURO_PY_MODULES="$examples/python/threshold.py" \
    ruro daemon --name pydeb --listen "$ep" --host /usr/bin/ruro-host-py >"$log" 2>&1 &
daemon=$!
trap 'kill $daemon 2>/dev/null; wait $daemon 2>/dev/null; rm -f "$log"' EXIT

for _ in $(seq 1 50); do
    ruro --connect "$ep" status >/dev/null 2>&1 && break
    sleep 0.2
done
ruro --connect "$ep" nodes | grep -E '^py/(threshold|brightness)' || {
    echo "the Python node types are not listed"; cat "$log"; exit 1
}
ruro --connect "$ep" deploy "$examples/python-threshold.yaml"
for _ in $(seq 1 60); do
    out=$(ruro --connect "$ep" status)
    if grep -Eq '^ +threshold +running +[1-9]' <<<"$out" && grep -Eq '^ +brightness +running +[1-9]' <<<"$out"; then
        echo "$out"
        # No errors counted on the Python nodes (the ERRORS column).
        if grep -E '^ +(threshold|brightness) ' <<<"$out" | awk '{ if ($(NF-1) != 0) exit 1 }'; then
            ruro --connect "$ep" stop
            echo "the Python nodes run"
            exit 0
        fi
        break
    fi
    sleep 0.5
done
echo "the Python nodes did not run cleanly"
ruro --connect "$ep" status || true
cat "$log"
exit 1
