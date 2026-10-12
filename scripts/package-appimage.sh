#!/usr/bin/env bash
# Builds an AppImage of ruro studio. The image also carries the `ruro` CLI and
# `ruro-host`, which the studio uses to start its local daemon.
#
#   scripts/package-appimage.sh                 # release build + AppImage
#   scripts/package-appimage.sh --no-build      # use existing target/release binaries
#   scripts/package-appimage.sh --fetch-tool    # download appimagetool if missing
#   scripts/package-appimage.sh --appdir-only   # stop after assembling the AppDir
#
# Output: target/package/ruro-studio-<version>-<arch>.AppImage
#
# The binaries link only against glibc (the windowing and GL libraries are
# loaded at run time from the system), so build on the oldest distribution you
# want to support.
#
# Environment:
#   CARGO              cargo executable (default: cargo)
#   CARGO_TARGET_DIR   cargo target directory (default: <repo>/target)
#   APPIMAGETOOL       path to appimagetool (default: looked up in PATH)
#   APPIMAGETOOL_URL   download location used with --fetch-tool
#
# Running the result:
#   ./ruro-studio-<version>-<arch>.AppImage [graph.yaml]
#   ./ruro-studio-<version>-<arch>.AppImage ruro status    # run a bundled tool
set -euo pipefail

usage() {
    sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'
}

build=1
fetch=0
appdir_only=0
while (($# > 0)); do
    case "$1" in
        --no-build) build=0 ;;
        --fetch-tool) fetch=1 ;;
        --appdir-only) appdir_only=1 ;;
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
arch="$(uname -m)"

version="$(awk '
    /^\[/ { section = $0 }
    section == "[workspace.package]" && $1 == "version" {
        sub(/^[^=]*=[ \t]*/, ""); gsub(/"/, ""); print; exit
    }' Cargo.toml)"
if [[ -z "$version" ]]; then
    echo "could not read [workspace.package] version from Cargo.toml" >&2
    exit 1
fi

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

appdir="$out_dir/ruro-studio.AppDir"
rm -rf "$appdir"
mkdir -p "$appdir" "$out_dir"

for bin in "${bins[@]}"; do
    install -Dm755 "$release_dir/$bin" "$appdir/usr/bin/$bin"
    if command -v strip >/dev/null; then
        strip --strip-unneeded "$appdir/usr/bin/$bin"
    fi
done
app_id=io.github.mhussainahmad.ruro.Studio
install -Dm644 "packaging/linux/$app_id.desktop" "$appdir/usr/share/applications/$app_id.desktop"
install -Dm644 "packaging/icons/$app_id.svg" "$appdir/usr/share/icons/hicolor/scalable/apps/$app_id.svg"
for png in packaging/icons/hicolor/*/apps/"$app_id.png"; do
    size="$(basename "$(dirname "$(dirname "$png")")")"
    install -Dm644 "$png" "$appdir/usr/share/icons/hicolor/$size/apps/$app_id.png"
done
# The studio offers examples/ found above its executable in File > Open Example.
install -d "$appdir/examples"
while IFS= read -r -d '' f; do
    install -Dm644 "examples/$f" "$appdir/examples/$f"
done < <(cd examples && find . -type f ! -name '.*' ! -path '*/__pycache__/*' -printf '%P\0')
install -Dm644 LICENSE-MIT "$appdir/usr/share/doc/ruro/LICENSE-MIT"
install -Dm644 LICENSE-APACHE "$appdir/usr/share/doc/ruro/LICENSE-APACHE"
for f in crates/ruro-studio/assets/fonts/LICENSE-*.txt crates/ruro-studio/assets/icons/LICENSE-*.txt; do
    install -m644 "$f" "$appdir/usr/share/doc/ruro/"
done

# Top-level entries required by the AppImage specification.
cp "packaging/linux/$app_id.desktop" "$appdir/$app_id.desktop"
cp "packaging/icons/hicolor/256x256/apps/$app_id.png" "$appdir/$app_id.png"
ln -s "$app_id.png" "$appdir/.DirIcon"

cat >"$appdir/AppRun" <<'EOF'
#!/bin/sh
# Starts ruro studio, or a bundled tool when its name is the first argument
# (or the name the AppImage was invoked as, for symlinks named ruro/ruro-host).
here="$(dirname "$(readlink -f "$0")")"
export PATH="$here/usr/bin:$PATH"
export RURO_HOST="${RURO_HOST:-$here/usr/bin/ruro-host}"
tool="$(basename "${ARGV0:-$0}")"
case "$tool" in
    ruro | ruro-host | ruro-studio) exec "$here/usr/bin/$tool" "$@" ;;
esac
case "${1:-}" in
    ruro | ruro-host | ruro-studio)
        tool="$1"
        shift
        exec "$here/usr/bin/$tool" "$@"
        ;;
esac
exec "$here/usr/bin/ruro-studio" "$@"
EOF
chmod 755 "$appdir/AppRun"

if ((appdir_only)); then
    echo "$appdir"
    exit 0
fi

tool="${APPIMAGETOOL:-}"
if [[ -z "$tool" ]] && command -v appimagetool >/dev/null; then
    tool="$(command -v appimagetool)"
fi
if [[ -z "$tool" ]]; then
    if ((!fetch)); then
        echo "appimagetool not found: install it, set APPIMAGETOOL, or pass --fetch-tool" >&2
        echo "the AppDir is ready at $appdir" >&2
        exit 1
    fi
    url="${APPIMAGETOOL_URL:-https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$arch.AppImage}"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    echo "downloading $url" >&2
    curl -fsSL --retry 3 -o "$tmp/appimagetool" "$url"
    chmod 755 "$tmp/appimagetool"
    tool="$tmp/appimagetool"
fi

image="$out_dir/ruro-studio-$version-$arch.AppImage"
# Extract-and-run lets appimagetool (itself an AppImage) work without FUSE,
# e.g. in containers and CI.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$arch" "$tool" --no-appstream "$appdir" "$image"
rm -rf "$appdir"
echo "$image"
