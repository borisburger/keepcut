#!/bin/bash
# Builds the Keepcut AppImage. Runs inside the AlmaLinux 9 build environment
# (packaging/linux/appimage/build.sh starts it in a container; CI runs it in almalinux:9).
#
# Builds ffmpeg (GPL: x264, x265, dav1d, SVT-AV1, NVENC/NVDEC, VA-API), libplacebo, libass and libmpv into
# $work/prefix (each once; kept between runs when $work is a volume), then Keepcut, then bundles
# everything with linuxdeploy. Libraries that must come from the user's system (glibc,
# OpenGL/EGL/Mesa, NVIDIA driver, X11/Wayland, audio, fontconfig/freetype) are not bundled;
# libstdc++/libgcc_s are left out too, so a newer system Mesa that needs a newer libstdc++
# still loads. libxkbcommon(-x11) is added explicitly: winit loads it with dlopen, so
# linuxdeploy can't see it, and not every system has the -x11 part.
# Output: dist/Keepcut-<version>-x86_64.AppImage
set -euo pipefail

src=$(cd "$(dirname "$0")/../../.." && pwd)
work=${KEEPCUT_APPIMAGE_WORK:-$HOME/.cache/keepcut-appimage}
prefix=$work/prefix
dl=$work/downloads
bld=$work/build
mkdir -p "$prefix" "$dl" "$bld" "$src/dist"
jobs=$(nproc)

export PKG_CONFIG_PATH=$prefix/lib/pkgconfig:$prefix/lib64/pkgconfig
export LD_LIBRARY_PATH=$prefix/lib
export LIBRARY_PATH=$prefix/lib
export PATH=$prefix/bin:$PATH
export CARGO_HOME=$work/cargo
export CARGO_TARGET_DIR=$work/target

version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' "$src/Cargo.toml")

# fetch URL SHA256 -> path of the verified download
fetch() {
  local url=$1 sha=$2 file
  file="$dl/$(basename "$url")"
  if [ ! -f "$file" ]; then
    curl -sSfL -o "$file.part" "$url"
    mv "$file.part" "$file"
  fi
  if ! echo "$sha  $file" | sha256sum -c --quiet -; then
    rm -f "$file"
    echo "SHA-256 mismatch: $url" >&2
    exit 1
  fi
  echo "$file"
}

# done NAME-VERSION: true if that component is already built into $prefix
done_() { [ -f "$prefix/.done-$1" ]; }
mark() { touch "$prefix/.done-$1"; }

# git_at URL REF DIR [COMMIT]: shallow clone of a tag/branch, checked against COMMIT
git_at() {
  rm -rf "$3"
  git -c advice.detachedHead=false clone -q --depth 50 --branch "$2" --recurse-submodules --shallow-submodules "$1" "$3"
  if [ -n "${4:-}" ]; then
    git -C "$3" checkout -q "$4"
  fi
}

step() { echo; echo "=== $*"; }

# --- third-party libraries (same versions as the Flatpak manifest) ---------------------------

if ! done_ nv-codec-headers-n13.0.19.0; then
  step nv-codec-headers
  git_at https://github.com/FFmpeg/nv-codec-headers.git n13.0.19.0 "$bld/nv-codec-headers" e844e5b26f46bb77479f063029595293aa8f812d
  make -C "$bld/nv-codec-headers" PREFIX="$prefix" install
  mark nv-codec-headers-n13.0.19.0
fi

if ! done_ x264-b35605ac; then
  step x264
  git_at https://code.videolan.org/videolan/x264.git stable "$bld/x264" b35605ace3ddf7c1a5d67a2eb553f034aef41d55
  (cd "$bld/x264" && ./configure --prefix="$prefix" --enable-shared --enable-pic --disable-cli && make -j"$jobs" && make install)
  mark x264-b35605ac
fi

