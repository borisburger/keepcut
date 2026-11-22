# Preview artifacts: black lines and blocks

Notes on an intermittent display problem in Keepcut's video preview, what has been
checked so far, and what to collect and try when it happens again.

## Symptom

- Seen on 2026-11-22 while previewing a clip in Keepcut: short **black horizontal lines**
  scattered over the picture, sometimes **whole blocks of the picture masked black**.
- The same file played cleanly in **VLC** and in the Windows **Media Player**.
- The clip was a smart cut made by Keepcut (`… - cut.mp4`) of a phone video. The project
  also had a soundtrack set, so it may have been watched in "Preview all".
- **Intermittent:** the artifacts stopped appearing while it was being investigated, and
  could not be reproduced in scripted runs afterwards.

## Machine

- Laptop with hybrid graphics: **NVIDIA GeForce RTX 4060 Laptop** (driver 32.0.16.2124,
  2026-11) and **Intel Iris Xe** (driver **31.0.101.5186, 2024-01**: old).
- Keepcut's OpenGL preview runs on the **Intel** GPU (log line
  `OpenGL: Intel(R) Iris(R) Xe Graphics (Intel), 3.3.0 - Build 31.0.101.5186`).
- Preview decoding was **`d3d11va-copy`**: D3D11 hardware decoding, with frames copied back
  to the CPU and then uploaded to OpenGL (status bar "video: OpenGL · hwdec d3d11va-copy").

## The file

- 478×850 H.264 High, level 3.1, yuv420p, BT.709 limited, variable frame rate (~30 fps),
  CABAC, `pic_order_cnt_type` 2, no B-frames, 1 reference frame.
- Smart cut: copied parts of the source plus **three x264-encoded heads** (from a cut to the
  next keyframe). The heads' SPS/PPS differ from the source's: `max_num_ref_frames` = 5
  instead of 1 and weighted prediction on. Every part carries its own parameter sets
  in-band (14 SPS in the file).
- Container timestamps strictly increase; Keepcut's own verification passed.

## What was checked (all clean)

1. **ffmpeg, software vs D3D11 hardware decoding:** identical frames over the whole cut
   (`framemd5` of both decodes, 3388 frames, 0 differences). The file decodes correctly on
   this GPU.
   ```
   ffmpeg -i cut.mp4 -map 0:v -pix_fmt nv12 -f framemd5 sw.md5
   ffmpeg -hwaccel d3d11va -i cut.mp4 -map 0:v -pix_fmt nv12 -f framemd5 hw.md5
   ```
2. **libmpv decoding** (Keepcut's libmpv, in mpv's encode mode, `o=out.nut ovc=rawvideo`)
   with `hwdec=no` and with `hwdec=d3d11va-copy`: both produce the same frames as ffmpeg,
   plus 3 frames that ffmpeg's `framemd5` run dropped because of its 30 fps timestamp
   rounding. **No black areas** in any decoded frame (`blackframe` filter). So mpv decodes
   the file correctly too, including the parameter-set changes.
3. **Synthetic reproduction:** a 478×850 x264 source with the same settings (1 ref, no
   B-frames, CABAC, 10 s GOP), smart-cut mid-GOP by `keepcut-cli` so it has x264 heads with
   5 refs. 12 in-app screenshots during playback: no artifacts.
4. **The real files in the app:** 20 s of playback of both the source and the cut, one in-app
   screenshot per second, scanned for thin dark lines: no difference between the two (the
   counts were natural dark detail). The artifacts did not show.
5. **Not caused by the export:** decoding is clean, timestamps are clean, and the same
   problem would show in VLC / Media Player if the file were damaged.

6. **Seen again (same day)** with a second export of the same project (joined, soundtrack
   replacing the audio). Its video stream is the smart cut copied unchanged (same 14
   SPS/PPS, D3D11 and software decodes identical), so the trigger isn't the file. It
   showed in the user's **release** build (`target/release`, same `libmpv-2.dll` as the
   debug build), started by hand. Scripted runs of the same file showed **nothing**, both
   with the debug and a fresh release build (12 in-app screenshots during playback, checked
   at full resolution). What differs between those runs and the user's:
   - scripted runs are **muted** (`mute`) and start in a fixed 1280×800 window; the user's
     window has its saved size and position (possibly maximized, or on another monitor);
   - the user's session had been running and doing other things before (exports with
     QSV/NVENC, other files, Preview all with the soundtrack);
   - the user interacts (mouse over the timeline, keyboard), which changes egui's frame
     timing.

