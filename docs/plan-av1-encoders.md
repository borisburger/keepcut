# Plan: software AV1 encoding in every package

Keepcut has to re-encode part of an AV1 clip in two cases: the short part from a cut
to the next keyframe (smart cut), and a whole clip that doesn't match the output's
format. The cut parts always use a **software** AV1 encoder (§6.3). Whole clips use a
hardware encoder when one works (except screen recordings below 4K), else software too. Today not every package's ffmpeg has
a software AV1 encoder. This plan fixes that and makes SVT-AV1 the preferred encoder.

Not affected: H.264 and HEVC (nearly all footage), and full re-encodes, which offer
only H.264 and HEVC as output codecs.

## Status (2026-11-23)

Implemented, not yet released:

- Steps 1 and 4: the Flatpak and AppImage build SVT-AV1 4.3.0 (the version Homebrew
  pairs with ffmpeg 9) and ship its licence. Windows bundles BtbN's "win64-gpl-shared"
  9.0.2 build (month-end release `autobuild-2026-10-31-12-59`, pinned by SHA-256).
  Neither Linux package has been built yet (CI does it on the next release tag).
- Step 2: calibrated on two Bandicam AV1 4K recordings. SSIM must be measured on NUT
  (or MP4) outputs: MKV's millisecond timestamps pair some frames wrongly (a lossless MKV
  copy scored 0.9963, the NUT copy 1.0000). Results:

  | Encoder | SSIM | Size vs source | fps |
  |---|---|---|---|
  | libaom crf 28 | 0.9991 / 0.9991 | 0.40 / 0.71 | 3 |
  | SVT preset 6 crf 28 | 0.9994 / 0.9991 | 0.81 / 0.88 | 19–26 |
  | SVT preset 6 crf 32 | 0.9993 / 0.9988 | 0.68 / 0.74 | 20–25 |
  | SVT preset 8 crf 32 | 0.9991 / 0.9986 | 0.85 / 0.93 | 27–44 |
  | SVT preset 10 crf 32 | 0.9987 / 0.9979 | 0.83 / 0.99 | 46–63 |

  Chosen: preset 6, crf 28 for cut parts, crf 32 for longer parts. A smart cut of both
  recordings with SVT-AV1 parts passed Keepcut's verification with a full decode check.
- Step 3: `EncoderSet::software(Av1)` prefers SVT-AV1, libaom is the fallback.
- Step 5: the pipeline test `av1_smart_cut_with_each_software_encoder` cuts with each of
  the two encoders the local ffmpeg has. Each package build checks that its ffmpeg has
  libsvtav1 (package.ps1, build-appimage.sh, macOS package.sh, and a release-workflow step
  for the Flatpak).

Windows package sizes (gyan.dev "full, shared" was tried first; its DLLs total 245 MB,
mostly filter libraries Keepcut doesn't use):

| Windows ffmpeg | Installer | Zip | Installed |
|---|---|---|---|
| gyan.dev essentials 9.1 (v0.1.0, no SVT-AV1) | 60 MB | 134 MB | |
| gyan.dev full, shared 9.1 | 86 MB | 151 MB | 369 MB |
| BtbN gpl-shared 9.0.2 (chosen) | 63 MB | 129 MB | 317 MB |

Hardware encoder detection was the same with both full builds. BtbN keeps month-end
builds long-term and daily builds only for a few weeks, so `fetch-ffmpeg.ps1` pins a
month-end one.

## State at v0.1.0

| Package | ffmpeg | Software AV1 encoders | AV1 smart cut |
|---|---|---|---|
| Windows | gyan.dev "essentials" 9.1, static | libaom | works |
| macOS | Homebrew | libsvtav1 | fails in 0.1.0; works from commit c592f8f (SVT-AV1 fallback) |
| Linux Flatpak | built in the manifest (x264, x265, dav1d) | none | **fails** |
| Linux AppImage | built by the script (x264, x265, dav1d) | none | **fails** |

dav1d is a decoder only, so the Linux packages play and probe AV1 but can't encode it.
A working hardware AV1 encoder (NVENC on RTX 40-series, QSV on Arc, AMF on RX 7000,
VA-API) helps only whole-clip re-encodes, not the cut parts.

What exists in code (commit c592f8f):

- `encoders::CANDIDATES` has `libaom-av1` and `libsvtav1`; detection test-encodes both.
- `EncoderSet::software(Av1)` picks libaom when it works, else SVT-AV1.
- libaom settings are calibrated (Bandicam AV1 4K: SSIM 0.9993 at 0.42× the source
  size, ~9 fps with `-usage good -cpu-used 6 -row-mt 1 -tiles 2x2 -crf 28`).
- SVT-AV1 settings are **not calibrated**: preset 6 for cut parts, 8 for long parts
  (10 at 4K), `-crf 26` for "Match source", `-svtav1-params irefresh-type=2` (closed
  GOPs).
- The pipeline test `av1_nut_intermediates` exercises whichever software AV1 encoder
  the local ffmpeg has: libaom on Windows and Linux CI, SVT-AV1 on macOS CI.

## Steps

### 1. SVT-AV1 in the Linux builds

The main fix. SVT-AV1 builds quickly (CMake + NASM) compared with libaom, and encodes
much faster.

- **Flatpak** (`packaging/linux/io.github.borisburger.Keepcut.yml`): a `svt-av1` module
  before `ffmpeg` (CMake, `-DBUILD_SHARED_LIBS=ON -DBUILD_APPS=OFF -DBUILD_TESTING=OFF`),
  source pinned to a release tag with its commit or sha256, and `--enable-libsvtav1`
  in ffmpeg's `config-opts`. Check that the freedesktop SDK has NASM (needed for the
  assembly; without it SVT-AV1 is very slow).
