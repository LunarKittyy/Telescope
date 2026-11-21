#!/usr/bin/env bash
# Copies libgphoto2 into the Linux package so DSLRs work without installing anything: DEST/lib/gphoto2 gets libgphoto2, libgphoto2_port,
# the ptp2 camera driver (Nikon, Canon and Sony DSLRs all speak PTP), the usb1 port driver and whatever else they link to that a desktop may lack.
# The program loads it at run time and falls back to the system's (webcam_capture/gp.rs).
# Usage: bundle_gphoto2.sh DEST [LIBXML2_LIB]
# LIBXML2_LIB is a folder with a libxml2.so.2 (build_libxml2.sh) to bundle in place of the system's, which links ICU and would bring its 30 MB along.
# Reads the build machine's own copies, so it needs libgphoto2 and its camera drivers installed there (Ubuntu: libgphoto2-6, libgphoto2-port12).
set -euo pipefail

dest=${1:?the package folder}/lib/gphoto2
xml2=${2:-}
if [ -n "$xml2" ]; then
    [ -f "$xml2/libxml2.so.2" ] || { echo "$xml2/libxml2.so.2 not found" >&2; exit 1; }
    # So that ldd below finds this one first, and what it links to (no ICU) is all that gets copied
    export LD_LIBRARY_PATH=$(realpath "$xml2")${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
fi
cache=$(ldconfig -p)
# The first x86-64 entry (not exiting early: that would break the pipe of ldconfig)
find_lib() {
    awk -v l="$1" '$1 == l && /x86-64/ && !done { print $NF; done = 1 }' <<< "$cache"
}
core=$(find_lib libgphoto2.so.6)
port=$(find_lib libgphoto2_port.so.12)
[ -n "$core" ] && [ -n "$port" ] || { echo "libgphoto2 isn't installed here" >&2; exit 1; }
libdir=$(dirname "$(realpath "$core")")
# The driver folders are versioned by the library (libgphoto2/2.5.33, libgphoto2_port/0.12.2)
ptp2=$(find "$libdir/libgphoto2" -name ptp2.so | sort | tail -n 1)
usb1=$(find "$libdir/libgphoto2_port" -name usb1.so | sort | tail -n 1)
[ -f "$ptp2" ] && [ -f "$usb1" ] || { echo "ptp2.so or usb1.so not found under $libdir" >&2; exit 1; }

rm -rf "$dest"
mkdir -p "$dest/camlibs" "$dest/iolibs"
cp -L "$ptp2" "$dest/camlibs/ptp2.so"
cp -L "$usb1" "$dest/iolibs/usb1.so"
# Real files under each library's soname. What every Linux has stays out (the C library and its parts, the compiler runtime, udev)
for file in "$core" "$port" "$dest/camlibs/ptp2.so" "$dest/iolibs/usb1.so"; do
    ldd "$file" | awk '/=> \// { print $1, $3 }' | while read -r soname path; do
        case $soname in
            libc.so.*|libm.so.*|libdl.so.*|libpthread.so.*|librt.so.*|libresolv.so.*|libgcc_s.so.*|libstdc++.so.*|libudev.so.*) continue ;;
        esac
        [ -e "$dest/$soname" ] || cp -L "$path" "$dest/$soname"
    done
done
for lib in libgphoto2.so.6 libgphoto2_port.so.12; do
    [ -e "$dest/$lib" ] || cp -L "$(find_lib "$lib")" "$dest/$lib"
done
# What libxml2 would have needed ICU for isn't used here, and a copy slipping back in is 30 MB
if ls "$dest" | grep -q '^libicu'; then
    echo "ICU is in the bundle: $(ls "$dest" | grep '^libicu' | tr '\n' ' ')(pass the folder build_libxml2.sh wrote)" >&2
    exit 1
fi
# The minimal libxml2 has fewer symbols than the system's: ptp2 must find every one it calls
if [ -n "$xml2" ] && missing=$(ldd -r "$dest/camlibs/ptp2.so" 2>&1 | grep 'undefined symbol: xml'); then
    echo "The bundled libxml2 lacks what ptp2 needs: $missing" >&2
    exit 1
fi
echo "libgphoto2 is in $dest: $(ls "$dest" | tr '\n' ' ')"
