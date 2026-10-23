#!/usr/bin/env bash
# Builds a Debian package containing ruro, ruro-host and ruro-studio.
#
# ruro-host-py (the host for Python nodes) is left out on purpose: it links
# the libpython of the build machine, which would tie the package to one
# Python minor version, and it finds the `ruro` Python package in its source
# tree unless the package is installed. Build it from source where Python
# nodes are needed.
#
#   scripts/package-deb.sh              # release build + package
#   scripts/package-deb.sh --no-build   # package existing target/release binaries
#
# Output: target/package/ruro_<version>_<arch>.deb
#
# Environment:
#   CARGO              cargo executable (default: cargo)
#   CARGO_TARGET_DIR   cargo target directory (default: <repo>/target)
#   DEB_MAINTAINER     Maintainer field (default: first workspace author)
set -euo pipefail

usage() {
    sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
}

build=1
while (($# > 0)); do
    case "$1" in
        --no-build) build=0 ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
    shift
done

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

cargo="${CARGO:-cargo}"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
release_dir="$target_dir/release"
out_dir="$target_dir/package"

# [workspace.package] values from the root manifest.
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
if [[ -z "$version" ]]; then
    echo "could not read [workspace.package] version from Cargo.toml" >&2
    exit 1
fi
authors="$(workspace_value authors)"
default_maintainer="$(sed -E 's/^\[ *"([^"]*)".*/\1/' <<<"$authors")"
maintainer="${DEB_MAINTAINER:-$default_maintainer}"
homepage="$(workspace_value repository)"
arch="$(dpkg --print-architecture)"

bins=(ruro ruro-host ruro-studio)

if ((build)); then
    "$cargo" build --release --locked -p ruro-cli -p ruro-host -p ruro-studio
fi
for bin in "${bins[@]}"; do
    if [[ ! -x "$release_dir/$bin" ]]; then
        echo "missing $release_dir/$bin (build first or drop --no-build)" >&2
        exit 1
    fi
done

pkg="ruro_${version}_${arch}"
stage="$out_dir/deb/$pkg"
rm -rf "$stage"
mkdir -p "$stage/DEBIAN" "$out_dir"
chmod 755 "$stage" "$stage/DEBIAN"

# Binaries, without their symbol tables (as Debian policy asks; it roughly
# halves their size).
for bin in "${bins[@]}"; do
    install -Dm755 "$release_dir/$bin" "$stage/usr/bin/$bin"
    if command -v strip >/dev/null; then
        strip --strip-unneeded "$stage/usr/bin/$bin"
    fi
done

# Desktop integration. The desktop file, icons and metadata are all named
# after the studio's app id, which is also its window's app id and class.
app_id=io.github.mhussainahmad.ruro.Studio
install -Dm644 "packaging/linux/$app_id.desktop" \
    "$stage/usr/share/applications/$app_id.desktop"
install -Dm644 "packaging/linux/$app_id.metainfo.xml" \
    "$stage/usr/share/metainfo/$app_id.metainfo.xml"
install -Dm644 "packaging/icons/$app_id.svg" \
    "$stage/usr/share/icons/hicolor/scalable/apps/$app_id.svg"
for png in packaging/icons/hicolor/*/apps/"$app_id.png"; do
    size="$(basename "$(dirname "$(dirname "$png")")")"
    install -Dm644 "$png" "$stage/usr/share/icons/hicolor/$size/apps/$app_id.png"
done

# systemd units (installed disabled).
install -Dm644 packaging/systemd/ruro-daemon.service \
    "$stage/lib/systemd/system/ruro-daemon.service"
install -Dm644 packaging/systemd/user/ruro-daemon.service \
    "$stage/usr/lib/systemd/user/ruro-daemon.service"
# Drop-in that enables `ruro update`, for the administrator to install.
install -Dm644 packaging/systemd/ruro-daemon-updates.conf \
    "$stage/usr/share/ruro/systemd/ruro-daemon-updates.conf"

# Documentation and examples.
doc="$stage/usr/share/doc/ruro"
install -Dm644 README.md "$doc/README.md"
# Example graphs with the files they reference (robot model, Python nodes).
# They are data the studio opens (File > Open Example), so they live in
# /usr/share/ruro rather than under /usr/share/doc, which minimal systems may
# leave out; the documentation directory links to them.
examples="$stage/usr/share/ruro/examples"
install -d "$examples"
while IFS= read -r -d '' f; do
    install -Dm644 "examples/$f" "$examples/$f"
done < <(cd examples && find . -type f ! -name '.*' ! -path '*/__pycache__/*' -printf '%P\0')
ln -s ../../ruro/examples "$doc/examples"
# The C and C++ node headers, for `#include <ruro/ruro.h>` without -I.
for f in sdk/include/ruro/*; do
    install -Dm644 "$f" "$stage/usr/include/ruro/$(basename "$f")"
done
# Helpers for nodes in other languages; examples/process finds them at ../../sdk.
while IFS= read -r -d '' f; do
    install -Dm644 "sdk/$f" "$stage/usr/share/ruro/sdk/$f"
done < <(cd sdk && find . -type f ! -name '.*' -printf '%P\0')
if [[ -f CHANGELOG.md ]]; then
    gzip -9n <CHANGELOG.md >"$doc/changelog.gz"
    chmod 644 "$doc/changelog.gz"
fi

assets=crates/ruro-studio/assets
{
    cat <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: ruro
Upstream-Contact: $maintainer
Source: $homepage

Files: *
Copyright: $(date +%Y) $default_maintainer
License: MIT or Apache-2.0
 ruro is dual-licensed under the MIT license or the Apache License,
 Version 2.0, at your option. The MIT license text follows. The Apache
 License 2.0 is available in /usr/share/common-licenses/Apache-2.0.
 .
EOF
    sed -e 's/^$/./' -e 's/^/ /' LICENSE-MIT
    cat <<'EOF'

Files: usr/bin/ruro-studio
Comment: The studio binary embeds the Inter and JetBrains Mono typefaces
 (SIL Open Font License 1.1) and the Lucide icon font (ISC license).
License: OFL-1.1 and ISC

License: OFL-1.1
EOF
    for f in "$assets/fonts/LICENSE-Inter.txt" "$assets/fonts/LICENSE-JetBrainsMono.txt"; do
        echo " ---- $(basename "$f") ----"
        sed -e 's/[[:space:]]*$//' -e 's/^$/./' -e 's/^/ /' "$f"
        echo " ."
    done
    echo
    echo "License: ISC"
    sed -e 's/[[:space:]]*$//' -e 's/^$/./' -e 's/^/ /' "$assets/icons/LICENSE-lucide.txt"
} >"$doc/copyright"
chmod 644 "$doc/copyright"

# Shared library dependencies. dpkg-shlibdeps finds what the binaries link
# directly; the studio loads its windowing and GL libraries at run time with
# dlopen, so those are listed explicitly.
runtime_deps="libegl1, libgl1, libwayland-client0, libwayland-egl1, libxkbcommon0, libxkbcommon-x11-0, libx11-6, libx11-xcb1, libxcursor1, libxi6, libxrandr2"
shlib_deps=""
if command -v dpkg-shlibdeps >/dev/null; then
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
    mkdir -p "$work/debian"
    printf 'Source: ruro\n\nPackage: ruro\nArchitecture: any\n' >"$work/debian/control"
    abs_bins=()
    for bin in "${bins[@]}"; do
        abs_bins+=("$stage/usr/bin/$bin")
    done
    if shlib_out="$(cd "$work" && dpkg-shlibdeps -O "${abs_bins[@]}" 2>/dev/null)"; then
        shlib_deps="$(sed -n 's/^shlibs:Depends=//p' <<<"$shlib_out")"
    fi
fi
if [[ -z "$shlib_deps" ]]; then
    shlib_deps="libc6 (>= 2.34), libgcc-s1"
fi

installed_size="$(du -sk --exclude=DEBIAN "$stage" | cut -f1)"

cat >"$stage/DEBIAN/control" <<EOF
Package: ruro
Version: $version
Architecture: $arch
Maintainer: $maintainer
Installed-Size: $installed_size
Depends: $shlib_deps, $runtime_deps
Recommends: adduser
Section: devel
Priority: optional
Homepage: $homepage
Description: fast, lightweight robotics dataflow with a native editor
 ruro runs a robot's software as a graph of small nodes (drivers, perception,
 control) with zero-copy messaging between nodes in the same process and
 shared-memory transport between processes.
 .
 This package contains the ruro command-line tool and daemon, the standard
 node host (ruro-host) and the ruro studio desktop editor, plus systemd units
 for running the daemon as a system or user service.
EOF

cat >"$stage/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = "configure" ]; then
    if ! getent passwd ruro >/dev/null; then
        if command -v adduser >/dev/null; then
            adduser --system --group --home /var/lib/ruro --no-create-home \
                --gecos "ruro daemon" ruro >/dev/null
        else
            useradd --system --user-group --home-dir /var/lib/ruro \
                --shell /usr/sbin/nologin ruro
        fi
    fi
    if [ -d /run/systemd/system ]; then
        systemctl daemon-reload >/dev/null || true
    fi
fi
exit 0
EOF

cat >"$stage/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e
if [ -d /run/systemd/system ] && [ "$1" = "remove" ]; then
    systemctl stop ruro-daemon.service >/dev/null 2>&1 || true
fi
exit 0
EOF

cat >"$stage/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null || true
fi
exit 0
EOF
chmod 755 "$stage/DEBIAN/postinst" "$stage/DEBIAN/prerm" "$stage/DEBIAN/postrm"

deb="$out_dir/$pkg.deb"
dpkg-deb --build --root-owner-group -Zxz "$stage" "$deb" >/dev/null
rm -rf "$out_dir/deb"
echo "$deb"
