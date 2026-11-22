# Keepcut

Mark the parts of your clips to keep, then cut and join them **losslessly**.

Keepcut is a desktop app for trimming and joining recordings (screen captures, camera and phone footage) without re-encoding them. You mark the parts to keep on a timeline, and Keepcut copies them into one file (or one per clip or segment). Only the few frames at a cut that doesn't land on a keyframe are re-encoded ("smart cut"), so exports are fast and keep the original quality.

- Frame-accurate preview (mpv), keyframe navigation, and a preview of the joined result
- Joined or separate outputs; clips with different formats are re-encoded to match
- Chapters, plus a ready-to-paste YouTube chapter list
- Loudness normalization (whole output, per clip, or per segment)
- Projects with autosave and crash recovery; relinking of moved files
- Every export is verified (duration, frame count, audio/video sync)

## Install

Downloads are on the [Releases](https://github.com/borisburger/keepcut/releases) page.

### Windows 10/11 (x64)

- **Installer** `Keepcut-<version>-windows-x64-setup.exe`: installs for the current user (no admin rights needed); adds a Start menu entry and opens `.keepcut` projects.
- **Portable** `Keepcut-<version>-windows-x64.zip`: unzip anywhere and run `keepcut.exe`.

The builds aren't code-signed yet, so Windows SmartScreen may warn about an unknown publisher: click **More info → Run anyway**.

ffmpeg and mpv are included. Hardware encoders (NVIDIA NVENC, Intel QSV, AMD AMF) are used when the GPU and driver support them.

### Linux (Flatpak)

Download `Keepcut-<version>-linux-x86_64.flatpak`. You need Flatpak and the Flathub remote (for the runtime), once:

| Distribution | Install Flatpak |
|---|---|
| Ubuntu, Kubuntu, Debian | `sudo apt install flatpak` |
| Fedora | already installed |
| AlmaLinux / Rocky / RHEL 9+ | `sudo dnf install flatpak` |

Then:

```sh
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user ./Keepcut-<version>-linux-x86_64.flatpak
flatpak run io.github.borisburger.Keepcut
```

After a fresh Flatpak install, log out and back in once so that Keepcut shows up in the application menu. The Flatpak has access to your files (clips can be anywhere, and outputs are written next to them) and to the GPU, including NVENC/NVDEC on NVIDIA and VA-API on Intel/AMD.

### macOS (best effort)

`Keepcut-<version>-macos-arm64.zip` is built automatically but hardly tested. It isn't notarized: the first time, right-click `Keepcut.app` → **Open**, or allow it in System Settings → Privacy & Security.

## Build from source

Needs Rust (stable, via [rustup](https://rustup.rs)) and ffmpeg 7 or newer at run time.

**Windows** (MSVC toolchain):

```powershell
powershell -ExecutionPolicy Bypass -File packaging/windows/fetch-mpv.ps1      # libmpv into third_party/
powershell -ExecutionPolicy Bypass -File packaging/windows/fetch-ffmpeg.ps1   # ffmpeg into third_party/
cargo run --release -p keepcut
```

`packaging/windows/package.ps1` builds the zip and the installer ([Inno Setup 7](https://jrsoftware.org/isinfo.php)) in `dist/`.

**Linux**: install libmpv and the windowing libraries, then `cargo run --release -p keepcut`.

- Ubuntu/Debian: `sudo apt install build-essential pkg-config libmpv-dev ffmpeg libgl1-mesa-dev libegl1-mesa-dev libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev`
- Fedora: `sudo dnf install gcc pkgconf-pkg-config mpv-libs-devel libxkbcommon-devel wayland-devel libX11-devel libXcursor-devel libXrandr-devel libXi-devel mesa-libGL-devel mesa-libEGL-devel`, and ffmpeg from RPM Fusion (Fedora's `ffmpeg-free` lacks H.264/HEVC).

`packaging/linux/build-flatpak.sh` builds the Flatpak bundle (needs `flatpak-builder`).

**macOS**: `brew install mpv ffmpeg`, then `LIBRARY_PATH="$(brew --prefix)/lib" cargo run --release -p keepcut`. `packaging/macos/package.sh` builds the app bundle.

`keepcut-cli` is a command-line front end to the same export pipeline (probing, encoder detection, exports from a JSON project description; `cargo run -p keepcut-cli -- --help`).

## License

Keepcut is free software under the [GNU General Public License v3.0 or later](LICENSE).

The packages bundle [mpv](https://mpv.io) (libmpv, LGPL-2.1-or-later) and [FFmpeg](https://ffmpeg.org) (GPL-3.0 builds); their licenses and source locations are included in the packages.
