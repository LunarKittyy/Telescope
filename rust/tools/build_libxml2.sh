#!/usr/bin/env bash
# Builds libxml2 without ICU for the Linux package's bundled libgphoto2 (bundle_gphoto2.sh --libxml2). Ubuntu's own libxml2 links ICU, and
# that drags libicuuc and libicudata (about 30 MB unpacked, 10 MB packed) into the download, for a library that reads a camera's small XML.
# Same soname (libxml2.so.2) and symbols as the 2.9 that libgphoto2 was built against, so it's a drop-in. Not libxml2 2.14: that is libxml2.so.16.
# Usage: build_libxml2.sh PREFIX     (writes PREFIX/lib/libxml2.so.2; needs a C compiler, make, curl, xz-utils and zlib1g-dev)
set -euo pipefail

version=2.13.9
sha256=a2c9ae7b770da34860050c309f903221c67830c86e4a7e760692b803df95143a
prefix=$(realpath -m "${1:?the folder to install into}")

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -sSfL --retry 3 -o "$work/libxml2.tar.xz" "https://download.gnome.org/sources/libxml2/${version%.*}/libxml2-$version.tar.xz"
echo "$sha256  $work/libxml2.tar.xz" | sha256sum -c --quiet -
tar -xJf "$work/libxml2.tar.xz" -C "$work"
cd "$work/libxml2-$version"
./configure --prefix="$prefix" --disable-static --without-icu --without-python --without-lzma > "$work/configure.log" 2>&1 || { tail -n 30 "$work/configure.log" >&2; exit 1; }
make -j"$(nproc)" > "$work/make.log" 2>&1 || { tail -n 30 "$work/make.log" >&2; exit 1; }
make install > /dev/null
strip --strip-unneeded "$prefix/lib/libxml2.so.$version"
[ -f "$prefix/lib/libxml2.so.2" ] || { echo "libxml2.so.2 wasn't built" >&2; exit 1; }
echo "libxml2 $version without ICU is in $prefix/lib"