if ! done_ x265-4.2; then
  step x265
  f=$(fetch https://download.videolan.org/videolan/x265/x265_4.2.tar.gz 40b1ea0453e0309f0eba934e0ddf533f8f6295966679e8894e8f1c1c8d5e1210)
  rm -rf "$bld/x265" && mkdir -p "$bld/x265" && tar -xzf "$f" -C "$bld/x265" --strip-components=1
  cmake -S "$bld/x265/source" -B "$bld/x265/out" -G Ninja -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$prefix" -DCMAKE_INSTALL_LIBDIR=lib -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
    -DENABLE_CLI=OFF -DENABLE_SHARED=ON
  ninja -C "$bld/x265/out" install
  rm -f "$prefix"/lib/libx265.a
  mark x265-4.2
fi

if ! done_ dav1d-1.5.5; then
  step dav1d
  f=$(fetch https://downloads.videolan.org/pub/videolan/dav1d/1.5.5/dav1d-1.5.5.tar.xz ca96b1d0ba3f01f55de7231d775892b64994ba061aaeeaaa4f662a390b3abf9e)
  rm -rf "$bld/dav1d" && mkdir -p "$bld/dav1d" && tar -xJf "$f" -C "$bld/dav1d" --strip-components=1
  meson setup "$bld/dav1d/out" "$bld/dav1d" --prefix="$prefix" --libdir=lib --buildtype=release \
    -Denable_tools=false -Denable_tests=false
  ninja -C "$bld/dav1d/out" install
  mark dav1d-1.5.5
fi

# SVT-AV1: the AV1 encoder for smart cuts of AV1 clips; NASM (deps-el9.sh) builds its assembly.
if ! done_ svt-av1-v4.3.0; then
  step svt-av1
  git_at https://gitlab.com/AOMediaCodec/SVT-AV1.git v4.3.0 "$bld/svt-av1" e3d63405f9ba8219913a7276c2c47d10e5d022fe
  cmake -S "$bld/svt-av1" -B "$bld/svt-av1/out" -G Ninja -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$prefix" -DCMAKE_INSTALL_LIBDIR=lib \
    -DBUILD_SHARED_LIBS=ON -DBUILD_APPS=OFF -DBUILD_TESTING=OFF
  ninja -C "$bld/svt-av1/out" install
  mark svt-av1-v4.3.0
fi

# The marker names the encoders, so adding one rebuilds ffmpeg in a kept work volume.
if ! done_ ffmpeg-9.1-svtav1; then
  step ffmpeg
  f=$(fetch https://ffmpeg.org/releases/ffmpeg-9.1.tar.xz 1a4d394ae63d190a526cd052eb4415c156b5c5e2df37efab41a7de0b8fde2433)
  rm -rf "$bld/ffmpeg" && mkdir -p "$bld/ffmpeg" && tar -xJf "$f" -C "$bld/ffmpeg" --strip-components=1
  (cd "$bld/ffmpeg" && ./configure --prefix="$prefix" \
    --enable-gpl --enable-version3 --enable-shared --disable-static --disable-doc --disable-ffplay \
    --enable-libx264 --enable-libx265 --enable-libdav1d --enable-libsvtav1 \
    --enable-vaapi --enable-libdrm --enable-ffnvcodec --enable-nvenc --enable-nvdec \
    && make -j"$jobs" && make install)
  mark ffmpeg-9.1-svtav1
fi

if ! done_ libplacebo-v7.387.0; then
  step libplacebo
  git_at https://github.com/haasn/libplacebo.git v7.387.0 "$bld/libplacebo" 275f7dea27924cdd34ab2e6c20e448b6bc4fab65
  meson setup "$bld/libplacebo/out" "$bld/libplacebo" --prefix="$prefix" --libdir=lib --buildtype=release \
    -Dopengl=enabled -Dvulkan=disabled -Dshaderc=disabled -Dglslang=disabled -Dlcms=enabled \
    -Ddemos=false -Dtests=false
  ninja -C "$bld/libplacebo/out" install
  mark libplacebo-v7.387.0
fi

if ! done_ libass-0.17.5; then
  step libass
  f=$(fetch https://github.com/libass/libass/releases/download/0.17.5/libass-0.17.5.tar.xz 2dca25c0e0c837ddf00b52011b3f82cac1e4ddd3ad018227806b0c2288864acc)
  rm -rf "$bld/libass" && mkdir -p "$bld/libass" && tar -xJf "$f" -C "$bld/libass" --strip-components=1
  (cd "$bld/libass" && ./configure --prefix="$prefix" --disable-static --disable-libunibreak && make -j"$jobs" && make install)
  mark libass-0.17.5
fi

# PipeWire output off: libpipewire isn't bundled (it must match the system's daemon) and a
# system without it would fail to load libmpv; PulseAudio/ALSA output works on PipeWire too.
if ! done_ mpv-v0.42.0-nopw; then
  step mpv
  git_at https://github.com/mpv-player/mpv.git v0.42.0 "$bld/mpv" b0841e8c4d156b8403dc1876c4cbd9011eb583f3
  meson setup "$bld/mpv/out" "$bld/mpv" --prefix="$prefix" --libdir=lib --buildtype=release \
    -Dlibmpv=true -Dcplayer=false -Dlua=disabled -Djavascript=disabled -Dvulkan=disabled \
    -Dmanpage-build=disabled -Dhtml-build=disabled -Dpdf-build=disabled \
    -Dcdda=disabled -Ddvdnav=disabled -Dlibbluray=disabled -Dlibarchive=disabled -Dpipewire=disabled
  ninja -C "$bld/mpv/out" install
  mark mpv-v0.42.0-nopw
fi

# --- Keepcut ----------------------------------------------------------------------------------

step "keepcut $version"
(cd "$src" && cargo build --release --locked -p keepcut -p keepcut-cli)

# --- AppDir and AppImage ----------------------------------------------------------------------

step AppImage
app=io.github.borisburger.Keepcut
appdir=$work/AppDir
rm -rf "$appdir"
install -Dm755 "$CARGO_TARGET_DIR/release/keepcut" "$CARGO_TARGET_DIR/release/keepcut-cli" -t "$appdir/usr/bin/"
install -Dm755 "$prefix/bin/ffmpeg" "$prefix/bin/ffprobe" -t "$appdir/usr/bin/"
install -Dm644 "$src/packaging/linux/$app.desktop" -t "$appdir/usr/share/applications/"
install -Dm644 "$src/packaging/linux/$app.metainfo.xml" -t "$appdir/usr/share/metainfo/"
install -Dm644 "$src/packaging/icons/keepcut.svg" "$appdir/usr/share/icons/hicolor/scalable/apps/$app.svg"
for s in 16 32 48 64 128 256; do
  install -Dm644 "$src/packaging/icons/keepcut-$s.png" "$appdir/usr/share/icons/hicolor/${s}x${s}/apps/$app.png"
done
install -Dm644 "$src/LICENSE" "$appdir/usr/share/licenses/keepcut/LICENSE"
install -Dm644 "$bld/ffmpeg/COPYING.GPLv3" "$appdir/usr/share/licenses/ffmpeg/COPYING.GPLv3"
install -Dm644 "$bld/svt-av1/LICENSE.md" "$bld/svt-av1/PATENTS.md" -t "$appdir/usr/share/licenses/svt-av1/"
install -Dm644 "$bld/mpv/LICENSE.LGPL" "$appdir/usr/share/licenses/mpv/LICENSE.LGPL"

# Smart cuts of AV1 clips need a software AV1 encoder (LD_LIBRARY_PATH finds $prefix/lib).
# Captured first: with pipefail, grep -q exiting early would kill ffmpeg with SIGPIPE.
encoders=$("$appdir/usr/bin/ffmpeg" -hide_banner -encoders)
if ! grep -q libsvtav1 <<<"$encoders"; then
  echo "the bundled ffmpeg has no libsvtav1 encoder" >&2
  exit 1
fi

linuxdeploy=$(fetch https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d)
chmod +x "$linuxdeploy"

out="Keepcut-$version-x86_64.AppImage"
rm -f "$work/$out"
# Containers usually have no FUSE: run the linuxdeploy AppImage extracted.
(cd "$work" && APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 LDAI_OUTPUT="$out" LDAI_NO_APPSTREAM=1 \
  "$linuxdeploy" --appdir "$appdir" \
    --executable "$appdir/usr/bin/keepcut" \
    --executable "$appdir/usr/bin/keepcut-cli" \
    --executable "$appdir/usr/bin/ffmpeg" \
    --executable "$appdir/usr/bin/ffprobe" \
    --library /usr/lib64/libxkbcommon.so.0 \
    --library /usr/lib64/libxkbcommon-x11.so.0 \
    --desktop-file "$appdir/usr/share/applications/$app.desktop" \
    --icon-file "$appdir/usr/share/icons/hicolor/256x256/apps/$app.png" \
    --exclude-library 'libstdc++.so*' --exclude-library 'libgcc_s.so*' \
    --output appimage)
mv -f "$work/$out" "$src/dist/$out"
echo "appimage: dist/$out ($(du -h "$src/dist/$out" | cut -f1))"
