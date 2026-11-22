#!/bin/bash
# Installs what the AppImage build needs on AlmaLinux 9 (as root): compilers and build tools,
# the development headers of the system libraries that stay on the user's machine
# (glibc, X11/Wayland, EGL/GL, audio, VA-API/DRM, fonts), and Rust via rustup.
# Used by the Containerfile and by the CI job (which runs in an almalinux:9 container).
#
# AlmaLinux 9 is the oldest target (glibc 2.34): the AppImage built here runs on it and on
# newer distributions (AlmaLinux 10, Ubuntu 22.04+, Debian 12+, Fedora).
set -euo pipefail

dnf install -y dnf-plugins-core
dnf config-manager --set-enabled crb
dnf install -y --setopt=install_weak_deps=False \
  gcc gcc-c++ make cmake ninja-build nasm pkgconf-pkg-config git python3 python3-pip \
  xz bzip2 tar file findutils diffutils which patch \
  zlib-devel lcms2-devel \
  freetype-devel fribidi-devel harfbuzz-devel fontconfig-devel \
  libX11-devel libXext-devel libXrandr-devel libXinerama-devel libXScrnSaver-devel \
  libXcursor-devel libXi-devel libxkbcommon-devel libxkbcommon-x11-devel \
  wayland-devel wayland-protocols-devel \
  mesa-libGL-devel mesa-libEGL-devel libdrm-devel libva-devel \
  alsa-lib-devel pulseaudio-libs-devel pipewire-devel \
  desktop-file-utils fuse-libs
dnf clean all

# mpv and libplacebo need meson >= 1.3; AlmaLinux 9 ships 0.63
python3 -m pip install --no-cache-dir 'meson>=1.3'
meson --version

# Rust (official installer), stable, minimal profile, into /usr/local
export RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  | sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable
chmod -R a+rwX /usr/local/rustup /usr/local/cargo
/usr/local/cargo/bin/rustc --version
