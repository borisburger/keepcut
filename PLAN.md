# Keepcut — Implementation Plan

Status: **Phase 0A (export pipeline) and Phase 0B (player) spikes done** on the author's Windows laptop (results and decisions: §3.3–3.5); **Phase 1 (MVP GUI) built, awaiting the author's hands-on test; Phase 2 (export completeness) done** (2026-11-18); **Phase 3 (comfort features) done; Phase 4 (packaging) first pass done** (2026-11-22). Open spike items are listed in §11. Plan written and finalized 2026-11-17.
This document is self-contained: it assumes no access to the planning conversation.

### Start here (for the implementer)
1. Read §1–§3 for goals, decisions and what testing already established. Then read §6, the export pipeline; it's the heart of the app.
2. Start with **Phase 0A** (§11): the export pipeline in `keepcut-core` + `keepcut-cli`, no GUI, validated on the sample corpus (Appendix A). Then **Phase 0B**, the player spike.
3. The dev environment on the author's Windows machine is ready (§10a). Rust 1.100 + MSVC are verified; libmpv is fetched by a script you write.
4. Keep this file current: when a spike settles an open item (§13), record the result and decision here, in the relevant section.
5. Never port LosslessCut code line by line (licence; see §2). Use this plan and ffmpeg's documentation.
6. Ask the author for missing sample media (§13) rather than guessing.

**Name:** Keepcut. You mark the parts to **keep**, and it cuts and joins them.
- The working name was "ClipCut", which is taken: a Hong Kong company has a pending US trademark application and runs an AI clipping product.
- As of 2026-11-17, no product named Keepcut was found and the crates.io name `keepcut` was free; GitHub has only a few small unrelated `keepcut` repos.
- Before publishing, check that the domain and GitHub org/repo name you want are available, and search the EUIPO/USPTO trademark databases.
- Spelling: "Keepcut" in UI text, `keepcut` for crates, binaries and file extensions.
- Flathub app ID: `io.github.<github-user>.Keepcut`.

---

## 1. Goal

A lightweight, cross-platform desktop app for this workflow:

1. Record many clips (mostly Bandicam screen recordings, sometimes a phone or camera; sources are **not** mixed within one project).
2. Load all clips at once, and on each clip mark the parts to **keep** (segments).
3. Export everything in one go, either:
   - **joined** into a single movie, or
   - as **separate files** (one per source clip, or one per segment).
4. Keep the original quality: copy the video stream wherever possible, and re-encode only where unavoidable, at a quality matching the source.

The user never names, saves or browses intermediate files. The app may create temporary files internally, in a hidden temp folder that is always cleaned up.

