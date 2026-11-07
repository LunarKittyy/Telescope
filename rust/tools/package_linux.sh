#!/usr/bin/env bash
# Builds the Linux package folder, the shape Telescope-linux.tar.gz has: what the Python app's updater and start.sh expect.
# Usage: package_linux.sh --ffmpeg DIR [--out DIR] [--channel stable|nightly|dev] [--build N] [--commit SHA]
# package_appimage.sh makes the AppImage from the same folder.
# DIR comes from build_ffmpeg.sh. What it's built on sets the oldest Linux it runs on, so a release builds on Ubuntu 22.04 (rust-package.yml).
# release.yml adds Telescope.apk afterwards and packs this folder into the tarball.
set -euo pipefail

rust=$(cd "$(dirname "$0")/.." && pwd)
out=$rust/target/dist-linux
channel=dev
build=0
commit=
ffmpeg=
while [ $# -gt 0 ]; do
    case $1 in
        --ffmpeg) ffmpeg=$(realpath "$2"); shift 2 ;;
        --out) out=$(realpath -m "$2"); shift 2 ;;
        --channel) channel=$2; shift 2 ;;
        --build) build=$2; shift 2 ;;
        --commit) commit=$2; shift 2 ;;
        *) echo "Unknown option $1" >&2; exit 2 ;;
    esac
done
[ -n "$ffmpeg" ] || { echo "Pass --ffmpeg, a build from build_ffmpeg.sh" >&2; exit 2; }
case $channel in stable|nightly|dev) ;; *) echo "The channel is stable, nightly or dev" >&2; exit 2 ;; esac

cd "$rust"
FFMPEG_DIR=$ffmpeg TELESCOPE_BUILD=$build TELESCOPE_CHANNEL=$channel TELESCOPE_COMMIT=$commit cargo build --release --locked -p telescope-app
target=${CARGO_TARGET_DIR:-target}

rm -rf "$out"
mkdir -p "$out/lib/fallback" "$out/telescope"
cp "$target/release/telescope-rs" "$out/"
cp tools/start.sh "$out/start.sh"
cp tools/main_stub.py "$out/main.py"
chmod +x "$out/start.sh" "$out/telescope-rs"
# The window's .slint files and icons are read at run time (telescope_ui_kit::library_path)
cp -r crates/ui-kit/ui "$out/ui-kit"
# The Python updater wants a telescope folder, and check_release.py reads the stamp here
version=$(tr -d '[:space:]' < ../VERSION)
printf "VERSION = '%s'\nBUILD = %s\nCHANNEL = '%s'\nCOMMIT = '%s'\n" "$version" "$build" "$channel" "$commit" > "$out/telescope/_build.py"
# Real files under each library's soname: build artifacts don't keep symlinks
for lib in libavcodec libavutil libswresample libswscale; do
    soname=$(basename "$(find "$ffmpeg/lib" -name "$lib.so.*" -regex '.*\.so\.[0-9]+')")
    cp -L "$ffmpeg/lib/$soname" "$out/lib/$soname"
done
# Only reached on a system without its own (start.sh)
for lib in libva.so.2 libva-drm.so.2 libdrm.so.2; do
    cp -L "$(ldconfig -p | awk -v l="$lib" '$1 == l && /x86-64/ { print $NF; exit }')" "$out/lib/fallback/$lib"
done
# The same for fontconfig, in a folder of its own: libva's being wanted mustn't put these ahead of a system's own
mkdir "$out/lib/fallback-fonts"
for lib in libfontconfig.so.1 libfreetype.so.6 libexpat.so.1 libpng16.so.16 libbrotlidec.so.1 libbrotlicommon.so.1 libuuid.so.1 libz.so.1; do
    cp -L "$(ldconfig -p | awk -v l="$lib" '$1 == l && /x86-64/ { print $NF; exit }')" "$out/lib/fallback-fonts/$lib"
done
cp tools/fonts.conf "$out/lib/fallback-fonts/fonts.conf"
# DSLR support (libgphoto2 with the PTP camera driver and what it links to), found by the program in lib/gphoto2 before the system's own
tools/bundle_gphoto2.sh "$out"
"$out/telescope-rs" app-icon "$out/telescope.png"
python3 tools/fetch_rnnoise.py linux "$out"
python3 tools/write_notices.py linux "$ffmpeg" "$out/THIRD_PARTY_NOTICES.txt"
echo "Telescope $version $channel $build is in $out"
