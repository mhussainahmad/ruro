#!/usr/bin/env bash
# Runs the criterion benchmarks and compares them against a saved baseline.
#
#   scripts/bench.sh save [NAME]      run the benchmarks and save them as baseline NAME
#   scripts/bench.sh compare [NAME]   run them again and fail if any got slower than
#                                     NAME by more than BENCH_THRESHOLD percent
#
# NAME defaults to `main`. In CI: save on the target branch, compare on the
# pull request (keep target/criterion between the two runs, e.g. in a cache).
#
# Environment:
#   BENCH_THRESHOLD  allowed slowdown in percent (default 10). A benchmark only
#                    counts as regressed when the lower bound of criterion's
#                    confidence interval for the change exceeds it, so noise
#                    alone does not fail the run.
#   BENCH_PACKAGES   crates to benchmark (default "ruro-node ruro-transport")
#   BENCH_ARGS       extra arguments for the benchmark harness
#                    (e.g. "--measurement-time 3" for a quicker run)
set -euo pipefail

cd "$(dirname "$0")/.."

mode=${1:-}
name=${2:-main}
threshold=${BENCH_THRESHOLD:-10}
packages=${BENCH_PACKAGES:-ruro-node ruro-transport}
read -r -a extra <<<"${BENCH_ARGS:-}"

pkg_args=()
for p in $packages; do
    pkg_args+=(-p "$p")
done

criterion_dir=${CARGO_TARGET_DIR:-target}/criterion

case "$mode" in
save)
    cargo bench "${pkg_args[@]}" -- --save-baseline "$name" "${extra[@]}"
    echo "saved baseline \`$name\` in $criterion_dir"
    ;;
compare)
    if ! compgen -G "$criterion_dir/*/*/$name" >/dev/null &&
        ! compgen -G "$criterion_dir/*/$name" >/dev/null; then
        echo "no baseline \`$name\` in $criterion_dir; run \`$0 save $name\` first" >&2
        exit 2
    fi
    # Comparison results of earlier runs must not be mistaken for this one's.
    find "$criterion_dir" -type d -name change -prune -exec rm -rf {} +
    cargo bench "${pkg_args[@]}" -- --baseline "$name" "${extra[@]}"
    python3 - "$criterion_dir" "$threshold" <<'EOF'
import json, pathlib, sys

root, threshold = pathlib.Path(sys.argv[1]), float(sys.argv[2])
rows, regressed = [], []
for path in sorted(root.glob("**/change/estimates.json")):
    bench = str(path.parent.parent.relative_to(root))
    mean = json.loads(path.read_text())["mean"]
    change = mean["point_estimate"] * 100
    low = mean["confidence_interval"]["lower_bound"] * 100
    high = mean["confidence_interval"]["upper_bound"] * 100
    bad = low > threshold
    rows.append((bench, change, low, high, bad))
    if bad:
        regressed.append(bench)
if not rows:
    sys.exit("no comparison results found; did the benchmarks run?")
width = max(len(r[0]) for r in rows)
print(f"\n{'BENCHMARK':{width}}  {'CHANGE':>8}  {'95% CI':>19}")
for bench, change, low, high, bad in rows:
    flag = "  REGRESSED" if bad else ""
    print(f"{bench:{width}}  {change:+7.1f}%  [{low:+7.1f}%, {high:+7.1f}%]{flag}")
if regressed:
    sys.exit(f"\n{len(regressed)} benchmark(s) slower than the baseline by more than {threshold}%")
print(f"\nno benchmark slower than the baseline by more than {threshold}%")
EOF
    ;;
*)
    awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
    exit 2
    ;;
esac