- **AppImage** (`packaging/linux/appimage/build-appimage.sh`): the same, built into
  `$prefix` before ffmpeg; `deps-el9.sh` may need `nasm` (CRB/EPEL on AlmaLinux 9).
  Ship `libSvtAv1Enc.so*` and its licence (BSD-3-Clause-Clear + AOM patent licence) under
  `usr/share/licenses/`.
- Licences: add SVT-AV1 to the README/licence notes of both packages.
- Cost: the manifest's hash changes, so the next Flatpak release build starts without
  cache (the slow one); later releases reuse it.
- Check: in each package, `ffmpeg -encoders | grep svtav1`, and a smart cut of an AV1
  clip that passes Keepcut's verification.

### 2. Calibrate SVT-AV1

Needs an ffmpeg with libsvtav1 on the development machine (see step 4 for Windows
builds that have it), and the same sample libaom was calibrated on (4K Bandicam AV1
screen recording), plus an AV1 camera/phone clip if one is available.

- For presets 6, 8 and 10 and a CRF range around 24–32: SSIM against the source, size
  relative to the source, encode speed. Aim for libaom's result: SSIM ≥ 0.999 at well
  below the source size.
- Cut parts: confirm that SVT-AV1 heads join cleanly with copied AV1 parts (Keepcut's
  verification passes; full decode check without errors), including the hidden
  frames SVT-AV1 uses and the closed-GOP setting.
- Record the result next to the libaom note in `encoders.rs` and update the
  `software_av1_falls_back_to_svt` unit test.

### 3. Prefer SVT-AV1

After step 2, make `EncoderSet::software(Av1)` pick SVT-AV1 first and libaom as the
fallback (Windows' current ffmpeg). Reason: much faster at similar quality, which
matters for whole-clip re-encodes (libaom manages ~9 fps on 4K).

### 4. Windows ffmpeg build

The current build (gyan.dev essentials) has only libaom. That keeps working; switching
gives SVT-AV1 on Windows too. Builds that include both libsvtav1 and libaom:

| Build | Version | Zip size | Notes |
|---|---|---|---|
| gyan.dev essentials (current) | 9.1 | 119 MB | static; **no SVT-AV1** |
| gyan.dev full | 9.1 | 264 MB | static; ffmpeg.exe and ffprobe.exe each carry every library |
| gyan.dev full, shared | 9.1 | 105 MB | DLLs shared by ffmpeg and ffprobe |
| BtbN gpl | 9.0 (release branch) | 198 MB | static |
| BtbN gpl-shared | 9.0 (release branch) | 87 MB | DLLs shared; BtbN has no 9.1 build yet |

(Sizes from the releases on 2026-11-22. BtbN enables SVT-AV1 for 64-bit Windows and
libaom for everything but Windows on ARM; see `scripts.d/50-svtav1.sh`, `50-aom.sh` in
github.com/BtbN/FFmpeg-Builds.)

Original suggestion (superseded, see Status): **gyan.dev full, shared**. Same publisher and version as now, has
SVT-AV1 and libaom, and the package likely gets *smaller*, because the libraries are
stored once instead of in both exes. Changes:

- `packaging/windows/fetch-ffmpeg.ps1`: the `full_build-shared` asset, checked against
  gyan.dev's published checksum as now.
- `packaging/windows/package.ps1`: copy the `bin/*.dll` files next to `ffmpeg.exe` and
  `ffprobe.exe`, and update the licence/source notes (the full build lists more
  libraries).
- `keepcut-player/build.rs` copies only libmpv; check that the ffmpeg DLLs don't clash
  with anything mpv loads (libmpv-2.dll is self-contained, so it shouldn't).
- Windows CI runs the tests with the fetched ffmpeg, so it picks up the change.
- Measure the installer and zip sizes before and after.

### 5. Tests

- Linux CI uses BtbN's static ffmpeg, which has both encoders, so it tests libaom
  (until step 3) and macOS tests SVT-AV1. Add a pipeline test that builds the plan
  with an encoder set *without* libaom (when the local ffmpeg has libsvtav1), so the
  SVT-AV1 path is tested on Linux CI as well, and the libaom path after step 3.
- A package-level check in the release workflow: run the packaged ffmpeg with
  `-encoders` and fail if no software AV1 encoder is listed.

### 6. Release

- 0.1.1 with steps 1, 4 and the tests; steps 2 and 3 can follow when calibrated, or go
  in the same release if the calibration is done first.
- Release notes: "AV1 smart cuts now work in the Linux and macOS packages".

## v0.1.0 release note

Known limitation: smart cuts of AV1 video don't work in the Linux packages and the
macOS package (the software AV1 encoder is missing there). Windows is not affected,
and H.264/HEVC video is not affected anywhere.
