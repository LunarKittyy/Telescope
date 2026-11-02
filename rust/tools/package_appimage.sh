#!/usr/bin/env bash
# Turns the Linux package folder into Telescope-x86_64.AppImage: the static type 2 runtime, which needs no libfuse2, in front of a squashfs of the folder.
# Usage: package_appimage.sh BUNDLE OUT
# BUNDLE comes from package_linux.sh, with Telescope.apk in it for a release. Needs squashfs-tools 4.5 or newer and works on any x86-64 Linux, since everything in it was built before.
set -euo pipefail

RUNTIME_RELEASE=20251108
RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

tools=$(cd "$(dirname "$0")" && pwd)
bundle=$(realpath "${1:?the package folder}")
out=$(realpath -m "${2:?the AppImage to write}")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

runtime=${APPIMAGE_RUNTIME:-$work/runtime}
[ -f "$runtime" ] || curl -fsSL --connect-timeout 30 --retry 5 --retry-all-errors --retry-delay 10 -o "$runtime" "https://github.com/AppImage/type2-runtime/releases/download/$RUNTIME_RELEASE/runtime-x86_64"
echo "$RUNTIME_SHA256  $runtime" | sha256sum -c --quiet -

# Its own message when it can't mount suggests --appimage-extract, which only unpacks. Same length, so nothing else in the file moves
python3 - "$runtime" "$work/runtime-told" <<'EOF'
import sys
data = open(sys.argv[1], "rb").read()
for old, new in ((b"You might still be able to extract the contents of this AppImage ", b"To start Telescope without FUSE, run this file with this option:"),
                 (b"if you run it with the --appimage-extract option. ", b"--appimage-extract-and-run")):
    assert data.count(old) == 1, old
    data = data.replace(old, new.ljust(len(old)))
open(sys.argv[2], "wb").write(data)
EOF

app=$work/Telescope.AppDir
cp -r "$bundle" "$app"
# The tarball's start.sh and what only the Python updater looks at
rm -rf "$app/start.sh" "$app/main.py" "$app/telescope"
chmod 755 "$app/telescope-rs"
cp "$tools/AppRun" "$app/AppRun"
chmod 755 "$app/AppRun"
ln -s telescope.png "$app/.DirIcon"
cat > "$app/telescope.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Telescope
GenericName=Phone webcam
Comment=Phone camera as a webcam
Exec=AppRun
Icon=telescope
StartupWMClass=telescope
Terminal=false
Categories=AudioVideo;Video;
Keywords=webcam;camera;phone;android;
EOF

mksquashfs "$app" "$work/image" -root-owned -noappend -no-progress -quiet -comp zstd -Xcompression-level 19 -b 1M
cat "$work/runtime-told" "$work/image" > "$out"
chmod 755 "$out"
echo "$out is $(($(stat -c %s "$out") / 1024 / 1024)) MB"
