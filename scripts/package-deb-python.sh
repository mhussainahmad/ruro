#!/usr/bin/env bash
# Builds the Debian package ruro-python: ruro-host-py, the host for Python
# nodes, and the `ruro` Python package it imports nodes with.
#
# ruro-host-py links the libpython of the build machine, so the package is
# for the distribution release it is built on (Ubuntu 24.04: Python 3.12); it
# needs Python 3.11 or newer, and the package `ruro` of the same version.
#
#   scripts/package-deb-python.sh              # release build + package
#   scripts/package-deb-python.sh --no-build   # package an existing build
#
# Output: target/package/ruro-python_<version>_<arch>.deb
#
# Environment:
#   CARGO              cargo executable (default: cargo)
#   CARGO_TARGET_DIR   cargo target directory (default: <repo>/target)
#   PYO3_PYTHON        the Python to build against (default: python3)
#   DEB_MAINTAINER     Maintainer field (default: first workspace author)
set -euo pipefail

build=1
while (($# > 0)); do
    case "$1" in
        --no-build) build=0 ;;
        -h | --help)
            sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
    shift
done

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
cargo="${CARGO:-cargo}"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
out_dir="$target_dir/package"
python="${PYO3_PYTHON:-python3}"

workspace_value() {
    awk -v key="$1" '
        /^\[/ { section = $0 }
        section == "[workspace.package]" && $1 == key {
            sub(/^[^=]*=[ \t]*/, "")
            gsub(/^"|"$/, "")
            print
            exit
        }' Cargo.toml
}
version="$(workspace_value version)"
authors="$(workspace_value authors)"
default_maintainer="$(sed -E 's/^\[ *"([^"]*)".*/\1/' <<<"$authors")"
maintainer="${DEB_MAINTAINER:-$default_maintainer}"
homepage="$(workspace_value repository)"
arch="$(dpkg --print-architecture)"

py_version="$("$python" -c 'import sys; print(f"{sys.version_info[0]}.{sys.version_info[1]}")')"
if ! "$python" -c 'import sys; sys.exit(sys.version_info < (3, 11))'; then
    echo "$python is Python $py_version; ruro's Python nodes need 3.11 or newer" >&2
    exit 1
fi

if ((build)); then
    PYO3_PYTHON="$python" "$cargo" build --release --locked -p ruro-host-py
fi
bin="$target_dir/release/ruro-host-py"
[[ -x "$bin" ]] || { echo "missing $bin" >&2; exit 1; }

pkg="ruro-python_${version}_${arch}"
stage="$out_dir/deb/$pkg"
rm -rf "$stage"
mkdir -p "$stage/DEBIAN" "$out_dir"
chmod 755 "$stage" "$stage/DEBIAN"

install -Dm755 "$bin" "$stage/usr/bin/ruro-host-py"
command -v strip >/dev/null && strip --strip-unneeded "$stage/usr/bin/ruro-host-py"
# The pure-Python part of the package; the host provides ruro._native itself.
# ruro-host-py looks for it in ../lib/ruro/python next to its executable.
pydir="$stage/usr/lib/ruro/python/ruro"
for f in crates/ruro-py/python/ruro/*.py crates/ruro-py/python/ruro/py.typed crates/ruro-py/python/ruro/_native.pyi; do
    install -Dm644 "$f" "$pydir/$(basename "$f")"
done
doc="$stage/usr/share/doc/ruro-python"
install -Dm644 crates/ruro-py/README.md "$doc/README.md"
{
    echo "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/"
    echo "Upstream-Name: ruro"
    echo "Source: $homepage"
    echo
    echo "Files: *"
    echo "Copyright: $(date +%Y) $default_maintainer"
    echo "License: MIT or Apache-2.0"
    echo " See /usr/share/doc/ruro/copyright."
} >"$doc/copyright"

shlib_deps=""
if command -v dpkg-shlibdeps >/dev/null; then
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
    mkdir -p "$work/debian"
    printf 'Source: ruro\n\nPackage: ruro-python\nArchitecture: any\n' >"$work/debian/control"
    if out="$(cd "$work" && dpkg-shlibdeps -O "$stage/usr/bin/ruro-host-py" 2>/dev/null)"; then
        shlib_deps="$(sed -n 's/^shlibs:Depends=//p' <<<"$out")"
    fi
fi
[[ -n "$shlib_deps" ]] || shlib_deps="libc6 (>= 2.34), libpython$py_version"

installed_size="$(du -sk --exclude=DEBIAN "$stage" | cut -f1)"
cat >"$stage/DEBIAN/control" <<EOF
Package: ruro-python
Version: $version
Architecture: $arch
Maintainer: $maintainer
Installed-Size: $installed_size
Depends: ruro (= $version), $shlib_deps, libpython$py_version-stdlib
Recommends: python3-numpy
Section: devel
Priority: optional
Homepage: $homepage
Description: Python nodes for the ruro robotics dataflow runtime
 ruro-host-py runs ruro nodes written in Python (Python $py_version) next to
 the standard Rust nodes, with the same ports, parameters, metrics and studio
 support. Point the daemon at it with --host /usr/bin/ruro-host-py, and name
 the node modules in RURO_PY_MODULES.
EOF

deb="$out_dir/$pkg.deb"
dpkg-deb --build --root-owner-group -Zxz "$stage" "$deb" >/dev/null
rm -rf "$out_dir/deb"
echo "$deb"