## Conclusion so far

The file and the decoding are fine. The artifacts come from the **live display path**:
after decoding, either the copy of decoded frames to OpenGL textures, or the OpenGL
drawing itself. It's intermittent, so the GPU, the driver or GPU memory pressure is
involved. Candidates, most likely first:

1. **The Intel OpenGL driver** (2024-01, old): texture upload or rendering bugs in mpv's
   OpenGL path.
2. **Hybrid graphics:** the preview on the Intel GPU while something else (another app, an
   export with NVENC/QSV) uses the GPUs.
3. **mpv's direct rendering or the copy-back** (`d3d11va-copy` downloads into buffers that
   are then uploaded); an mpv/driver interaction.
4. A bug in Keepcut's own render code (`crates/keepcut-player/src/render.rs`): the FBO
   texture shown by egui. Less likely: it would not depend on the file, and resizing (the
   one known case of a blank texture) was fixed in commit 2418752.

## When it happens again

Don't restart Keepcut first. Note:

- **What you were doing:** normal playback, frame stepping, seeking, Preview all (with or
  without a soundtrack), cut preview, right after resizing or moving the window, or with
  another program using the GPU (a game, a browser video, an export running).
- **Which file:** does the *source* show it too, or only the smart cut? Does it happen at
  the same place each time?
- **The status bar:** `video: OpenGL/software · hwdec … | GPU: …` (the GPU that draws the
  window; hover it for the driver version).
- **The log** (Settings → Open log folder, today's `keepcut.…log`): the `OpenGL:` line at
  startup and the `preview decoding:` lines.
- **Power:** on battery or plugged in (hybrid laptops switch GPUs and clocks).
- **Monitors:** which display the window is on (the laptop panel or an external one; on
  hybrid laptops external outputs are often wired to the NVIDIA GPU while Keepcut draws on
  the Intel one), maximized or not, and whether moving it to the other display changes
  anything.

Then narrow it down, one change at a time:

1. **Settings → Hardware decoding: Off (CPU)**: applies at once. If the artifacts go away,
   the hardware decoding or its copy-back is involved.
2. **Settings → Video display: Software** (after a restart): mpv renders on the CPU and
   Keepcut shows the result as an ordinary texture. If the artifacts go away, it's the
   OpenGL path (mpv's GL renderer or the Intel driver).
3. **Run Keepcut on the NVIDIA GPU:** Windows Settings → System → Display → Graphics →
   add `keepcut.exe` → High performance. Check the `OpenGL:` log line afterwards.
4. **Update the Intel graphics driver** (Intel's driver assistant, or the laptop maker's).
5. Play the same file in the standalone **mpv player**
   (`mpv --hwdec=auto-safe --vo=gpu --gpu-api=opengl file.mp4`); if it shows the same
   artifacts, it's mpv + driver, not Keepcut.

## Tools for the investigation

- Script commands (`keepcut --script FILE`, see `crates/keepcut/src/script.rs`): `play`,
  `seek T`, `screenshot PATH` (in-app capture of Keepcut's own frame), `mute`,
  `hw-decoding auto|off`, `mpv-set NAME VALUE` (any mpv property, e.g. `mpv-set hwdec no`),
  `window-size W H`, `print-state` (includes `hwdec`).
- Decoding check against ffmpeg without looking at the picture: decode with libmpv in encode
  mode (`hwdec`, `o`, `of=nut`, `ovc=rawvideo`, `aid=no`), then compare `framemd5` with
  ffmpeg's software decode, and run `blackframe=amount=0:threshold=24` over mpv's output.
  The small program used for this was temporary and is not in the repository; it is about
  30 lines of `libmpv2` (create mpv with those options, `loadfile`, wait for `EndFile`).