**Why this exists:** Bandicut and LosslessCut (https://github.com/mifi/lossless-cut, cloned at `C:\Projects\lossless-cut`) both require cutting each clip, saving it to disk, switching to a separate join tool, and re-loading the cut files. The user likes Bandicut's simple UI and finds LosslessCut's UI counter-intuitive.

### Users and platforms
- The author and colleagues.
- Windows and Linux (Ubuntu, Kubuntu, Debian, Fedora, AlmaLinux); macOS nice-to-have (untested, the author has no Mac).
- Users can install software themselves.
- Hardware:
  - the author's hybrid laptop: Intel iGPU + NVIDIA RTX 4060 (driver 621.24). In ffmpeg, NVENC and QSV both work. Bandicam on this laptop records with **Intel QSV**, because its NVENC would not initialize even with the high-performance GPU setting;
  - a work laptop with an integrated AMD Radeon (AMF on Windows, VAAPI on Linux).

### Typical media
- **Bandicam:** H.264 (HEVC support wanted), 1080p up to 3200×2000, 15 fps (sometimes 20–30 fps), MP4, AAC, usually one audio track (mic; sometimes mic + desktop sound, possibly as a second track — verify).
  - The encoder varies by machine: NVENC, Intel QSV or AMD AMF. Each produces slightly different bitstreams (see §3.2 items 11–12).
- Clips usually 30 s to a few minutes. Occasionally multi-hour recordings of presentations, up to a few GB.
- Phone/camera clips, occasionally.
- **Sometimes 4K (3840×2160) content**: screen recordings and/or camera/phone footage, possibly 50/60 fps, HEVC, 10-bit, at high bitrates (50–150 Mbps for camera footage). Everything must work at 4K (§6.14 collects the 4K-specific rules).

### Requirements (functional)
- F1: Add many clips (file dialog and drag & drop). Default order is by recording time; clips can be reordered by drag.
- F2: Per-clip editor: video preview, play/pause, frame step forward/back, jump to previous/next keyframe, keyframes visible on the timeline.
- F3: Segments (keep-ranges) per clip: create, trim start/end, split, delete; frame-accurate.
- F4: Preview a clip with only its segments ("cut preview"), and preview the whole project.
- F5: Undo/redo for all segment and clip-list edits.
- F6: Export modes: single joined file / one file per source clip / one file per segment.
- F6a: Two encoding modes, like Bandicut's "high-speed" vs "encoding" modes:
  - **Smart:** lossless stream copy wherever possible; re-encode only between a cut point and the nearest keyframe, and clips that don't match the target.
  - **Full re-encode:** everything re-encoded with one set of output settings (codec, quality, optionally resolution/frame rate).
- F7: Output folder defaults to the source folder; user can browse to a custom folder, which is remembered.
- F8: Clips that cannot be stream-copied into the joined output (different resolution, codec, …) are detected on load, the user is told why, and they are re-encoded, scaled **up** (or down) to the target resolution.
- F9: Optional loudness normalization: Off / whole video / per clip / per segment. Default target −14 LUFS.
- F10: Optional chapters: none / one per source clip / one per segment. Chapters are embedded in the MP4, and a YouTube description timestamp block can be copied to the clipboard.
- F11: Optional project save/load, plus autosave and crash recovery.
- F12: An ffmpeg command log, viewable and copyable, for debugging.

### Requirements (non-functional)
- Same look and behaviour on every OS; no web view; small footprint.
- Frame-accurate cuts. A/V sync within one audio frame (≈21 ms) after any number of joins.
- No quality loss on copied parts. Re-encoded parts must be visually indistinguishable from the source, without exceeding the source's quality or bitrate unnecessarily.
- A responsive UI while probing and exporting (all heavy work off the UI thread).

### Out of scope (v1)
Transitions/fades, titles/overlays, effects, cropping, rotation editing, multi-track timeline, mixing of different source types in one project as a design goal (it works through re-encoding, but isn't optimized), a web version.

---

## 2. Key decisions

| Topic | Decision | Rationale |
|---|---|---|
| Language | Rust | The author's preference; lightweight, single binary. |
| UI toolkit | **egui via eframe** (eframe 0.37.0, released 2026-10-15), using the **`glow` (OpenGL) renderer** (`Renderer::Glow`; recent eframe defaults to wgpu, so select glow explicitly) | Native Rust; draws everything itself, so it looks the same on every OS. Popularity equal to iced (~32k GitHub stars each), very active. **Decisive reason:** libmpv's render API outputs only to OpenGL or to a CPU buffer. eframe's glow backend exposes the GL context (`CreationContext::gl`) and loader (`CreationContext::get_proc_address`), so mpv renders on the GPU directly into a texture egui displays. iced is wgpu-only (no GL backend); it would need platform-specific GL↔wgpu interop or the slower CPU path. Immediate mode also suits the custom timeline and snapshot undo. |
| Video playback | **libmpv OpenGL render API** rendering into an app-owned GL framebuffer/texture, shown in egui via `egui_glow` native texture registration | mpv plays everything, does exact-frame seeking, frame back-step and hardware decoding, and its EDL playlists give a free "cut preview". |
| Playback fallback | libmpv **software** render API (`MPV_RENDER_API_TYPE_SW`) into an `egui::TextureHandle` (`set()` per frame) | Same toolkit; used if GL interop misbehaves on some machine/driver. Selectable in Settings. |
| Rust mpv bindings | `libmpv2` (crates.io, v7.0.1, repo `kohsine/libmpv-rs`) for the client API (commands, properties, events); **render contexts via raw FFI over `libmpv2-sys`** | Verified in 0B: libmpv2's render module covers OpenGL only (no SW), and its `RenderContext<'_>` borrows the handle. A small own wrapper (`keepcut-player/src/render.rs`) does both GL and SW. `Mpv::ctx` is public. |
| libmpv for Windows | **Official mpv release build**, `libmpv-v<ver>-x86_64-w64-mingw32-lgpl.zip` from github.com/mpv-player/mpv releases (v0.42.0 in 0B), with its `.asc` signature | Upstream now publishes signed Windows builds of libmpv (GPL and LGPL variants), so no third-party build is needed. **LGPL** variant: playback needs nothing GPL-only; all encoding runs in the separate GPL `ffmpeg.exe`. |
| Media processing | **ffmpeg/ffprobe CLI** bundled next to the binary (not linked as a library) | Process overhead (~10–50 ms) is negligible against I/O and encoding. Crash isolation, swappable builds, loggable reproducible commands, no FFI churn. LosslessCut does the same. |
| Intermediate files | Hidden temp dir, always deleted (also on cancel/crash at next start) | The user only cares not to manage files. |
| Encoding modes | **Smart** (default) and **Full re-encode**, chosen per export (§6.0) | Smart keeps the original quality; Full gives control over codec/size/resolution and is the safe fallback if a Smart export fails verification. |
| Cutting strategy (Smart mode) | **Smart cut**: stream-copy whole GOPs, re-encode only from the cut point to the next keyframe (and, for B-frame sources, the last partial GOP) | Keyframes in Bandicam recordings are 10–30 s apart (§3.2), so keyframe snapping is unusable. |
| Audio | **Always re-encoded once**, as one continuous track built from exact-trimmed pieces | Copying audio drifted ~0.33 s per smart-cut seam in tests. One continuous encode also avoids AAC priming gaps and enables loudness normalization. |
| Container | MP4 (`-movflags +faststart`), H.264/HEVC video, AAC 48 kHz stereo, 192 kbps default | YouTube-friendly; matches sources. |
| License | **GPL-3.0-or-later** (decided). Add the `LICENSE` file and `license = "GPL-3.0-or-later"` in every `Cargo.toml` at repo init. | **Not constrained by LosslessCut:** only ideas/knowledge about ffmpeg usage were taken from it, no code, UI or assets. Do **not** translate LosslessCut's TypeScript line by line into Rust (a translation is a derivative work of GPL-2.0-only code); implement from this plan. **Constrained by what we ship:** libmpv standard builds are GPLv2+ and we link them, so the distributed bundle is GPL as a whole. ffmpeg runs as a separate process (no linking); ship/point to the exact corresponding source of the bundled ffmpeg build. egui/eframe are MIT/Apache. |
| Linux distribution | Flatpak (bundles libmpv + ffmpeg with H.264/HEVC) | Fedora/Alma system ffmpeg lacks H.264. Flatpak works on all target distros. |
| Windows distribution | Portable zip + installer, bundling `ffmpeg.exe`, `ffprobe.exe`, `libmpv-2.dll` | |

---

## 3. Background research (already done)

### 3.1 How LosslessCut uses ffmpeg (reference: `C:\Projects\lossless-cut`)
LosslessCut is Electron + React and spawns the `ffmpeg`/`ffprobe` executables (`src/main/ffmpeg.ts`). The arguments are built in `src/renderer/src/hooks/useFfmpegOperations.ts`.

- **Lossless cut** (`losslessCutSingle`, ~line 256):
  `ffmpeg -ss <start> -i in.mp4 -t <dur> -map 0:<ids> -c copy -avoid_negative_ts make_zero -map_metadata 0 -movflags +faststart out.mp4`.
  With `-ss` before `-i` and stream copy, the start snaps back to the keyframe at or before `start`.
- **Smart cut** (`src/renderer/src/smartcut.ts` and `cutMultiple` ~line 690–740):
  - Read keyframes around the cut via `ffprobe -show_packets -read_intervals`.
  - Re-encode `[cut, nextKeyframe − 1 frame]` with the same codec name, 1.2× the source bitrate, the same timebase and `-bf`.
  - Stream-copy `[nextKeyframe, end]`.
  - Concat the two.
- **Merge** (`concatFiles`, ~line 132): the concat demuxer, with the list on stdin as `file 'file:<abs path>'` (escape `'` as `'\''`), `-f concat -safe 0`, `-c copy`, `-ignore_unknown` (data tracks such as timecode would otherwise fail; LosslessCut issue #170), and `-map_metadata 1` taken from the first file as a second input.
- **Cut + merge of one file:** writes segment files, concats them, optionally deletes them (`autoDeleteMergedSegments`).
- **Compatibility check before merging** (`src/renderer/src/components/ConcatDialog.tsx` ~line 156) compares `codec_name, width, height, pix_fmt, level, profile, sample_fmt, r_frame_rate, time_base`.

### 3.2 Test results on the sample corpus (ffmpeg 9.0.2 gyan.dev essentials, Windows)
The sample clips are in the project root folder, to be moved to `samples/` (see Appendix A for full properties).

1. **Bandicam keyframes are far apart.**
   - Bandicam skips unchanged frames (VFR: frame deltas of 0.067/0.133/0.2 s at 15 fps), so its frame-based keyframe interval (~150 frames) becomes **10–13 s** on active footage.
   - A 6-hour, mostly static recording had only 684 keyframes (avg **~32 s** apart).
   - So keyframe snapping is not acceptable: smart cut is a core feature.
2. **Reading the full keyframe list is fast:** `ffprobe -select_streams v:0 -show_entries packet=pts_time,flags` on the 6-hour file took ~1 s (the MP4 index is read, not the video).
3. **Copy-only join of 3 compatible Bandicam clips**, keyframe-snapped (requested 125 s): 147.5 s output (22.5 s of unwanted extra footage); A/V lengths matched; no decode errors.
4. **Smart-cut join of the same 3 segments** (head re-encoded with libx264 CRF 18 up to the next keyframe; rest copied; all pieces concatenated):
   - video **125.18 s (exact)**, no decode errors;
   - audio (copied) 126.19 s: drift of ~0.33 s per seam. Hence the separate audio pipeline (§6.6).
5. **Re-encoded heads at x264 CRF 18 were at or below the source bitrate** (screen content). Matching source quality does not inflate size.
6. **Joining clips with different timebases/odd frame rates** (Bandicut re-encodes of VFR phone footage: 27.41/27.86/29.87 fps, timebases 1/10964, 1/11144, 1/11948) by stream copy produced **video timestamp gaps of 0.15 s and 0.9 s** at the joins, so A/V desync. `-video_track_timescale 90000` on the final output did **not** fix it. `time_base` must therefore be part of the hard compatibility check (§6.2).
7. **Joining 2 identical-format DaVinci Resolve exports** (H.264 High, 29.97 fps, with a timecode data track) by stream copy: clean (video 11.33 s, audio 11.35 s, no errors). Needed `-ignore_unknown` because of the timecode track.
8. **Single-pass concat with `inpoint`/`outpoint` directives** produced "non monotonically increasing dts" errors. **Rejected**; use separate pieces + concat.
9. **Hardware encoder availability is not guaranteed** even when listed by `ffmpeg -encoders`: ffmpeg 9.0.2's `h264_nvenc` needs NVIDIA driver ≥ 610 (the machine had 577 at the time). After upgrading to 621.24, `h264_nvenc`, `hevc_nvenc` (including 4K 10-bit Main10), `av1_nvenc` and `h264_qsv` (Intel iGPU) all pass the test-encode; `h264_amf` is unavailable (no AMD GPU in this machine). Hence runtime detection by test-encode (§6.8).
10. One Bandicam clip (`2025-05-07`) is 720p with 48 kHz audio, among 1080p/44.1 kHz clips: a real-world mismatch the app must handle (re-encode + upscale, and tell the user).
11. **Bandicam HEVC from Intel Quick Sync (QSV) breaks plain MP4 stream copy** (tested 2026-11-17 on `bandicam 2026-11-17 19-40-12-243.mp4`, 3200×2000):
    - The source's `hvcC` header lists its parameter sets in a non-standard order (PPS, SPS, VPS). ffprobe/ffmpeg warn "SPS 0 does not exist / VPS 0 does not exist", but the source decodes fine (51/51 frames), because VPS/SPS/PPS are also repeated in-band before every IDR frame.
    - `ffmpeg -i src -c copy out.mp4` produced an **undecodable** file (every frame: "No start code is found / Invalid data"), and concat-demuxing that file failed outright.
    - `-bsf:v hevc_metadata` refuses the stream ("SPS id 0 not available").
    - **Working route:** `-c copy -bsf:v hevc_mp4toannexb` to `.ts`, then `-c copy` to `.mp4`. That gave 51/51 frames and zero decode messages.
    - A libx265 head concatenated with a copied QSV piece in MP4 failed for the same reason.
    - ⇒ **Strategy A (MPEG-TS/Annex B intermediates, §6.7) is mandatory for HEVC sources**, and the default unless spike 0A shows MP4 intermediates are clean for H.264 too.
12. **Bandicam H.264 from QSV** (`bandicam 2026-11-17 19-39-51-272.mp4`, 3200×2000, Main, level 5.1) copies and concatenates cleanly in MP4 (124/124 frames, no decode messages).

### 3.3 Spike 0A results (2026-11-17, `keepcut-cli`, ffmpeg 9.0.2 gyan.dev essentials)
Test projects are in `samples/projects/*.json` (gitignored with `samples/`); synthetic 4K clips in `samples/synth/`. Every row was run in **Smart** and **Full re-encode** mode with a full decode check. "ts err" = the largest difference between an output frame's timestamp and its source frame's timestamp.

| Set | Content | Smart | Full |
|---|---|---|---|
| `nvenc` | 3 Bandicam NVENC clips (starts off keyframes) + the 720p clip (upscaled) | ✓ 89.667 s exact, A−V 0.1 ms, 1256/1256 frames, ts err 0 | ✓ same |
| `davinci` | DaVinci B-frame clips (5-frame decode delay), head + body + tail, 3 segments | ✓ 26.860 s, A−V 0, ts err 0 | ✓ (h264_nvenc) |
| `qsv-h264` / `qsv-hevc` | Bandicam QSV, two cut pieces each | ✓ (HEVC via the TS route, clean) | ✓ |
| `bandicut` | three odd time bases (1/10964, 1/11144, 1/11948) | ✓ all **copied** (see the time-base decision below), ts err 0.013 ms | ✓ (h264_nvenc) |
| `4k-h264` | synthetic 3840×2160 30 fps | ✓ | ✓ (h264_nvenc) |
| `4k-hevc10` | synthetic 3840×2160 60 fps HEVC Main10, **open GOP** (x265 CRA + leading pictures) | ✓ 606/606 frames, ts err 0 | ✓ |
| `sixhour` | the 6-hour file: 2 segments, one deep inside, one to the end | ✓ 11.5 min output in 8.4 s; probe 1.4 s | ✓ (after the screen-content `-bf 0` decision below) |

Integration tests on synthetic media (`crates/keepcut-core/tests/pipeline.rs`: CFR, VFR via `mpdecimate`, B-frames with tail, HEVC open GOP, 720p mismatch, no audio, two audio tracks) pass; `cargo test -p keepcut-core --test pipeline -- --ignored` runs all corpus projects in both modes.

**Problems found and decisions taken (all reflected in §6):**
1. **Mixed decode delays break DTS at seams.** The DaVinci source has pts−dts = 5 frames, the x264 heads 2 frames, so DTS went backwards at head→copy seams; ffmpeg "fixed" them and the output lost 100 ms. **Decision:** every piece of an output gets the same decode delay (the largest one): copied pieces get `setts=pts=PTS:dts=DTS-<shift>/TB` in their bitstream filter chain; encoded pieces are encoded first, their delay is measured (`ffprobe` first packets), and they are remuxed with the same filter if needed (§6.4). Note: `setts` with only `dts=` also rewrites PTS; `pts=PTS` is required.
2. **Open GOP (HEVC CRA, possibly H.264 recovery-point I-frames).** Copying by decode-order packet count included the start keyframe's leading pictures (they reference the previous GOP) and missed the end keyframe's leading pictures. **Decision (§6.3):** drop the start keyframe's leading pictures from the copy with `noise=drop=gte(n\,1)*lte(n\,R)` (the head covers them; `-frames:v` counts packets after the filter), and end the copied body where the last keyframe's leading pictures begin, re-encoding the tail from there. This resolves risk §13.3; the planned "extend the head to the second keyframe" fallback isn't needed.
3. **Time base demoted to a soft key (§6.2 open item: yes).** With MPEG-TS intermediates (90 kHz) and exact `duration` directives in the concat list, the three odd-time-base Bandicut clips stream-copy into one output with timestamps within 0.013 ms of the source and no gaps. The output uses `-video_track_timescale 90000` when sources differ, otherwise the common source time base.
4. **Concat lists carry exact `duration` directives** (`ffconcat version 1.0`), taken from the plan (frame-exact `E − S`), so the concat demuxer never estimates TS/FLAC durations. Audio piece length D also comes from the plan (it equals the produced video by construction; the per-frame timestamp check in verification proves it), not from probing the produced pieces.
5. **FLAC audio pieces:** `asetnsamples=n=4608:p=0` is required (the FLAC encoder rejected a 6-sample first frame from `aresample=async=1:first_pts=0`), and all pieces are `-ar 48000 -ac 2 -sample_fmt s16`, because the concat demuxer stopped at a piece whose sample format differed (silence vs mixed tracks).
6. **Encoder time base:** encode pieces use `-fps_mode passthrough -enc_time_base:v demux`, so VFR timestamps aren't rounded to 1/fps. `-noautorotate` is an input option (before `-i`).
7. **Verification additions:** frame count per piece right after it's produced; the full list of output frame timestamps compared with the source's (catches any retiming); the decode check runs `-fps_mode passthrough -enc_time_base:v demux` so the null muxer doesn't report harmless "non monotonically increasing dts" on mixed-rate outputs.
8. **Processes:** `std::process` + threads with a polling supervisor (cancel = kill), not tokio. The core is synchronous and runs on worker threads; no async runtime needed. Revisit only if the GUI needs it.

**Calibration (x264, screen content, 1080p Bandicam segment of 20 s; size relative to the source packets of the same range):**

| Setting | Size | SSIM |
|---|---|---|
| crf 18, `-g 300` | 1.04× | 0.9994 |
| crf 20 | 0.92× | 0.9992 |
| crf 22 | 0.82× | 0.9989 |
| crf 18 `-tune stillimage` | 1.14× | 0.9993 |
| crf 18, `-g 30` (2 s GOP) | **1.64×** | — |
| crf 18, `-g 150` (10 s GOP) | 1.05× | — |

- Smart heads/tails: SSIM 0.9992 (Bandicam head, x264 slow crf 18), 0.992–0.994 (DaVinci head/tail), 0.998 (4K x264 medium). Full (h264_nvenc cq 19) on DaVinci: 0.9955; x264 crf 18 on Bandicam: 0.9995. **All ≥ 0.99; presets kept.**
- `-tune stillimage` doesn't help; not used.
- **Decision:** Full re-encode of **screen content uses no B-frames** (`-bf 0`; they saved only 3% on Bandicam footage). With B-frames on VFR footage, the decode delay spans long frame gaps (0.53 s on the 6-hour file) and the MP4 video track ended one frame (67 ms) short of the audio. (The last-frame-duration problem found later, §3.4 item 1, may have contributed; the `-bf 0` choice stands on its own because B-frames gain little here.) **Risk:** VFR camera/phone footage with B-frames could hit the same; test with a real phone clip.
- **Decision:** Full re-encode keyframe interval is **10 s for screen content** (2 s cost +64% size on Bandicam footage) and 2 s for camera footage. With that, Full re-encode of the `nvenc` set is 8.1 MB vs 9.1 MB for Smart.
- **4K head encode times** (this laptop): x264 `medium` at 2160p30 ≈ 38 fps (a 3.7 s head took 2.9 s); **x265 `fast` at 2160p60 10-bit ≈ 8 fps** (1.3 s head: 8–11 s). A 30 s GOP at 4K60 would take ~4 min per head with x265, so "Fast heads (hardware encoder)" (§6.14) is needed for 4K HEVC screen recordings. Not yet measured: hardware heads (hevc_nvenc) and their concat compatibility.

---

### 3.4 More Bandicam samples: 4K QSV H.264/HEVC, NVENC AV1, 1080p 29.97 fps (2026-11-17)
New samples from the author (Bandicam 8.0.1, all VFR with frame skipping, AAC 44.1 kHz, no color tags):

| File (prefix `bandicam 2026-11-17 `) | Codec | Res | Time base | Keyframes |
|---|---|---|---|---|
| `22-37-27-327-nvenc-av1-4k`, `22-37-43-926-nvenc-av1-4k` | AV1 Main (NVENC), level 12 | 3840×2160 | 1/15000 | 0, 10.133 / 0 |
| `22-38-44-032-intel-hevc-4k`, `22-39-01-100-intel-hevc-4k` | HEVC Main (QSV), level 150 | 3840×2160 | 1/15000 | 0 only |
| `22-39-52-089-intel-h264-4k`, `22-40-03-223-intel-h264-4k` | H.264 Main (QSV), level 5.1 | 3840×2160 | 1/15000 | 0 only |
| `22-41-01-241-intel-h264-1080p`, `22-41-23-134-intel-h264-1080p` | H.264 Main (QSV), level 4.0 | 1920×1080 | **1/29970** (29.97 fps) | 0, 17.117 / 0 |

| `22-36-13-937-x264`, `22-36-30-001-x264` (Bandicam **software** H.264; encoder tag has no encoder name) | H.264 **Constrained Baseline**, level 5.2 | 3840×2160 | 1/15000 | 0, 11.067 / 0, 10.067 |

In several files the audio runs 0.18–0.27 s longer than the video; harmless, since cuts follow the video.

Projects `b4k-h264`, `b4k-hevc`, `b1080-h264-2997`, `b4k-av1`, `b4k-mixed` (H.264 + HEVC + AV1 joined; HEVC becomes the target, the others are re-encoded to HEVC), `b4k-x264` (head + copy across a keyframe, whole re-encode of a keyframe-less segment) and `b4k-x264-qsv` **all pass in both modes**: exact duration, A−V 0.0 ms, ts err 0, clean full decode.

- **Software vs QSV H.264 don't mix by stream copy:** Bandicam's x264 writes Constrained Baseline and its QSV encoder writes Main, so joining them re-encodes the non-target clips ("profile Main vs Constrained Baseline"). Profile stays a hard key: the MP4 `avcC` declares one profile, and hardware decoders configured from it may fail on a later SPS with a higher profile. A user who mixes encoders gets re-encodes, with the reason shown.
- x264 4K Baseline head (libx264 slow→medium at 2160p, crf 18, `-profile:v baseline`): SSIM 0.9998 at **1.29×** the source bytes. That's slightly above the ~1.2× target, because crf 18 is higher quality than Bandicam's software encoding. Acceptable for heads; if it matters, derive the head CRF from the source bitrate later.

**Problems found and decisions:**
1. **Last frame duration.** MPEG-TS stores no packet durations, so the final mux guessed 1/fps for the last frame. When the last kept frame lasted longer (VFR), the video track ended 67 ms before the audio (4K sets). **Decision:** the final mux sets the duration of the last packet so the track ends exactly at the planned end: `-bsf:v setts=pts=PTS:dts=DTS:duration=if(eq(NEXT_DTS\,NOPTS)\,<end>/TB-(PTS-STARTPTS)\,DURATION)`. Verification now allows only 2 ms (was one frame).
2. **AV1 can't use MPEG-TS** (this ffmpeg writes it as an opaque `bin_data` stream), and **Bandicam's NVENC AV1 files have a malformed `av1C`**: first byte `0xC0` (version 64) instead of `0x81`; the sequence header inside it is valid. libaom only warns ("Unknown version 64 of AV1CodecConfigurationRecord"), but the Matroska, IVF (its forced `av1_metadata` filter) and MP4 muxers all refuse it. **Decision:** intermediates for codecs other than H.264/HEVC are **NUT** (stores codec headers without parsing; keeps VFR timestamps). For AV1 outputs, the final mux adds `extract_extradata` before `setts`, so the MP4 gets a valid `av1C` built from the in-band sequence header. The output decodes with zero messages, not even the version warning.
3. **AV1 heads** use `libaom-av1 -usage good -cpu-used 6 -row-mt 1 -tiles 2x2 -crf 28 -b:v 0` (software, like the other heads). 4K, 100 frames: libaom 11.4 s, 0.42× the source size, SSIM 0.9993; `av1_nvenc -cq 36`: 1.8 s, 1.17×, SSIM 0.998 (cq 25/30 are larger than the source; cq 42: 0.87×, 0.996). av1_nvenc cq 36 is the "Fast heads" setting for AV1. This ffmpeg build has no SVT-AV1; decoding AV1 uses libaom too (no dav1d in the gyan.dev essentials build; consider the full build or hardware decoding later).
4. **Windows Media Player shows the Bandicut clips darker than VLC.** Their source files (`Katka50-*` without "Hudba") are Bandicut NVENC re-encodes of phone HDR footage: 8-bit H.264 that kept the **HLG / BT.2020** tags. Media Player honors the tags and tone-maps; VLC ignores them. The author confirmed the source looks equally dark in Media Player, so Keepcut's outputs are faithful (tags copied). Optional later: offer to retag such clips as BT.709 if the content is really SDR, or flag "HDR-tagged 8-bit" clips in the clip list.

### 3.5 Spike 0B results: player (2026-11-18, author's Windows laptop)
`crates/keepcut-player` (eframe 0.37 glow + libmpv v0.42.0 official LGPL build). Features: open file (dialog or argument), play/pause, seek by timeline click/drag, frame step both ways, keyframe jumps from the `keepcut-core` frame index (keyframe ticks on the timeline), cut preview of typed segments via EDL, `--project <json>` previews all segments of a project via EDL, debug overlay (render path, hwdec, fps, drops, render ms, CPU), GL path with automatic fallback to the SW path, `--sw`, `--hwdec`, `--step-mode`, `--bench` (scripted gate: 10 s play, 8 random seeks, 8 steps, 8 back-steps inside a GOP; prints JSON), `--screenshot PATH` / F12 (egui's own frame capture). Benchmarks: `crates/keepcut-player/bench.ps1`.

Release build, window 1400×900 (video area ≈ 1300×700 logical). Times in ms, avg / max; CPU = % of one core during playback.

| File | Path | Playback | CPU | Seek | Frame step | Back-step |
|---|---|---|---|---|---|---|
| DaVinci 1080×1920 H.264 30p, B-frames | GL | 29.9/29.97 fps, 0 drops | 13–15 | 77 / 115 | 46 (fast) | 71 / 86 (index seek) |
| Bandicam 1080p H.264, 12 s GOP | GL | 11.6 fps (VFR), 0 drops | 9 | 177–239 / 266–397 | 47 (fast) | 224–243 / 283–298 |
| 6-hour Bandicam 1080p, ~32 s GOP | GL | 13.3 fps (VFR), 0 drops | 13–14 | 181–288 / 267–464 | 70 (fast) | 162–223 / 183–281 |
| Bandicam 4K HEVC QSV | GL | 9.0 fps (VFR), 0 drops | 12–14 | 311–318 / 483–517 | 52 (fast) | 305–310 / 317–333 |
| synthetic 4K30 H.264 | GL | 29.9 fps, 0 drops | 20 | — | 76 | — |
| synthetic 4K60 HEVC Main10 | GL | 59.8 fps, 0 drops | 42–47 | **531 / 1034** | 78 | 644 |
| Bandicam 4K AV1 (NVENC) | GL | 14.8 fps, 0 drops, **software decode** (hwdec "no") | 35 | 144 / 155 | 113 (mpv) | — |
| DaVinci 1080p30 | SW | 29.6 fps, 0 drops | 80 | 66 / 132 | 63 | 42 |
| 6-hour Bandicam | SW | 13.3 fps, 0 drops | 114 | 187 / 282 | 89 | 132 |
| synthetic 4K30 H.264 | SW | 29.8 fps, 0 drops | 138 | 474 / 700 | 64 | 358 |
| synthetic 4K60 HEVC Main10 | SW | 59.6 fps, 0 drops | 276 | 498 / 1034 | 86 | 592 |

**Findings and decisions:**
1. **GL path works** on this hybrid Intel + NVIDIA laptop: mpv renders into an app-owned FBO/texture registered with `Frame::register_native_glow_texture`; the texture object is resized in place (`glTexImage2D` on the same name), so the egui `TextureId` stays valid. After rendering, bind framebuffer 0 and reset the viewport. **No `FLIP_Y`**: with egui's texture coordinates the unflipped FBO is upright (FLIP_Y=1 showed the video upside down). Render cost 0.2–0.4 ms per frame.
2. **Hardware decoding is copy-back** (`d3d11va-copy`) with `hwdec=auto-safe` and `auto`. Zero-copy `d3d11va` and `nvdec` cannot interop with this GL context and mpv falls back to software decoding (130% CPU at 4K60). Copy-back still plays 4K60 HEVC 10-bit smoothly at ~45% CPU. **Default stays `auto-safe`.** mpv v0.42 also ships `render_d3d11.h`, a D3D11 render API; a possible zero-copy Windows path later, but egui's glow renderer can't consume it directly.
3. **Frame step:** mpv's `frame-step` plays until the next frame is due, so on Bandicam VFR footage (frame gaps up to 0.5 s) it took 113–278 ms. **Decision ("auto" step mode, default):** forward = `frame-step` at `speed=32` (reset to 1 afterwards) → 46–70 ms avg; backward = exact seek to the previous frame from the frame index (decodes from the keyframe; 71–310 ms even inside ~32 s GOPs, far under the 1.5 s gate). The ~220 ms maxima are the first step after a seek.
4. **Seek latency:** 1080p 77–290 ms avg; the 6-hour file averages under the 300 ms gate but peaks at 464 ms. **4K60 HEVC 10-bit with copy-back hwdec: 531 ms avg, 1034 ms max: fails the 500 ms gate**, while software decoding seeks in 185 ms (at 130% CPU during playback). Open: try `hwdec-extra-frames`, `vd-lavc-threads`, or switching to software decoding while paused/scrubbing.
5. **SW fallback works** and even keeps up with 4K60, but at 1.4–2.8 cores; fine as a fallback, as planned.
6. **AV1** plays with software decoding (libmpv's bundled dav1d), smooth at 4K15 with 35% CPU. AV1 hardware decoding didn't engage through `auto-safe`; not investigated.
7. **CPU** on the GL path: 9–47% of one core, "well under 1 core" ✓.
8. **Exit order:** the mpv render context must be freed before the mpv handle and while the GL context is current: free it in `App::on_exit(gl)`; otherwise the process crashed on exit (`0xC0000409`).
9. **Screenshots for debugging** use egui's own frame capture (`ViewportCommand::Screenshot`), never screen-region copies (a screen copy caught the Windows task switcher and the user's other windows).
10. Not yet checked: the AMD laptop, Linux X11/Wayland, a large monitor at 4K output size, HDR display.

## 4. Architecture

### 4.1 Repository layout (Cargo workspace)
```
keepcut/
  Cargo.toml                 # workspace
  PLAN.md
  crates/
    keepcut-core/            # no UI deps; all media logic; unit + integration tests
      src/
        lib.rs
        ffmpeg/              # locating binaries, spawning, progress parsing, cancellation
        probe.rs             # ffprobe JSON -> MediaInfo; frame/keyframe index
        model.rs             # Project, Clip, Segment, ExportSettings (serde)
        compat.rs            # copy-signature, target selection, per-clip verdicts
        plan.rs              # Project + MediaInfo + settings -> ExportPlan (pure, testable)
        exec.rs              # runs an ExportPlan: temp dir, jobs, progress, cancel, cleanup
        encoders.rs          # hardware/software encoder detection and quality presets
        audio.rs             # audio piece extraction, loudness measure, gain
        chapters.rs          # ffmetadata writer, YouTube block
        verify.rs            # post-export checks
        project_io.rs        # load/save/autosave, schema versioning
        history.rs           # undo/redo
        timefmt.rs           # time parsing/formatting
    keepcut-cli/             # thin CLI over core (used for spikes, CI integration tests, debugging)
    keepcut/                 # egui/eframe GUI app (glow renderer)
      src/
        main.rs              # eframe::run_native with Renderer::Glow
        app.rs               # eframe::App: state, update() builds panels each frame, message/event draining
        player/              # mpv wrapper: client handle, event thread, GL render target (+ SW fallback)
        widgets/
          video.rs           # shows the mpv-rendered GL texture (or SW texture), letterboxed
          timeline.rs        # custom egui widget (ui.painter()): segments, keyframes, playhead, zoom/pan, drag
        views/               # clip list, editor, segment list, export dialog, progress, settings
        config.rs            # user settings (TOML)
  packaging/
    windows/                 # installer script, bundling script
    flatpak/                 # manifest
  samples/                   # local test media (gitignored)
```
Move the sample `.mp4` files from the project root into `samples/` (gitignored) when initializing the repo. Add `.gitignore` with `target/`, `samples/`, `*.mp4`.

### 4.2 Main dependencies (verify current versions)
- GUI: `eframe` 0.37 (features: `glow`, `persistence` optional) + `egui` + `egui_glow`. Check the eframe 0.37 docs/examples (`custom_3d_glow`, which shows raw GL inside egui) for exact APIs; egui's API changes between releases. Optional: `egui_extras` (tables for the segment list), `egui_dnd` or a hand-rolled drag-reorder for the clip list.
- Playback: `libmpv2` / `libmpv2-sys`.
- Async/process: `tokio` (process, sync, time), `futures`.
- Data: `serde`, `serde_json`, `toml`.
- Utilities: `uuid`, `directories` (config/data/cache dirs), `rfd` (native file dialogs; uses the XDG portal on Linux/Flatpak), `tempfile`, `fs2` or `sysinfo` (free disk space), `anyhow`/`thiserror`, `tracing` + `tracing-subscriber` + `tracing-appender` (file logs), `which`.
- Clipboard: egui's `ctx.copy_text`, or `arboard`.

### 4.3 Threads
- **UI thread:** the eframe event loop; it owns the GL context and never blocks.
  - Each `update()` drains the channels (mpv events, task results, export progress) and applies them to state.
  - Background threads call `egui::Context::request_repaint()` when they send something.
- **mpv event thread:** loops on `mpv_wait_event`, forwards property changes (`time-pos`, `pause`, `duration`, `eof-reached`, …) to the UI through a channel, then calls `request_repaint()`.
- **mpv rendering (GL path):**
  - Happens **on the UI thread**, because GL calls need the current context.
  - The render-context update callback (called from an mpv thread) only sets an atomic flag and calls `request_repaint()`.
  - In `update()`, if `mpv_render_context_update()` reports a new frame, render into the app-owned FBO (§7).
- **mpv rendering (SW fallback):** a dedicated render thread renders into a CPU buffer at the widget's physical pixel size, publishes `Arc<Frame>`, and the UI uploads it with `TextureHandle::set`.
- **Work tasks:** a tokio runtime on background threads handles probing (max 2–4 concurrently), thumbnail extraction and export execution. Results and progress come back through channels (`std::sync::mpsc` or `crossbeam-channel`).

---

## 5. Data model

All times are stored **relative to the file's start** (`pts_time − format.start_time`). This matches both mpv's `time-pos` (mpv rebases start time by default) and ffmpeg's input `-ss` (ffmpeg adds `start_time` to `-ss` by default).

```rust
struct Project {
    version: u32,                 // schema version, start at 1
    id: Uuid,
    clips: Vec<Clip>,             // order = output order
    export: ExportSettings,
}
struct Clip {
    id: Uuid,
    path: PathBuf,                // absolute
    file_size: u64, file_mtime: i64,   // detect changed/missing sources
    enabled: bool,
    segments: Vec<Segment>,       // sorted by start, non-overlapping
    chapter_title: Option<String>,// default: file stem
}
struct Segment {
    id: Uuid,
    start: f64,                   // seconds; snapped to a frame timestamp
    end: f64,                     // exclusive; = timestamp of first excluded frame, or clip duration
    chapter_title: Option<String>,
}
```
- New clips get **one segment covering the whole clip**, so "just join everything" needs no editing.
- A clip with zero segments, or disabled, contributes nothing.
- `MediaInfo` (not saved in the project; cached in the cache dir, keyed by path+size+mtime):
  - format: duration, start_time, bitrate, tags (creation_time, encoder);
  - video stream: codec, profile, level, w, h, SAR, pix_fmt, color tags, time_base, r/avg frame rate, has_b_frames, rotation (side data), field order;
  - audio streams: codec, sample rate, channels, layout;
  - data streams;
  - **frame index**: all video packet pts (sorted, relative to start) and keyframe flags, plus the decode-order packet list (needed for B-frame handling).
  - Memory: 6 h at 30 fps ≈ 650k entries ≈ 10 MB, which is fine.
- Snapping: every user-set time snaps to the nearest frame pts in the index. Display uses frame pts.

---

## 6. Export pipeline (the core; build and test it first, through `keepcut-cli`)

### 6.0 Encoding modes
The user chooses the mode in the Export dialog; the last choice is remembered. Both modes share probing (§6.1), the audio pipeline (§6.6), the final mux (§6.7), loudness, chapters, verification, naming and temp handling.

**Smart (default): "lossless where possible".**
- §6.2–6.5: stream-copy everything possible.
- Re-encode only heads/tails between a cut point and the nearest keyframe, and whole segments of clips that don't match the target signature.
- Output video format = the target signature (same codec, resolution and timing as the sources).

**Full re-encode.**
- Every segment of every clip is decoded and re-encoded with one set of output settings.
- Use cases:
  - a smaller file, or a different codec/resolution/frame rate;
  - heterogeneous sources;
  - a fallback when a Smart export fails verification. The finish dialog then offers "Re-export with Full re-encode".
- **Output settings:**
  - codec: H.264 (default) / HEVC;
  - encoder: Auto / a specific detected encoder (§6.8);
  - quality: "Match source" (default, the presets of §6.8) / custom CRF-CQ value / target bitrate (kbps);
  - resolution: Same as target signature (default) / 2160p / 1440p / 1080p / 720p / custom. Scale + pad keeps the aspect ratio (§6.5); portrait sources keep portrait orientation (the preset applies to the short side);
  - frame rate: Keep original timing (VFR passthrough, default) / constant 15, 24, 25, 30, 50, 60 fps (`-fps_mode cfr -r <N>`);
  - pixel format: 8-bit `yuv420p` (default); keep 10-bit only when the output codec is HEVC and the source is 10-bit.
  - **content type:** Screen recording (default when the source encoder tag says Bandicam/OBS, or the frame rate is ≤ 30 with VFR frame-skipping) / Camera.
    - Screen recording: prefer the software encoder (`libx264 -tune stillimage` for mostly static slides/UI, or `-tune animation`; calibrate in spike 0A against text sharpness), keep the original resolution (never scale unless asked) and keep VFR timing, so static screens cost almost no bits.
    - **Why:** the author's experience is that DaVinci Resolve re-encodes of Bandicam recordings come out huge and with blurred text. Likely causes: resampling to a timeline resolution/frame rate (15 fps VFR → 30 fps CFR duplicates frames) and generic encoder tuning.
    - Verify in 0A with a zoomed-in comparison of text crops (source vs re-encode) and SSIM on text-heavy frames.
    - Camera: hardware encoder allowed, default tuning.
- **Pipeline:** one encode piece per segment, using the §6.4 encode command over the full segment `[S, E)` (midpoint `-ss`, `-frames:v N`).
  - **All pieces use identical encoder settings and closed GOPs**, so concatenating them by stream copy is safe (identical SPS/PPS). Keyframe interval: ~2 s (`-g <2×fps>`) for camera footage, which also makes a later Smart re-cut easy; **10 s for screen content**, where 2 s GOPs cost +64% size (§3.3 calibration).
  - Encode sequentially in Phase 1; optionally 2 pieces in parallel for software encoders later.
- Compatibility badges (§6.2) still show scaling information, but no clip is "copied".
- Considered and rejected: a single ffmpeg command with `trim`/`concat` filters over all sources. It gives worse progress and cancel granularity, and has many-input limits.

### 6.1 Probing
- `ffprobe -v error -of json -show_format -show_streams <file>`.
- Frame index: `ffprobe -v error -select_streams v:0 -show_entries packet=pts_time,flags -of csv=p=0 <file>`. Keyframe = flags contains `K`. Keep decode order; also build a pts-sorted copy.
- Recording time for default sorting, in priority order:
  1. `creation_time` tag;
  2. filename timestamp (Bandicam pattern `bandicam YYYY-MM-DD HH-MM-SS-mmm`);
  3. file mtime.

### 6.2 Compatibility ("copy signature")
- **Hard keys:** a clip may be stream-copied into an output only if these equal the output target: `codec_name, profile, width, height, pix_fmt, SAR, field_order, rotation`.
- **Soft keys** (warn only): `level`, color primaries/transfer/space/range, nominal frame rate, `time_base` (sources with different time bases are joined at a 90 kHz timescale; §3.3 item 3).
- Audio is not part of the signature (always re-encoded).
- **Target signature for a joined export:**
  - Group enabled clips by signature, and pick the group with the largest total kept duration. On a tie, pick the larger pixel count.
  - The user can override the target in the Export dialog (a list of the signatures present).
- Clips outside the target group are marked **"re-encode"**, with human-readable reasons, e.g. "1280×720 vs 1920×1080 → will be re-encoded and scaled up".
- Show this **in the clip list as soon as clips are probed** (badge + tooltip), not only at export.
- In per-clip/per-segment export modes, each output's target is its own source's signature (no cross-clip compatibility needed, no scaling).
- ~~Open item for spike 0A: timescale mismatch.~~ **Settled (§3.3 item 3):** the MPEG-TS intermediates plus exact concat `duration` directives already fix it, so `time_base` is a soft key.

### 6.3 Video pieces per segment (smart cut)
Notation: segment `[S, E)`; `K1` = first keyframe ≥ S; `KL` = last keyframe ≤ E; `prev(S)` = the frame pts before S.

For a clip whose signature matches the target:
1. **If no keyframe lies in `[S, E)`**: re-encode the whole segment (one "encode" piece).
2. **Head:** if `S` is not a keyframe, re-encode `[S, K1)`. If `S == K1` (within half a frame), there is no head.
3. **Body:** stream-copy `[K1, Xend)`, where:
   - if the source has **no B-frames** (`has_b_frames == 0`, as for Bandicam NVENC): `Xend = E`. Decode order equals presentation order, so copying an exact packet count is exact;
   - if the source **has B-frames**: the copy stops before the packet of `KE` (= `E` if `E` is a keyframe, else `KL`). `Xend` = the earliest time among `KE`'s leading pictures, or `KE` if it has none. Re-encode a **tail** `[Xend, E)` if non-empty. If `E` is the end of the file, copy everything to the end (no tail).
4. **Open GOP (leading pictures):** a keyframe's *leading pictures* are the packets right after it in decode order that are displayed before it (HEVC CRA RASL/RADL; probably also H.264 open-GOP I-frames). The copy drops `K1`'s leading pictures (`noise` bitstream filter, §6.4); the head covers their times. Packets copied = decode-order count from `K1` to `KE` minus the dropped ones. If the body would be empty, re-encode the whole segment. (Verified on x265 open-GOP 4K60 10-bit; §3.3 item 2.)
5. **Uniform decode delay:** all pieces of one output must have the same pts−dts delay at their start, or DTS goes backwards at seams. The executor encodes the encode pieces first, measures their delay, takes the largest delay D among all pieces (copy-piece delays come from the frame index), and lowers DTS by `D − delay` on every other piece (§6.4, §3.3 item 1).

For a clip that does not match the target: re-encode the whole segment, with scaling (§6.5).

### 6.4 ffmpeg commands for pieces
Seconds are formatted with 6 decimals. Video-only pieces (`-an -sn -dn`); audio is handled in §6.6.

**Encode piece `[A, B)`** (head, tail, or whole segment), with N = the number of frames with pts in `[A, B)`:
```
ffmpeg -hide_banner -nostdin -y -progress pipe:1 -nostats
  -ss <midpoint(prev(A), A)>  -i <src>          # input seek; transcoding => frame-accurate; midpoint avoids rounding hitting the wrong frame
  -map 0:v:0 -an -sn -dn
  -frames:v <N>
  -fps_mode passthrough -enc_time_base:v demux   # keep VFR timestamps exactly; never duplicate/drop frames
  (-noautorotate goes BEFORE -i: it's an input option; keeps rotation as metadata, like the copied pieces)
  [-vf <scale/pad chain, only for mismatched clips, §6.5>]
  <encoder args, §6.8>
  -pix_fmt <target pix_fmt> -profile:v <target profile> -level <target level>
  -color_primaries .. -color_trc .. -colorspace .. -color_range ..   # copied from the target/source
  -bf <0 if target has_b_frames==0 else 2>
  [-display_rotation <deg> for rotated sources — verify the flag in the ffmpeg version used]
  -f mpegts <piece.ts>                           # intermediates are MPEG-TS (Annex B), see §6.7
```
If `prev(A)` doesn't exist (A is the first frame), use `-ss 0` or omit it.

**Copy piece `[K, X)`**, with M = the number of packets in decode order from the keyframe packet K up to (not including) the packet at X:
```
ffmpeg -hide_banner -nostdin -y -progress pipe:1 -nostats
  -ss <K + min(0.0005, half a frame)> -i <src>   # copy-mode seek lands on keyframe <= ss, i.e. exactly K
  -map 0:v:0 -an -sn -dn -c copy
  -frames:v <M>
  -bsf:v <chain>                                 # comma-separated, in this order:
         h264_mp4toannexb | hevc_mp4toannexb     #   by codec; puts parameter sets in-band
         noise=drop=gte(n\,1)*lte(n\,R)          #   only if K has R leading pictures (§6.3 item 4)
         setts=pts=PTS:dts=DTS-<secs>/TB         #   only if this piece's delay < the output's max (§6.3 item 5)
  -avoid_negative_ts make_zero
  -f mpegts <piece.ts>
```
(For a source already in Annex B, e.g. `.ts` input, omit the mp4toannexb filter.) Encoded pieces that need a DTS shift are remuxed afterwards: `ffmpeg -i piece.ts -map 0:v:0 -c copy -bsf:v setts=… -f mpegts piece.shift.ts`. After each piece, its packet count is checked against the plan.

### 6.5 Mismatched clips (re-encode + scale)
Video filter, keeping the aspect ratio:
```
scale=W:H:force_original_aspect_ratio=decrease:flags=lanczos,pad=W:H:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,format=<target pix_fmt>
```
- Frame timing is kept as-is (`-fps_mode passthrough`); mixed frame rates in one MP4 are fine (VFR).
- HDR (BT.2020/HLG/PQ) sources joined into an SDR target need tone mapping (`zscale`/`tonemap`). This is out of scope for v1: show a warning.

### 6.6 Audio pipeline (always re-encoded, sample-exact)
1. **Per-segment duration D** = the produced video's duration for that segment = Σ of its pieces' output durations from the plan (`E − S`, or N/fps for constant-frame-rate Full re-encodes). Audio follows video, so drift can't accumulate. (Decided in 0A: computed, not probed; verification compares every output frame timestamp with the source, which proves it.)
2. **Extract each segment's audio** as a temp FLAC (48 kHz stereo, s16):
   ```
   ffmpeg -ss <S> -t <D+1> -i <src>
     -filter_complex "<mix>aresample=48000:async=1:first_pts=0,aformat=sample_rates=48000:channel_layouts=stereo,apad=whole_dur=<D>,atrim=duration=<D>,asetnsamples=n=4608:p=0[a]"
     -map "[a]" -vn -ar 48000 -ac 2 -sample_fmt s16 -c:a flac piece_i.flac
   ```
   - `first_pts=0` pads silence if the audio starts after S; `asetnsamples` is required by the FLAC encoder; the identical sample format in every piece is required by the concat demuxer (§3.3 item 5).
   - `<mix>`: with multiple audio tracks, `amix=inputs=n:normalize=0` (default: "mix all tracks"; option: "first track only").
   - No audio stream: generate silence with `anullsrc=r=48000:cl=stereo` and `-t D`.
3. **Loudness (optional)**, see §6.9: measure, then write gain-adjusted pieces (`volume=<g>dB`).
4. **Final audio** is produced in the final mux (§6.7) from a concat list of the FLAC pieces, encoded once to AAC.
   - If loudness is on, a peak limiter runs over the whole stream: `alimiter=limit=<-1 dBTP as linear ≈ 0.891>:level=disabled`.
- Disk: FLAC at 48 kHz stereo ≈ 100 KB/s, so 6 h ≈ 2 GB of temp. This is acceptable, and checked by the free-space check (§6.10).

### 6.7 Final mux (joined mode)
```
ffmpeg -hide_banner -nostdin -y -progress pipe:1 -nostats
  -f concat -safe 0 -i video_list.txt
  -f concat -safe 0 -i audio_list.txt
  [-i chapters.ffmeta]
  -map 0:v:0 -map 1:a:0 [-map_metadata 2 -map_chapters 2]
  -c:v copy -c:a aac -b:a 192k [-af alimiter=...]
  -video_track_timescale <target tb den>
  -metadata creation_time=<first clip's creation_time> -metadata encoder="Keepcut <version>"
  -movflags +faststart -ignore_unknown
  <output.mp4>
```
- List files start with `ffconcat version 1.0` and contain, per piece, `file 'file:<abs path with ' escaped as '\''>'` followed by `duration <exact piece duration>` (from the plan). Write them to the temp dir. The `duration` directives make the concat offsets exact; without them the demuxer estimates TS durations.
- `-map_metadata -1 -map_chapters -1` when there are no chapters. The output is first written as `.<name>.keepcut-partial` in the output folder (§6.10).
- **Last frame duration:** the video bitstream filter of the final mux is `[extract_extradata,]setts=pts=PTS:dts=DTS:duration=if(eq(NEXT_DTS\,NOPTS)\,<planned end>/TB-(PTS-STARTPTS)\,DURATION)`. The intermediates carry no packet durations, so without it the last frame got 1/fps (§3.4 item 1). `extract_extradata` only for AV1 (§3.4 item 2).
- **Decision: intermediates are MPEG-TS with in-band parameter sets ("strategy A") for H.264/HEVC, and NUT for other codecs (AV1; §3.4 item 2).**
  - **Why:** re-encoded pieces (x264/x265 or a hardware encoder) have different codec parameter sets (SPS/PPS/VPS) than copied pieces from the source encoder, while MP4 stores only one `avcC`/`hvcC` header per track.
    - With Annex B pieces in `.ts`, every keyframe carries its own parameter sets, so decoders switch correctly at each seam. The final mux to MP4 keeps them in-band.
    - It is also **required** for Bandicam QSV HEVC, whose malformed `hvcC` makes a direct MP4 copy undecodable (§3.2 item 11). The `.ts` route was verified clean.
  - **Timestamps:** MPEG-TS uses a 90 kHz clock. Verify in 0A that VFR timestamps survive exactly: 90000/15000 = 6 and 90000/30000 = 3, so the observed timebases map exactly. Odd timebases (e.g. 1/10964) may round, which is one more reason they are hard-key mismatches (§6.2).
  - **Fallback (strategy B):** MP4 intermediates (LosslessCut's approach). Use only if 0A finds a concrete problem with TS for some H.264 source. Record the decision here.
  - **Player check** for the chosen strategy: ffmpeg decode, mpv, VLC, Chrome/Firefox `<video>`, Windows Media Player, and one unlisted YouTube test upload.
  - Inspect parameter sets with `ffmpeg -i <f> -c copy -bsf:v trace_headers -f null -`.
- **Per-clip / per-segment modes:** the same flow, one final mux per output file.

### 6.8 Encoders: detection and quality presets
- At startup (cached per ffmpeg binary path + mtime, re-run on demand in Settings), test-encode each candidate:
  `ffmpeg -v error -f lavfi -i color=black:s=320x240:r=15:d=0.2 -c:v <enc> -f null -`.
  Exit code 0 means usable. Candidates: `h264_nvenc, hevc_nvenc, h264_amf, hevc_amf, h264_qsv, hevc_qsv, h264_vaapi, hevc_vaapi, h264_videotoolbox, hevc_videotoolbox, av1_nvenc, av1_amf, av1_qsv, av1_vaapi, libx264, libx265, libaom-av1`. (AV1 sources: heads use libaom-av1; §3.4 item 3.)
- **Policy:**
  - **Heads/tails** (short, ≤ 1 GOP): software (`libx264`/`libx265`). Best quality per bit, deterministic, and fast enough at up to 1440p. At 4K, see §6.14.
  - **Whole-segment re-encodes:** the user's choice in Export: Auto (first working hardware encoder, else software) / a specific encoder.
- **Starting presets** ("match source quality"; calibrate in spike 0A so re-encoded pieces reach SSIM ≥ 0.98 against the source, using the `ssim` filter, while staying at or below ~1.2× the source bitrate):
  - `libx264`: `-preset slow -crf 18` (heads), `-preset medium -crf 18` (long)
  - `libx265`: `-preset medium -crf 20`
  - `h264_nvenc`/`hevc_nvenc`: `-preset p5 -tune hq -rc vbr -cq 19 -b:v 0 -spatial-aq 1` (HEVC: cq 21)
  - `h264_amf`/`hevc_amf`: `-quality quality -rc cqp -qp_i 18 -qp_p 20`
  - `h264_qsv`/`hevc_qsv`: `-preset slower -global_quality 20`
  - `h264_vaapi`: `-vaapi_device /dev/dri/renderD128 -vf format=nv12,hwupload -rc_mode CQP -qp 20`
- **Notes:**
  - NVENC minimum driver depends on the ffmpeg build (ffmpeg 9.x needs ≥ 610). Detection handles this. Show the reason when an encoder is unavailable.
  - Fedora/Alma system Mesa disables H.264/HEVC VAAPI encoding (needs RPM Fusion `mesa-va-drivers-freeworld`). The Flatpak GL runtime may differ; verify.
  - Fallback is always `libx264`/`libx265`.
  - Encoder detection uses a tiny test frame. Some hardware encoders reject 4K, 10-bit or a given profile, so before planning a job, re-test the chosen encoder once with the real target parameters (resolution, pix_fmt, profile), using a 1-frame `lavfi` source of that size. Fall back to software if it fails.

### 6.9 Loudness normalization
- **Modes:** Off / Whole video / Per clip (one gain per source clip) / Per segment. Target −14 LUFS (editable); true-peak ceiling −1 dBTP.
- **Measure** on the FLAC pieces:
  `ffmpeg -i <piece or concat of pieces> -af loudnorm=I=-14:TP=-1:print_format=json -f null -`, parse `input_i` from the JSON block on stderr.
  - Per clip: concat that clip's pieces.
  - Whole: all pieces.
  - Per segment shorter than 3 s: use its clip's value (integrated loudness is unreliable on short audio).
- **Gain** = target − measured, clamped to [−20, +20] dB. Skip units quieter than −70 LUFS (silence).
- Apply with `volume=<g>dB` into new FLAC pieces. The limiter runs in the final mux.
- In separate-file modes, "Whole video" means per output file.

### 6.10 Execution, temp files, progress, cancel
- **Temp dir:** `<OS temp>/keepcut/<uuid>/`. Before starting, estimate the space needed (sum of copied bytes ×1.1 + encoded estimate + FLAC 100 KB/s × total duration). If the OS temp volume lacks space, use the hidden dir `<output folder>/.keepcut-tmp-<uuid>/`.
- Always delete: on success, on error, on cancel. At startup, delete stale `keepcut/*` dirs older than 1 day.
- **Spawning:** `std::process::Command` on a worker thread, with a supervisor loop that polls a cancel token and kills the child (decided in 0A; no tokio needed). On Windows set `creation_flags(0x08000000)` (CREATE_NO_WINDOW), or consoles will flash.
- **Output file:** written as `<output folder>/.<name>.keepcut-partial`, verified, then renamed to the final name (also when verification reports problems; the result carries them); deleted on error or cancel.
- Read `-progress pipe:1` (`out_time_us=`) for progress. Capture stderr to the job log.
- **Overall progress:** weight each job by its media duration × a factor (copy 0.05, x264 head 1.0, hardware encode 0.3, audio extract 0.1, final mux 0.1).
- **Cancel:** kill the running child, delete temp and the partial output.
- **Command log:** every command line (shell-quoted, copyable) is appended to an in-app log panel and the log file.
- **Job order:** probe check → video pieces → measure segment video durations → audio pieces → loudness → final mux(es) → verify.

### 6.11 Verification (after each output)
- ffprobe the output:
  - video duration = Σ segment durations ± 1 frame;
  - |audio − video| ≤ 30 ms.
- **Baseline:** some sources emit harmless decoder warnings, e.g. Bandicam QSV HEVC prints "SPS 0 does not exist / VPS 0 does not exist" while decoding perfectly.
  - Before judging an output, decode-check the corresponding source range once and treat messages the source itself produces as the baseline.
  - Only *new* messages count as failures, and any frame-count mismatch always counts.
  - Never treat "ffmpeg wrote to stderr" as failure by itself; use exit codes, frame counts and new error messages.
- **Seam check:** for each seam time t: `ffmpeg -v error -ss <t-2> -t 4 -i out.mp4 -f null -` must print nothing. Also check that packet pts are monotonic and have no gap > 2× the max source frame delta around t.
- Optional "full decode check" (slow) in Settings.
- Show "Verified ✓" or a warning with details in the finish dialog.

### 6.12 Output naming and location
- **Location:** default "Same folder as source" (joined: folder of the first clip; separate modes: each next to its own source). "Custom folder" with Browse; the last custom folder is remembered in config.
- **Names** (editable in the Export dialog for joined mode):
  - joined: `<first clip stem> - joined.mp4`
  - per clip: `<stem> - cut.mp4`
  - per segment: `<stem> - part 01.mp4`
- Never overwrite: append ` (2)`, ` (3)`, … Refuse any output path equal to a source path.

### 6.13 Chapters
- Modes: none / per source clip / per segment. Titles default to the clip's file stem (per segment: `<stem> – 1`, …) and are editable in the clip list / segment list.
- Chapter times are computed from the produced segment durations.
- Embed via an ffmetadata file:
  ```
  ;FFMETADATA1
  [CHAPTER]
  TIMEBASE=1/1000
  START=0
  END=83400
  title=Intro
  ```
  Escape `=`, `;`, `#`, `\` and newlines with `\`.
- **YouTube:** YouTube ignores embedded chapters; it reads timestamps from the video description. "Copy YouTube chapters" puts lines like `0:00 Intro` on the clipboard (`h:mm:ss` when ≥ 1 h).
  - Warn if YouTube's rules are not met: first chapter at 0:00, at least 3 chapters, each at least 10 s.

### 6.14 4K content
Smart mode is where 4K pays off most: copied parts cost nothing extra at 4K. The rules below cover the re-encoded parts, playback and resources.

- **Codec levels:** H.264 at 4K needs level 5.1/5.2; HEVC level 5.1 (5.2 for 60 fps). Heads/tails copy the source level (§6.4). In Full re-encode, choose the level from resolution × frame rate, or let the encoder pick it automatically; never force a level lower than required.
- **Head/tail encoding speed:**
  - Software x264 at 4K with `-preset slow` may run at only a few fps on a laptop. That's fine for camera footage (keyframes ~0.5–2 s apart), but a 4K screen recording with Bandicam-style 10–30 s GOPs could take a minute or more per head.
  - Rule: at ≥ 2160p (or when the estimated head encode time exceeds ~30 s), use `libx264 -preset medium` (`libx265 -preset fast` for HEVC).
  - Advanced option "Fast heads (hardware encoder)": uses the detected hardware encoder for heads, at a slight quality-per-bit cost.
  - Calibrate both on a real 4K sample in spike 0A (SSIM and time per head).
- **10-bit sources** (common for 4K HEVC from phones and cameras):
  - Heads must stay 10-bit (`-pix_fmt yuv420p10le`, `-profile:v main10` for libx265; `high10` for libx264), or they won't be concat-compatible.
  - Check that the bundled ffmpeg's libx264/libx265 supports 10-bit (current builds do).
  - Hardware encoders: NVENC supports HEVC Main10; AMF/VAAPI support depends on the GPU, so the per-job test in §6.8 decides.
- **Full re-encode at 4K:**
  - Software libx265 at 4K is very slow. "Auto" should prefer a working hardware HEVC/H.264 encoder at ≥ 2160p (estimate the ETA from a short test).
  - Resolution presets already include 2160p, and downscaling 4K → 1080p (or 1440p) for smaller files is a first-class option.
  - Downscaling uses `scale=…:flags=lanczos`. Screen-recording text stays sharp when going from 4K to exactly 1080p (2:1).
- **Disk and temp space:** 4K camera footage at 50–150 Mbps makes the copied pieces large (≈ 0.4–1.1 GB per minute). The free-space check (§6.10) must account for this, and the export plan summary should show the estimated temp and output sizes prominently.
- **Probing:** unaffected (index-based); the frame index for a 1-hour 4K60 file ≈ 216k entries, which is fine.
- **Thumbnails and waveform:** extract thumbnails with input seeking + `scale=320:-2` so 4K decode cost stays minimal.
- **HDR (PQ/HLG) 4K:** copy keeps HDR intact. Heads/tails copy the source color tags. Tone mapping to SDR stays out of scope (warning only, as in §6.5).

---

## 7. Playback (mpv) integration

- Create the mpv handle with: `vo=libmpv`, `hwdec=auto-safe` (GL path; mpv falls back to copy-back or software decoding when GPU interop is unavailable; use `auto-copy-safe` for the SW fallback), `keep-open=always`, `idle=yes`, `pause=yes`, `hr-seek=yes`, `hr-seek-framedrop=no`, `osc=no`, `input-default-bindings=no`, `input-vo-keyboard=no`, `audio-display=no`, `demuxer-max-back-bytes=200MiB` (helps frame back-step), `force-window=no`.
- **Render context (GL path, default):**
  - In `eframe::App` creation, take `cc.gl` (`Arc<glow::Context>`) and `cc.get_proc_address`.
  - Create the context with `mpv_render_context_create` and `MPV_RENDER_PARAM_API_TYPE = "opengl"`, plus `MPV_RENDER_PARAM_OPENGL_INIT_PARAMS { get_proc_address }`. The callback wraps eframe's `get_proc_address`.
  - Register `mpv_render_context_set_update_callback`. The callback only sets an atomic "dirty" flag and calls `ctx.request_repaint()`.
  - Keep an app-owned GL **framebuffer + RGBA8 texture** sized to the video widget's physical pixels; recreate it on resize.
  - Register the texture with egui once per (re)creation: `egui_glow::Painter::register_native_texture`, reachable via `frame.gl()`/the glow painter (check the eframe 0.37 API), giving an `egui::TextureId`.
  - In `update()`, when dirty and `mpv_render_context_update() & MPV_RENDER_UPDATE_FRAME`:
    1. call `mpv_render_context_render` with `MPV_RENDER_PARAM_OPENGL_FBO { fbo, w, h }` and `MPV_RENDER_PARAM_FLIP_Y = 0` (flip as needed so the image is upright when shown by egui);
    2. restore GL state egui expects (bind the default framebuffer, reset the viewport);
    3. after painting, optionally call `mpv_render_context_report_swap`.
  - Show the texture with `ui.image((texture_id, size))`, letterboxed in the available rect.
  - Reference: the eframe example `custom_3d_glow` shows raw GL inside egui.
- **SW fallback:** `MPV_RENDER_PARAM_API_TYPE = "sw"`, with `SW_SIZE`/`SW_FORMAT="rgb0"`/`SW_STRIDE`/`SW_POINTER` on a render thread. Convert to `egui::ColorImage` (alpha = 255) and call `TextureHandle::set(image, TextureOptions::LINEAR)`.
- Reference project for ideas: `mpv-engine` (crates.io; toolkit-agnostic libmpv core with GL and SW render helpers; young, Linux-focused).
- **Controls → mpv commands:**
  - play/pause: `cycle pause`
  - seek: `seek <t> absolute+exact`
  - frame step (decided in 0B, §3.5 item 3): forward `set speed 32` + `frame-step`, then `set speed 1`; backward `seek <previous frame pts> absolute+exact` from the frame index (mpv's `frame-back-step` as fallback without an index)
  - keyframe jumps: compute from the frame index, then `seek <K> absolute+exact`
  - observe `time-pos`, `pause`, `eof-reached`, `duration`.
- **Cut preview / project preview via EDL:**
  `loadfile "edl://%<len>%<path>,start=<S>,length=<E-S>;%<len>%<path>,start=…"`, where `%<len>%` is the UTF-8 byte length of the path (this quoting handles commas/semicolons).
  - Map preview time ↔ (clip, segment, source time) using cumulative lengths, so the timeline can show the position.
  - Small hiccups at EDL boundaries are acceptable.
- **Performance gate** (spike 0B), on the author's Windows laptop and one Linux machine (X11 and Wayland):
  - 1080p30 H.264 playing in a ~1280×720 viewport is smooth, with A/V in sync;
  - the UI process uses well under 1 CPU core while playing;
  - frame-step < 100 ms; seek on the 6-hour file < 300 ms; frame-back-step within a 30 s GOP < 1.5 s;
  - the GL path works on NVIDIA (Windows), AMD integrated (Windows and Linux), and under Wayland and X11.
  - **4K:** 4K30 H.264 and 4K60 HEVC (8-bit and 10-bit) play smoothly with hardware decoding on the GL path; frame-step < 200 ms; seek < 500 ms. Check that mpv's `hwdec-current` property shows hardware decoding actually in use, and display it in a debug overlay.
  - If the GL path fails on some configuration, the SW fallback must work there, and it is selectable in Settings (auto-fallback if GL render context creation fails).
    - The SW fallback is not expected to keep up with 4K60 (copy-back of 4K frames plus CPU scaling).
    - In SW mode at ≥ 2160p, set mpv `vd-lavc-skiploopfilter`/`framedrop` defaults or offer a "Preview at reduced quality" toggle, and show a hint that the GL path is recommended for 4K.
  - Everything in `keepcut-core` is independent of this.
- **Large sources in general:** set `demuxer-max-bytes`/`demuxer-max-back-bytes` from a memory budget (e.g. 500 MiB / 300 MiB), not a fixed frame count. Frame back-step at 4K with long GOPs is slower; show a busy indicator rather than blocking.

---

## 8. UI specification

### 8.1 Layout (single window)
```
┌──────────────────────────────────────────────────────────────────────────┐
│ [+ Add clips] [Open] [Save]  [Preview all]      ↶ ↷        [Export…]     │
├────────────────┬─────────────────────────────────────────────────────────┤
│ CLIPS          │                                                         │
│ ┌────────────┐ │                    VIDEO PREVIEW                        │
│ │thumb  name │ │                                                         │
│ │0:42 → 0:31 │ │                                                         │
│ │⚠ re-encode │ │                                                         │
│ └────────────┘ ├─────────────────────────────────────────────────────────┤
│ (drag to       │ ⏮K  ◀|  ▶/❚❚  |▶  K⏭   00:01:23.400 / 00:05:00.000     │
│  reorder,      │ [Set start  I] [Set end  O] [Split  S] [Delete  Del]     │
│  checkbox =    │ [Cut preview P]                         zoom − ▬▬▬ +    │
│  enabled)      ├─────────────────────────────────────────────────────────┤
│                │ TIMELINE: ▓▓▓▓▓░░░░▓▓▓▓▓▓▓▓░░▓▓▓  keyframe ticks, playhead│
│ [Sort by       ├─────────────────────────────────────────────────────────┤
│  recording     │ SEGMENTS: # | start | end | duration | chapter title | ✕ │
│  time]         │                                                         │
├────────────────┴─────────────────────────────────────────────────────────┤
│ 7 clips · kept 00:12:34 · target 1920×1080 H.264 15 fps · 1 clip re-encode│
└──────────────────────────────────────────────────────────────────────────┘
```

### 8.2 Clip list
- Thumbnail (a frame at 10% of the duration, extracted via ffmpeg into the cache dir), file name, original duration → kept duration, badges (⚠ re-encode with tooltip reasons; ⚠ missing file; audio track count if > 1), enable checkbox.
- Drag to reorder. "Sort by recording time" button. New clips are inserted sorted by recording time.
- Selecting a clip loads it into the editor (mpv `loadfile`), keeping its last playhead position.
- Context menu: Remove, Show in folder, Edit chapter title.

### 8.3 Timeline (custom egui widget: `ui.allocate_exact_size` + `ui.painter()`, with input from `Response`/`ctx.input`)
- Full-clip overview, zoomable with Ctrl+wheel around the cursor, pannable with wheel/drag on the scrollbar.
- **Kept segments** are filled bars; gaps are dimmed. The **selected** segment is highlighted.
- **Keyframe ticks**, drawn only when ≥ 3 px apart at the current zoom.
- **Segment start-edge colour:** green = on a keyframe (pure copy), amber = head will be re-encoded. Tooltip: "re-encodes 4.2 s up to the next keyframe".
- **Playhead**; click or drag on empty area to seek; drag segment edges to trim. Edges snap to frames, and to keyframes when holding Shift.
- Later (phase 3): an audio waveform lane, computed from `ffmpeg -i src -map 0:a:0 -ac 1 -ar 1000 -f s16le -` (peak per pixel bucket).

### 8.4 Segment editing model (Bandicut-like; keep = segments, removed = gaps)
- New clip ⇒ one segment covering the whole clip.
- **I / Set start:** if the playhead is inside a segment, move that segment's start to the playhead. Otherwise create a new segment from the playhead to the next segment's start (or the clip end).
- **O / Set end:** if the playhead is inside a segment, move its end to the playhead. Otherwise create a new segment from the previous segment's end (or the clip start) to the playhead.
- **S / Split:** split the segment under the playhead into two.
- **Del:** delete the selected segment (the footage becomes a gap).
- Typical "cut out the middle": `S` at A, `S` at B, select the middle part, `Del`.
- Segments within a clip are always sorted by time and never overlap. Only clips are reordered, not segments.

### 8.5 Keyboard shortcuts (shown in tooltips)
| Key | Action |
|---|---|
| Space | Play/pause |
| ← / → | Previous/next frame |
| Shift+← / Shift+→ | Previous/next keyframe |
| Ctrl+← / Ctrl+→ | −5 s / +5 s |
| Home / End | Clip start/end |
| I / O | Set start / set end |
| S | Split at playhead |
| Del | Delete selected segment |
| P | Toggle cut preview (play kept segments only) |
| PgUp / PgDn | Previous/next clip |
| Ctrl+Z / Ctrl+Y (Ctrl+Shift+Z) | Undo/redo |
| Ctrl+S / Ctrl+O | Save/open project |
| Ctrl+E | Export |

### 8.6 Export dialog
- **Mode:** Single joined file / One file per source clip / One file per segment.
- **Output folder:** (•) Same as source ( ) Custom [path] [Browse…] (remembered). **File name** (joined mode).
- **Target format** (joined mode): dropdown of signatures present, default as in §6.2.
- **Encoding mode:** (•) Smart — lossless where possible ( ) Full re-encode.
  - Smart: **Encoder for re-encoded parts** (whole mismatched clips; heads always use software x264/x265): Auto / detected working encoders. **Quality:** "Match source" (default) / advanced CRF/CQ value.
  - Full re-encode: Codec (H.264/HEVC), Encoder (Auto / detected), Quality (Match source / CRF-CQ / bitrate), Resolution (Same / 2160p / 1440p / 1080p / 720p / custom), Frame rate (Keep timing / 15 / 24 / 25 / 30 / 50 / 60).
- **Audio:** Mix all tracks / First track only; AAC bitrate (default 192k).
- **Loudness:** Off / Whole video / Per clip / Per segment; target LUFS (−14).
- **Chapters:** None / Per clip / Per segment; [Copy YouTube chapters].
- **Plan summary** (computed live by `plan.rs`):
  - per clip: "copied; 3 heads re-encoded (7.4 s total)", or "fully re-encoded: 1280×720 → 1920×1080 (upscaled)";
  - estimated output size and temp space;
  - warnings (HDR, soft-key mismatches).
- [Export] → progress dialog: overall bar, current step text, elapsed/ETA, [Cancel], expandable log.
- Finish: verification result, [Open folder], [Play], [Copy YouTube chapters].

### 8.7 Other dialogs
- **Settings:** ffmpeg/ffprobe path (default: bundled, then PATH), re-detect encoders, temp location, default loudness target, full decode check toggle, theme (light/dark/system).
- **Recovery:** on start, if an autosave is newer than the last explicit save (or there is no saved file), offer Restore / Discard.
- **Missing source:** relink (browse) or remove.

---

## 9. Projects, autosave, undo, settings
- **Project file:** JSON, extension `.keepcut`, `"version": 1`. Paths are absolute; source size and mtime are stored to detect changes (if changed: re-probe and clamp segments to the new duration, then warn).
- **Autosave:**
  - Saved after every change (debounced 2 s) to `<data dir>/autosave/<project id>.keepcut`, written atomically (temp file + rename).
  - Deleted on clean exit after explicit save or discard.
  - Works for unsaved ("Untitled") sessions too.
- **Undo/redo:** snapshot-based. Push a clone of `(clips order, segments, enabled flags, chapter titles)` before each edit; a drag operation is pushed once on release. Cap of 200. Playhead/selection changes are not undoable.
- **Settings:** `<config dir>/config.toml` (via `directories::ProjectDirs("", "", "Keepcut")`): last custom output folder, export defaults, encoder preference, window size, theme.
- **Caches** (`<cache dir>`): probe results and frame indexes (keyed by path+size+mtime), thumbnails, encoder detection results.
- **Logs:** `<data dir>/logs/keepcut-YYYY-MM-DD.log`, keeping 7 days.

---

## 10. Packaging and distribution

### Windows
- Bundle `ffmpeg.exe` + `ffprobe.exe`: GPL builds with nvenc/amf/qsv, from gyan.dev or BtbN. Check the build's minimum NVIDIA driver; a release-branch build may support older drivers than the latest one.
- Bundle `libmpv-2.dll` from the official mpv release (`libmpv-v<ver>-x86_64-w64-mingw32-lgpl.zip`; §2). The package's import library is MinGW-style (`libmpv.dll.a`); for the MSVC toolchain, `fetch-mpv.ps1` generates `mpv.lib` (§10a). Ship the zip's `LICENSE.LGPL` alongside. Note the DLL is 126 MB unpacked (it bundles FFmpeg and its dependencies); it compresses well in the installer.
- Portable `.zip` and an installer (Inno Setup or `cargo-packager`).
- Unsigned builds trigger SmartScreen ("More info → Run anyway"); code signing is optional later.

### Linux
- **Flatpak** (primary; implemented in Phase 4, see §11):
  - runtime `org.freedesktop.Platform` (current version), SDK extension `rust-stable`;
  - build modules for ffmpeg (with x264, x265, nv-codec-headers, VAAPI) and mpv/libmpv (use Flathub's `io.mpv.Mpv` manifest as reference);
  - cargo sources vendored via `flatpak-cargo-generator`.
- Distribute as a `.flatpak` bundle file or a self-hosted repo; Flathub later.
- Install notes: Ubuntu/Kubuntu/Debian `sudo apt install flatpak`; Fedora has it; AlmaLinux `sudo dnf install flatpak`.
- Also document building from source: `libmpv-dev`/`mpv-libs-devel`, ffmpeg in PATH. AppImage is optional later.

### macOS (best effort)
CI build with Homebrew `mpv` dylibs bundled (`dylibbundler`) and an ffmpeg static build; unsigned (right-click → Open).

### CI
GitHub Actions matrix (windows-latest, ubuntu-latest, macos-latest): `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, integration tests on synthetic media, packaging artifacts on tags.

---

## 10a. Development environment

### Windows (the author's machine; checked 2026-11-17)
**Status:** ready.
- Installed and verified: Rust 1.100.0 (`stable-x86_64-pc-windows-msvc`, with clippy and rustfmt); MSVC build tools + Windows SDK (via rustup-init; a hello-world builds and runs); git 2.56; ffmpeg/ffprobe 9.0.2 (Chocolatey, gyan.dev essentials); 7-Zip; scoop; VS Code/Zed/Cursor.
- Working ffmpeg encoders: `h264_nvenc`, `hevc_nvenc`, `av1_nvenc`, `h264_qsv`, `libx264`, `libx265`. AMF is not available (no AMD GPU).
- **Still to do:** libmpv (step 3, scripted by the implementer).

1. **Rust + MSVC (done; how it was installed):** run `rustup-init.exe` from https://rust-lang.org/learn/get-started/.
   - When it reports that MSVC is missing, choose **"Quick install via the Visual Studio Community installer"**. That installs the C++ build tools and the Windows SDK (several GB).
   - Then accept the default installation (stable, `x86_64-pc-windows-msvc`, default profile with `cargo`, `clippy`, `rustfmt`).
   - Verify in a **new** terminal: `rustc --version`, `cargo --version`, and a smoke test `cargo new hello && cd hello && cargo run`.
   - Equivalent manual route: `winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--passive --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"` then `winget install --id Rustlang.Rustup`.
   - Alternative considered: the GNU toolchain (`x86_64-pc-windows-gnu` via MSYS2). It's smaller and links libmpv's MinGW import library directly, but it's less common on Windows. Not chosen.
2. **libmpv for development** (the implementer scripts this; nothing to install system-wide):
   - **Done (0B):** `packaging/windows/fetch-mpv.ps1 [-Tag v0.42.0] [-Variant lgpl|gpl]` downloads the official `libmpv-v<ver>-x86_64-w64-mingw32-<variant>.zip` (+ `.asc`) from github.com/mpv-player/mpv releases, unzips it into `third_party/mpv/` (gitignored), and builds `mpv.lib` with `dumpbin`/`lib` found via `vswhere`. `crates/keepcut-player/build.rs` adds the link path and copies `libmpv-2.dll` next to the executable (it finds `target/<profile>` by name; cargo 1.100 nests build-script output one level deeper than older versions).
   - Create an MSVC import library from the DLL: `dumpbin /exports libmpv-2.dll` → `mpv.def` → `lib /def:mpv.def /name:libmpv-2.dll /out:mpv.lib /machine:x64`, unless the package already contains a usable `.lib`.
   - Point the linker at it in `.cargo/config.toml`: `[target.x86_64-pc-windows-msvc] rustflags = ["-L", "native=third_party/mpv"]`. Check the `libmpv2-sys` README for the exact library name it links (`mpv` vs `libmpv`).
   - Copy `libmpv-2.dll` next to the built executable (from a `build.rs` of the GUI crate, or an `xtask`).
3. **ffmpeg:** the installed 9.0.2 is fine for development. The app looks for bundled binaries first, then `PATH`.
4. **Optional:**
   - players to check seams: VLC (`winget install --id VideoLAN.VLC`) and mpv (`scoop bucket add extras; scoop install mpv`);
   - an editor with rust-analyzer (VS Code extension, or built into Zed);
   - later: `cargo install cargo-insta cargo-nextest`.

### Linux (for colleagues / CI)
- Rust: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`.
- Ubuntu/Debian/Kubuntu: `sudo apt install build-essential pkg-config libmpv-dev ffmpeg libgl1-mesa-dev libegl1-mesa-dev libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev`.
- Fedora: `sudo dnf install gcc pkgconf-pkg-config mpv-libs-devel libxkbcommon-devel wayland-devel libX11-devel libXcursor-devel libXrandr-devel libXi-devel mesa-libGL-devel mesa-libEGL-devel`, plus full ffmpeg from RPM Fusion (Fedora's `ffmpeg-free` lacks H.264/HEVC).
- AlmaLinux: like Fedora, with EPEL + RPM Fusion; or build inside the Flatpak SDK.

---

## 11. Phases and acceptance criteria

### Phase 0 — Spikes (de-risk before building UI)
**0A Pipeline spike** (`keepcut-core` + `keepcut-cli`, no GUI). Input: a JSON project listing clips + segments.
- Implement probe, frame index, smart-cut planning (§6.3–6.4), the Full re-encode planning (§6.0), the audio pipeline (§6.6), final mux (§6.7), verification (§6.11), encoder detection (§6.8).
- Confirm the MPEG-TS intermediate strategy (§6.7) for H.264 as well (it's already required for HEVC), including the player check.
- Calibrate encoder presets (SSIM ≥ 0.98, bitrate ≤ ~1.2× source).
- Test the timescale-remux fix (§6.2 open item).
- Test sources with B-frames: the DaVinci samples (`has_b_frames=2`).
- **Accept when**, in **both encoding modes**, on:
  - the Bandicam NVENC samples (3 segments from 3 compatible clips, starts not on keyframes, plus one segment from the 720p clip);
  - the DaVinci samples;
  - the Bandicam QSV H.264 and HEVC samples (including a join of two cut pieces from each; use longer QSV recordings once the author provides them):
  - output video duration = Σ segments ± 1 frame;
  - |A − V| ≤ 30 ms;
  - zero decode errors (full decode);
  - no visible glitch at seams in mpv, VLC and a browser.
- **4K:** also run 0A on a 4K sample: synthetic (see §12) and at least one real 4K clip from the author.
  - Record head encode times and SSIM for `slow` vs `medium` presets and for hardware heads.
  - Record Full re-encode speed with each working hardware encoder.
  - Use the numbers to finalize the §6.14 rules.

**0A status (2026-11-17):** implemented; all acceptance runs pass on the available samples in both modes (§3.3). Still open:
- player check of the outputs in mpv, VLC, a browser, Windows Media Player, and one unlisted YouTube upload (outputs from `keepcut-cli export … --out-dir samples/out`);
- longer Bandicam QSV recordings (H.264 + HEVC, ≥ 60 s with activity) and a real 4K clip from the author;
- hardware heads ("Fast heads", §6.14): hevc_nvenc/h264_nvenc heads, their speed, SSIM and seam compatibility (hardware whole-segment re-encodes are verified, Phase 2);
- loudness (§6.9) and chapters: done in Phase 3 (see there);
- free-space check (§6.10) and encoder re-test with the real target parameters (§6.8): implemented in Phase 2.

**0B Player spike** (minimal eframe/glow app): open a file; play/pause; seek; frame step both ways; keyframe jump; EDL cut preview; the GL render path and the SW fallback.
- **Accept** on the performance gate in §7. Record the results in this file.
- **0B status (2026-11-18):** implemented in `crates/keepcut-player`; results in §3.5. Passes on the author's Windows laptop except the 4K60 HEVC seek target (531 ms avg vs 500 ms). Still open: the AMD laptop (Windows), Linux X11 and Wayland, and the 4K seek investigation.

### Phase 1 — MVP
- Workspace and app skeleton, config, logging.
- Add clips (dialog + drag&drop), probing in the background, clip list with sort and reorder.
- Editor: video, transport controls, timeline with keyframes and segments, segment editing (§8.4), segment list, keyboard shortcuts, undo/redo.
- Cut preview and Preview all.
- Export: **joined mode**, **Smart** encoding mode (smart cut, re-encode of mismatched clips with upscaling), **Full re-encode** mode with default settings (H.264, match-source quality, same resolution, keep timing, Auto encoder), audio pipeline (no loudness), progress/cancel, temp cleanup, verification, output folder default/browse/remember, naming rules.
- **Done when** the author can do their Bandicam workflow end-to-end on Windows.
- **Status (2026-11-18, first pass, `crates/keepcut`):** all items above implemented; awaiting the author's hands-on test.
  - Layout per §8.1: toolbar (Add clips, Preview all, Undo/Redo, Export), clip list (enable checkbox, original → kept duration, resolution, audio-track count, "re-encode" badge with reasons, "can't read" badge, drag to reorder, context menu Remove / Show in folder, Sort by recording time; new clips are placed by recording time once probed), video (GL path with SW fallback; render path from config), editor (transport, I/O/S/Del, cut preview, timeline per §8.3, segment list with per-segment "keyframe: copied" / "re-encodes N s"), status bar (clips, kept total, target format, re-encode count, probing, render path/hwdec).
  - Segment editing logic is in `keepcut-core/src/edit.rs`, undo in `history.rs` (snapshot of the clip list, cap 200; a drag is one step). Edge drags snap to frames (Shift: keyframes) and keep ≥ 1 frame per segment.
  - Export dialog (§8.6) with the live plan summary; progress window (bar, step, elapsed/ETA, Cancel, log); finish window (verification per output, Show in folder, Play, Copy log, "Re-export with Full re-encode" when Smart verification fails). **Per-clip and per-segment modes are already exposed** since `keepcut-core` supports them (Phase 2 item done early).
  - Config `<config dir>/config.toml` (output location + remembered custom folder, encoding and output mode, render path); logs `<data dir>/logs/keepcut.YYYY-MM-DD.log`, 7 days.
  - **Test automation:** `keepcut --script FILE [CLIPS…]` runs commands through the same code paths as the UI (select, seek, step, set-start, split, trim, undo, previews, export, cancel, print-state, screenshot of the app's own frame). Scripted runs never save the config. Verified overnight: edits/undo/redo, previews with time mapping, trims with keyframe snapping, an unreadable file, Smart and Full joined exports of the edited Bandicam set (60.933 s exact, A−V 0, 885/885 frames), cancel (no partial output).
  - egui's default proportional font lacks ⚠ ✕ ✓ → ▶ ◀; UI strings avoid them (the monospace font has →).
  - Not yet: thumbnails, project save/open, autosave (Phase 3); frame step during a preview uses mpv's own stepping.
  - **Review fixes (independent code review, 2026-11-18):**
    - Probe results are cached per *file path* (not per clip). Removing, undoing or redoing a clip can no longer leave a clip stuck "reading…" or block Export.
    - Unprobed clips hold `segments: None` ("whole clip"), filled in on probe and after undo/redo. Undo can't empty clips any more.
    - Adding files is one undo step.
    - The enable checkbox sits outside the draggable card (inside it, egui gave the click to the card).
    - Timeline zoom uses `zoom_delta` (egui turns Ctrl+wheel into zoom). Edge grabs hit-test at `press_origin`; at a split point the pointer side picks the edge.
    - "Preview all" selects the clip that is playing, so I/O/S edit the right clip at the right time; edits and leaving a preview keep the position.
    - Steps and keyframe jumps compute from a pending seek's target. Seeks clamp at 0. Re-selecting the current clip and finished probes no longer reset the playhead.
    - Shortcuts are blocked while dragging; a drag records its own clip.
    - Export: a crashed export thread ends with an error instead of hanging; closing the window cancels a running export and waits up to 5 s for cleanup.
    - The joined file name follows the first clip until edited. "Custom" without a folder blocks Export. The config is saved once, not per keystroke.

### Phase 2 — Export completeness
- Per-clip and per-segment export modes.
- Full re-encode output options: HEVC, resolution presets/custom, constant frame rate, custom CRF/CQ or bitrate. "Re-export with Full re-encode" offered when a Smart export fails verification.
- Encoder selection UI; hardware encoders for long re-encodes (NVENC, AMF, VAAPI, QSV).
- Export plan summary; soft-key warnings; free-space check; command log panel.
- HEVC sources end-to-end: libx265 heads, 10-bit, HEVC Full re-encode. The MPEG-TS intermediate route already exists from Phase 1, so copying HEVC is safe from the start.
- **Status (2026-11-18, first pass, not yet committed):** all items above implemented.
  - **Export dialog:**
    - target format dropdown (joined, when formats differ; stored as the format itself, not an index, so trims can't silently switch it);
    - Smart: encoder and quality for whole re-encoded clips;
    - Full: codec, encoder, quality (match source / constant quality / bitrate), size (same / 2160p / 1440p / 1080p / 720p / custom), frame rate (keep timing / 15–60), content (detect / screen / camera);
    - audio (mix all / first track, AAC 128–320 kbps);
    - estimated output size, temp space and free space;
    - "Encoders on this computer" with the reason each unavailable one failed.
    - All choices are remembered in the config.
  - **Resolution presets** set the *short* side (portrait stays portrait; 1366×768 at 720p → 1280×720), even dimensions. `model::Resolution`.
  - **Full re-encode:** one `VideoEncode` per output, from the output's reference clip (joined: the target; separate files: the clip itself). So every piece of a joined file has the same pixel format, profile and GOP. Before, a 10-bit clip in an 8-bit join got its own 10-bit pieces.
  - **Auto encoder** (`plan::pick_encoder`): software for screen recordings below 2160p; otherwise the first working hardware encoder for the codec, else software. An explicit choice is used only if it works and matches the codec.
  - **Encoder re-test (§6.8):** before an export, every distinct hardware encode is test-encoded with its exact arguments at the output size (`encoders::test_encode`). Failures switch to libx264/libx265, with a note in the finish window. If a hardware encode still fails on real content, that piece is retried once in software.
  - **Unavailable-encoder reasons** come from ffmpeg's first message, e.g. "DLL amfrt64.dll failed to open" or "Current codec type is unsupported" (av1_qsv on this laptop).
  - **Free space (§6.10):**
    - `OutputPlan.est_bytes` / `est_temp_bytes`: copies cost their source bytes; re-encodes cost the source rate × pixel ratio × codec efficiency; a set bitrate is used exactly.
    - Measured against real outputs, the estimates were within ~2× (10-bit HEVC on Quick Sync: 4× under). So the check refuses only when 1.5× the estimate doesn't fit.
    - Too little OS temp space moves the temp dir to `<output folder>/.keepcut-tmp-*`.
    - Each output's intermediates are deleted as soon as it is done, so peak temp = the largest output.
  - **Command log panel** (toolbar "Log"): every ffmpeg command and note of the session, shell-quoted, with Copy all. Commands also go to the log file at info level.
  - **CLI:** `--codec --encoder --res --fps --crf --bitrate`. Script commands: `set KEY VALUE`, `wait-encoders`, `log-panel`.
  - **Verified on this laptop:** all with exact duration, A−V 0 and every frame present.

    | Run | Encoder | Result |
    |---|---|---|
    | Full HEVC Auto, 4K Bandicam | picked hevc_nvenc | 16.467 s |
    | Full H.264, 4K | h264_qsv | 15.333 s |
    | Full HEVC, 4K → 1080p | hevc_qsv | 16.467 s |
    | Full HEVC 4K60 10-bit (stays Main10) | hevc_nvenc | 10.100 s |
    | Full HEVC 4K60 10-bit | hevc_qsv | 10.100 s |
    | NVENC set at 720p / 30 fps CFR | h264_nvenc | 89.667 s, 2690 frames |
    | NVENC set, 1500 kbps | hevc_nvenc | 89.667 s |
    | NVENC set, software HEVC | libx265 | 89.667 s |
    | Smart, mixed H.264/HEVC/AV1 4K, mismatched clips | hevc_nvenc | 16.000 s |
    | Per-segment Full | h264_qsv | 2 files |

    Through the GUI script:
    - Full HEVC 720p/30 fps CRF 26 (219.867 s, 6596 frames);
    - target override to 720p;
    - per-clip Smart (3 files, all copied).

    Synthetic tests added: Full HEVC 10-bit with 4:3 + 16:9 sources, downscale and 25 fps; per-clip / per-segment naming and "(2)" suffixes; resolution presets; Auto encoder choice; refusing when out of space.
  - **Not done / open:**
    - "Fast heads" with hardware encoders (§6.14). Heads/tails still always use libx264/libx265.
    - AMF and VAAPI are untested (no such hardware here; AMD laptop pending).
    - Size estimates for heavily static screen content at a forced frame rate are rough.

### Phase 3 — Comfort features
- Loudness normalization (§6.9); chapters + YouTube block (§6.13).
- Project save/open, autosave + recovery (§9); missing-file relink.
- Waveform lane; clip thumbnails; Settings dialog.
- **Status (2026-11-22, first pass; committed as "Phase 3: comfort features"):** all items above implemented.
  - **Loudness (§6.9, `exec::loudness_gains`):**
    - Measured with `ebur128` on the FLAC pieces, which runs at ~500× real time (a concat list per unit). This replaces the planned `loudnorm` analysis, which gives the same BS.1770 integrated loudness more slowly.
    - Per clip: all of that clip's pieces in the output. Per segment: each segment, except segments under 3 s, which use their clip's value. Whole: the output.
    - Gains are clamped to ±20 dB; units at −70 LUFS or below (silence) get 0 dB.
    - **Applied in the final mux, not as new FLAC pieces.** `plan::loudness_filter` builds `asetnsamples=n=64,volume='Σ gain·window(t)':eval=frame,alimiter=limit=0.891:level=disabled:latency=1`, read from a file with `-/af` because it can be long.
      - The gain runs in float, so nothing clips before the limiter. Re-encoded s16/s32 FLAC pieces would clip at +gain.
      - The gain switches on 1.3 ms frames, half a frame early.
    - **Limiter fix:** without `latency=1`, `alimiter` delays the audio by its 5 ms look-ahead (measured: 239 samples). The Phase 1 mux had this latent bug.
  - **Chapters (§6.13):**
    - Per-clip chapters use the clip's chapter title (was always the file stem) and merge that clip's consecutive segments.
    - The default segment title is just the clip title when the clip has a single kept segment, else "Title – N" (`chapters::default_segment_title`).
    - Titles are stored as typed and trimmed by the planner.
  - **GUI:**
    - Export dialog: Loudness (Off / Whole / Per clip / Per segment + target) and Chapters (None / Per clip / Per segment). "Copy YouTube chapters" with YouTube's rules checked, in the dialog and per output in the finish window.
    - Clip context menu "Chapter title…" (dialog); segment list "Chapter title" column (inline, one undo step per edit).
    - All of these are remembered in the config.
  - **Projects (§9, core `projfile`, GUI `project.rs`):**
    - `.keepcut` JSON, version 1, atomic writes, refuses newer versions.
    - Stores each clip's path relative to the project folder too, so a folder moved together with its project opens unchanged.
    - Project menu: New / Open / Save / Save as (Ctrl+N/O/S/Shift+S). A `.keepcut` on the command line or dropped on the window opens it. The window title shows the name and `*`.
    - Unsaved-changes prompt (Save / Don't save / Cancel) on New, Open and closing the window. An untitled session that was exported after its last edit closes without asking.
    - **Autosave:** 2 s after the last change, to `<data dir>/autosave/<project id>.keepcut`. Deleted on clean exit and on save.
    - **Recovery:** at start (no files given), the newest autosave that is newer than its project file is offered (Restore / Discard); stale ones are deleted.
    - **Changed sources:** a size/date that differs from the project → segments fitted to the new length (`edit::normalize`), a "changed" badge, and a status message.
    - **Missing sources:** a "missing file" badge, a relink dialog on open (Locate… / Remove) and "Locate…" in the clip menu. Locating one file also relinks the other missing clips found in that folder. Relinking is undoable.
    - **Fix found while testing:** probing re-sorted every clip by recording time, so opening a project could reorder it. Now only newly added clips are placed by recording time.
  - **Thumbnails and waveform (`core::assets`, GUI `media::AssetService`):**
    - Two background workers, cached in `<cache dir>/{thumbs,waves}/` by path + size + date.
    - Thumbnail: 192×108 RGBA, letterboxed, from the keyframe at or before 10% (`-noaccurate_seek`, one frame decoded even with 30 s GOPs).
    - Waveform: all audio tracks mixed to mono at 8 kHz, peak per 10 ms. Drawn in a lane under the timeline on a −48…0 dB scale, brighter inside kept segments. The selected clip goes first.
  - **Settings dialog (§8.7):**
    - ffmpeg folder (or automatic; the version is shown; unreadable clips are re-read after a change).
    - Re-detect encoders, temp folder (with free space), default loudness target, full decode check, theme (System / Light / Dark), video display path (after a restart), and buttons that open the log and settings folders.
  - **CLI:** `--loudness off|whole|per-clip|per-segment --lufs N --chapters none|per-clip|per-segment`; prints the YouTube chapter lines.
  - **Script commands:** `set loudness|lufs|chapters`, `clip-title`, `seg-title`, `print-youtube`, `save`, `open`, `new`, `relink`, `settings`, `show-save-prompt`, `close-dialogs`. Scripted runs never autosave, prompt or offer recovery.
  - **Verified:**
    - Unit tests: ebur128 parsing, gain rule, filter builder, per-clip chapter merging, project round trip and moved folder, newer-version refusal, peaks.
    - Pipeline tests: loudness per clip + chapters (each clip within 1 LU of −16, chapter times and titles); thumbnail and waveform on synthetic media.
    - Corpus (bandicut.json, per segment −16): gains +6.4 / +0.2 / +6.6 dB, Verified, A−V 0.
    - GUI script (synthetic clips):
      - Per-clip loudness: −43.8 → −23.8 (clamped at +20 dB), −10.4 → −16.0, −29.3 → −16.0.
      - Chapters "Intro / Main match / Outro" at 0 / 8 / 23 s; export 31.000 s, 930/930 frames, A−V 0.
      - Save, dirty after an edit, clean again after undo.
      - Missing + changed files after moving and replacing sources; relinking one found the other.
      - Config unchanged; no autosave written.
  - **Not done / open:**
    - Autosave and recovery are untested by automation (scripted runs disable them by design); they need a hands-on check, e.g. kill Keepcut after an edit and start it again.
    - Several Keepcut instances share the autosave folder, so a second instance could offer the first one's autosave.
    - Seeking right after selecting a clip can fail while mpv is still loading the file ("Raw(-12)"); this predates Phase 3.

### Phase 4 — Packaging
- Windows zip + installer; Linux Flatpak; macOS best-effort; CI release workflow; README with install instructions per distro.
- **Status (2026-11-22, first pass, not yet committed):**
  - **App icon:** `packaging/icons/keepcut.svg` (a clip with a removed part, joined below), rendered with Inkscape to PNGs and a multi-size `keepcut.ico`. Embedded in `keepcut.exe` with version info (`crates/keepcut/build.rs`, `winresource`) and set as the window icon; the window's app id is `io.github.borisburger.Keepcut` (Wayland/X11 matching with the .desktop file).
  - **Windows** (`packaging/windows/`):
    - `fetch-ffmpeg.ps1`: gyan.dev essentials 9.1 from the GitHub mirror `GyanD/codexffmpeg`, SHA-256 pinned (matches gyan.dev's published hash).
    - `package.ps1`: release build, staging (keepcut.exe, keepcut-cli.exe, libmpv-2.dll, ffmpeg.exe, ffprobe.exe, `licenses/`, README.txt with source locations), portable zip, then the installer.
    - `keepcut.iss` (Inno Setup 7.1): per-user install by default (no admin; "all users" offered), Start menu entry, optional desktop icon, `.keepcut` association (task, on by default), "Open with" for mp4/mov/mkv, uninstaller.
    - Sizes: unpacked ~360 MB (libmpv 126 MB, ffmpeg 111 MB, ffprobe 110 MB: three static copies of FFmpeg). Zip 134 MB. **Installer 60 MB** with `LZMADictionarySize=256 MB`, which lets LZMA store the nearly identical ffprobe.exe almost for free (98 MB with the default 64 MB dictionary).
    - Verified: silent per-user install into a scratch folder, scripted run of the installed app (bundled ffmpeg 9.1 used, hardware encoders detected, export verified 180/180 frames), silent uninstall removes files, registry keys and the Start menu entry.
    - mpv's "Clang" Windows zip is the mpv.exe player only (no libmpv), so it isn't an option for embedding.
  - **Linux Flatpak** (`packaging/linux/`): manifest `io.github.borisburger.Keepcut.yml` on Freedesktop 26.08 + rust-stable; builds nv-codec-headers, x264, x265 4.2, ffmpeg 9.1 (GPL, NVENC/NVDEC, VA-API), libplacebo 7.387 (OpenGL only), libass 0.17.5, libmpv 0.42.0; Keepcut fetched crates with network during the build (Flathub would need `flatpak-cargo-generator`). Permissions: wayland/x11, pulseaudio, `--device=all` (NVENC needs /dev/nvidia*), `--filesystem=host`. Desktop file, AppStream metainfo, MIME type `application/x-keepcut`, hicolor icons. `build-flatpak.sh` builds outside the source tree and exports `dist/Keepcut-<ver>-linux-x86_64.flatpak`; with flatpak-builder < 1.3 (AlmaLinux 9) it turns off appstream-compose, which current SDKs no longer ship.
    - Fix found while building: libplacebo and mpv installed to `/app/lib64` without `--libdir=lib`.
    - Fix found while testing: libmpv without Lua has no `osc` option, so setting it failed and the player couldn't start. `osc` is now optional, and a failing mpv option is logged by name.
    - Built in WSL AlmaLinux 9 (flatpak 1.12.9, flatpak-builder 1.2.3): bundle 20 MB. Verified inside the sandbox (`flatpak build`): `keepcut-cli` runs, ffmpeg 9.1 has libx264/libx265/NVENC/VA-API, `keepcut` links `/app/lib/libmpv.so.2`, mpv starts, encoder detection works, and a smart-cut CLI export with loudness normalization verifies (3.400 s, 102/102 frames, A−V 0).
    - **Not verified:** the GUI on a real Linux desktop. Under WSLg the sandboxed app loses its Wayland connection within milliseconds ("Broken pipe"), and WSLg's X socket isn't in `/tmp/.X11-unix`; this looks WSLg-specific. Needs a test on a Linux machine (Ubuntu/Fedora/AlmaLinux, X11 and Wayland; NVIDIA and Intel/AMD).
  - **macOS** (`packaging/macos/package.sh`, best effort, CI only): Homebrew mpv + ffmpeg, `dylibbundler` into `Keepcut.app/Contents/Frameworks`, icns, Info.plist with document types, ad-hoc signature, zip. Untested here.
  - **CI** (`.github/workflows/`): `ci.yml` on push/PR: Linux (fmt, clippy -D warnings, tests with a BtbN static ffmpeg 9.0, because Ubuntu's 6.1 lacks `-/af`), Windows (clippy, tests with the fetched libmpv/ffmpeg), macOS (tests, allowed to fail). `release.yml` on `v*` tags: Windows zip + installer, Flatpak (flathub-infra container + flatpak-builder action), macOS zip (allowed to fail), then a **draft** GitHub release with all files.
  - `cargo fmt` run over the workspace (the code wasn't rustfmt-clean before; CI now checks it).
  - **README.md:** features, install per platform and distro, build from source, licenses.
  - **Not done / open:**
    - The CI and release workflows haven't run yet (nothing pushed).
    - Size: a slim libmpv built against shared FFmpeg DLLs (also used by ffmpeg/ffprobe) would leave one FFmpeg in the package (installer maybe ~30–35 MB), at the cost of building mpv for Windows in CI.
    - Code signing (Windows), notarization (macOS), Flathub submission.

---

## 12. Testing strategy
- **Unit tests** (`keepcut-core`): time formatting/parsing; frame snapping; compat/target selection; **`plan.rs` snapshot tests** (probe fixtures in JSON → expected list of jobs/ffmpeg args, e.g. with `insta`); chapter escaping; YouTube block rules; ffmetadata; concat list escaping (paths with `'`, spaces, Unicode); undo/redo; project (de)serialization and migration.
- **Integration tests** (need ffmpeg on PATH; run in CI). Generate synthetic media in the test with ffmpeg:
  - CFR H.264 without B-frames: `-f lavfi -i testsrc2=s=1920x1080:r=15 -f lavfi -i sine=f=440:r=48000 -c:v libx264 -bf 0 -g 150 -c:a aac`;
  - a VFR variant (e.g. `-vf mpdecimate` with `-fps_mode vfr`);
  - a B-frame variant (`-bf 2`);
  - a 720p variant, an HEVC variant, a no-audio variant, a two-audio-track variant;
  - **4K variants:** 3840×2160 H.264 30 fps 8-bit, and HEVC 60 fps 10-bit (`-pix_fmt yuv420p10le`). Keep them short (5–10 s) and generate them with fast presets so CI stays quick.
  - Assert the verification criteria (§6.11) and frame counts.
- **Local corpus tests** (ignored by default, `cargo test -- --ignored`): run on `samples/`.
- **Manual QA checklist** per release: playback in mpv/VLC/browser/Windows player; one YouTube unlisted upload when the muxing strategy changes.

---

## 13. Risks and open questions
1. mpv GL render inside eframe: GL state interplay with egui_glow, Wayland/X11, AMD/NVIDIA drivers, hwdec interop (gate in 0B; SW fallback).
2. Parameter-set handling across re-encoded and copied pieces. Decided: MPEG-TS intermediates (§6.7). Confirm for H.264 in 0A; also confirm VFR timestamps survive the 90 kHz TS clock.
3. ~~Open-GOP / B-frame phone footage~~ **Resolved in 0A** (§3.3 item 2, §6.3 item 4): leading pictures are detected from the frame index, dropped at the copy start and re-encoded at the copy end. Still to confirm on a real phone HEVC clip, and on H.264 open-GOP sources.
4. HDR phone footage: SW render shows it un-tonemapped (washed-out). Acceptable for preview; out of scope for export conversion in v1.
5. Bandicam's two-track audio: confirm whether "mic + desktop" produces two tracks; the default "mix all tracks" covers both cases.
6. NVENC driver requirements of the bundled ffmpeg build; VAAPI H.264 encoding on Fedora/Alma.
7. mpv SW render API availability in older distro libmpv (moot with Flatpak).
8. Encoder-specific bitstream quirks (e.g. Bandicam QSV HEVC's malformed `hvcC`) can break stream copy, bitstream filters (`hevc_metadata`) or concat.
   - Keep a sample per Bandicam encoder (NVENC, QSV, AMF; H.264 and HEVC) in the local corpus and run the full 0A acceptance on each.
   - Bandicam's NVENC could not be enabled on the author's hybrid laptop, so new NVENC samples may have to come from another machine. The 2025 NVENC H.264 samples exist.
9. 4K: head encode time with long GOPs; 10-bit support in hardware encoders and in the bundled x264/x265; smooth 4K playback relies on hardware decoding. 10-bit **H.264** (High10) has almost no hardware decode support, so it plays with software decoding and may stutter at 4K.
10. **Missing sample media — ask the author for:**
    - longer (≥ 60 s, with on-screen activity) Bandicam QSV recordings in H.264 and HEVC;
    - a real 3840×2160 clip (screen recording and/or camera);
    - a Bandicam recording with mic + desktop sound;
    - ideally one phone original (HEVC, possibly HDR/VFR).
11. Ownership: if Keepcut is developed on work time or equipment, confirm with the employer that it may be published as open source under GPL before making the repository public.

---

## Appendix A — Sample corpus (currently in the project root folder, to be moved to `samples/`)

**Bandicam 8.0.1 (GDI capture, Nvidia NVENC):** all H.264 High, yuv420p, `has_b_frames=0`, r_frame_rate 15/1 (VFR, avg ≈ 13.9–15), time_base 1/15000, AAC LC stereo, single audio track.

| File | Res | Level | Audio | Duration | Note |
|---|---|---|---|---|---|
| bandicam 2025-05-07 23-56-39-097.mp4 | 1280×720 | 3.1 | 48 kHz 192k | 9.3 s | **mismatch** (720p, 48 kHz) |
| bandicam 2025-05-10 13-01-22-046.mp4 | 1920×1080 | 4.0 | 44.1 kHz 96k | 2:47.7 | |
| bandicam 2025-05-10 13-08-45-313.mp4 | 1920×1080 | 4.0 | 44.1 kHz 96k | 6:00:35 (446 MB) | mostly static; 684 keyframes |
| bandicam 2025-05-19 16-06-25-504.mp4 | 1920×1080 | 4.0 | 44.1 kHz 96k | 5.1 s | |
| bandicam 2025-05-19 16-07-45-961.mp4 | 1920×1080 | 4.0 | 44.1 kHz 96k | 2.9 s | |
| bandicam 2025-05-19 16-57-11-870.mp4 | 1920×1080 | 4.0 | 44.1 kHz 96k | 42.9 s | |
| bandicam 2025-05-19 16-58-11-314.mp4 | 1920×1080 | 4.0 | 44.1 kHz 96k | 87.0 s | keyframes at 0, 12.27, 22.53, 32.80, 45.80 … |

**Bandicut re-encodes of phone HDR footage** (encoder `Lavc60.3.100 h264_nvenc`): H.264 Main 1080×1920 (portrait), yuv420p but tagged BT.2020/HLG, `has_b_frames=0`, AAC 44.1 kHz. Each file has a different odd frame rate/timebase. Stream-copy joins of these produced 0.15 s / 0.9 s video gaps.

| File | fps | time_base |
|---|---|---|
| Katka50-Wiper-Najprv-2.mp4 | 27.41 | 1/10964 |
| Katka50-Wiper-ToeToe.mp4 | 27.86 | 1/11144 |
| Katka50-ToeToe-Najprv2.mp4 | 29.87 | 1/11948 |

**Bandicam 8.0.1 high-resolution recordings via Intel Quick Sync (QSV)**, recorded 2026-11-17 on the author's hybrid Intel + NVIDIA laptop (Bandicam picked QSV; NVENC could not be enabled there). Both: 3200×2000, 15 fps nominal with VFR frame-skipping, time_base 1/15000, `has_b_frames=0`, yuv420p 8-bit, no color tags, AAC 44.1 kHz 96k stereo.

| File | Codec | Profile/level | Bitrate | Duration | Keyframes | Note |
|---|---|---|---|---|---|---|
| bandicam 2026-11-17 19-39-51-272.mp4 | H.264 | Main / 5.1 | 2.6 Mbps | 5.0 s | only at 0 | copies/concats cleanly |
| bandicam 2026-11-17 19-40-12-243.mp4 | HEVC | Main / 5.0 (level 150) | 2.9 Mbps | 3.9 s | only at 0 | malformed `hvcC` order; MP4 copy breaks; needs the Annex B/TS route (§3.2 item 11) |

These are too short to test smart cut (one keyframe each). **Longer recordings are still needed** (≥ 60 s with on-screen activity, in both codecs) to exercise heads/seams at high resolution, plus a real 3840×2160 clip.

**DaVinci Resolve Studio exports** (encoder "H.264 NVIDIA"): H.264 High 1080×1920, BT.709, 30000/1001 fps, time_base 1/30000, `has_b_frames=2`, AAC 48 kHz 320k, **plus a timecode data track** (needs `-ignore_unknown`). Keyframes every 1.0 s. They join cleanly by stream copy.
- `Katka50-Wiper-Najprv-Hudba.mp4` (19.6 s)
- `Katka50-ToeToe-Najprv2-Hudba.mp4` (23.1 s)

## Appendix B — Reference commands that worked in testing
```bash
# keyframe list
ffprobe -v error -select_streams v:0 -show_entries packet=pts_time,flags -of csv=p=0 in.mp4 | awk -F, '$2 ~ /K/ {print $1}'

# smart-cut head (x264) + copied rest, then concat (video lengths exact; audio must NOT be copied like this)
ffmpeg -ss 5 -i in.mp4 -t <K-5-0.001> -map 0 -c:v libx264 -preset slow -crf 18 -profile:v high -level 4.0 -pix_fmt yuv420p -bf 0 -g 150 -c:a copy -video_track_timescale 15000 head.mp4
ffmpeg -ss <K> -i in.mp4 -t <E-K> -map 0 -c copy -avoid_negative_ts make_zero body.mp4
ffmpeg -f concat -safe 0 -i list.txt -map 0 -c copy out.mp4

# decode check
ffmpeg -v error -i out.mp4 -f null -

# Bandicam QSV HEVC: the direct MP4 copy breaks; this route is clean
ffmpeg -i src.mp4 -map 0:v -c copy -bsf:v hevc_mp4toannexb piece.ts
ffmpeg -i piece.ts -map 0:v -c copy out.mp4

# encoder availability test
ffmpeg -v error -f lavfi -i color=black:s=320x240:r=15:d=0.2 -c:v h264_nvenc -f null -
```
(Note: these spike commands used `-t` and copied audio. The production design in §6.4/§6.6 replaces them with frame counts and the separate audio pipeline.)
