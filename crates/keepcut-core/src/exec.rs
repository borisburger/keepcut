//! Runs an [`ExportPlan`]: temp dir, jobs, progress, cancel, cleanup (§6.10).

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::chapters::{self, Chapter};
use crate::encoders;
use crate::error::{Error, Result};
use crate::ffmpeg::{self, CancelToken, LogLine, LogSink, RunOpts, Tools};
use crate::model::{Chapters, ExportSettings, Loudness};
use crate::plan::{self, ExportPlan, OutputPlan, PieceKind};
use crate::probe::MediaInfo;
use crate::verify::{self, DecodeCheck, Expect, Verification};

#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    /// Parent of the `keepcut/<uuid>` temp dir; default: the OS temp dir.
    pub temp_root: Option<PathBuf>,
    /// Keep intermediates (debugging).
    pub keep_temp: bool,
    pub decode_check: DecodeCheck,
}

#[derive(Debug, Clone)]
pub struct Progress {
    /// 0..1 over the whole export.
    pub fraction: f64,
    pub step: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputResult {
    pub path: PathBuf,
    pub verification: Verification,
    /// Per-piece timing, for calibration: (description, seconds).
    pub timings: Vec<(String, f64)>,
}

/// Deletes `keepcut/*` temp dirs older than one day (call at startup).
pub fn cleanup_stale(temp_root: Option<&Path>) {
    let root = temp_root
        .map_or_else(std::env::temp_dir, Path::to_path_buf)
        .join("keepcut");
    let Ok(rd) = std::fs::read_dir(&root) else {
        return;
    };
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age.as_secs() > 86400);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn piece_weight(k: &PieceKind) -> f64 {
    match k {
        PieceKind::Copy { .. } => 0.05,
        PieceKind::Encode { enc, .. } if encoders::is_software(&enc.encoder) => 1.0,
        PieceKind::Encode { .. } => 0.3,
    }
}

const AUDIO_WEIGHT: f64 = 0.1;
const MUX_WEIGHT: f64 = 0.1;

struct Tracker<'a> {
    total: f64,
    done: f64,
    cb: &'a mut dyn FnMut(Progress),
}

impl Tracker<'_> {
    fn report(&mut self, step: &str, within: f64) {
        let f = if self.total > 0.0 {
            ((self.done + within) / self.total).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (self.cb)(Progress {
            fraction: f,
            step: step.to_string(),
        });
    }
}

/// Executes all outputs of the plan. On error or cancel, partial outputs and
/// temp files are deleted.
#[allow(clippy::too_many_arguments)]
pub fn execute(
    tools: &Tools,
    plan: &ExportPlan,
    infos: &[MediaInfo],
    settings: &ExportSettings,
    opts: &ExecOptions,
    cancel: &CancelToken,
    log: &LogSink,
    progress: &mut dyn FnMut(Progress),
) -> Result<Vec<OutputResult>> {
    if settings.loudness != Loudness::Off {
        log(LogLine::Info(
            "loudness normalization is not implemented yet; exporting without it",
        ));
    }
    let root = opts
        .temp_root
        .clone()
        .unwrap_or_else(std::env::temp_dir)
        .join("keepcut");
    std::fs::create_dir_all(&root)?;
    let tmp = tempfile::Builder::new()
        .prefix("export-")
        .tempdir_in(&root)?;

    let total: f64 = plan
        .outputs
        .iter()
        .map(|o| {
            o.segments
                .iter()
                .map(|s| {
                    s.pieces
                        .iter()
                        .map(|p| (p.end - p.start) * piece_weight(&p.kind))
                        .sum::<f64>()
                        + s.duration() * AUDIO_WEIGHT
                })
                .sum::<f64>()
                + o.duration() * MUX_WEIGHT
        })
        .sum();
    let mut tracker = Tracker {
        total,
        done: 0.0,
        cb: progress,
    };

    let mut results = Vec::new();
    for (oi, out) in plan.outputs.iter().enumerate() {
        let dir = tmp.path().join(format!("out{oi}"));
        std::fs::create_dir_all(&dir)?;
        let partial = partial_path(&out.path);
        let r = run_output(
            tools,
            out,
            infos,
            settings,
            opts,
            &dir,
            &partial,
            cancel,
            log,
            &mut tracker,
        );
        match r {
            Ok(mut res) => {
                std::fs::rename(&partial, &out.path)?;
                res.path = out.path.clone();
                results.push(res);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&partial);
                if opts.keep_temp {
                    let kept = tmp.keep();
                    log(LogLine::Info(&format!(
                        "temp files kept in {}",
                        kept.display()
                    )));
                }
                return Err(e);
            }
        }
    }
    if opts.keep_temp {
        let kept = tmp.keep();
        log(LogLine::Info(&format!(
            "temp files kept in {}",
            kept.display()
        )));
    }
    tracker.report("done", 0.0);
    Ok(results)
}

fn partial_path(p: &Path) -> PathBuf {
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    p.with_file_name(format!(".{name}.keepcut-partial"))
}

#[allow(clippy::too_many_arguments)]
fn run_output(
    tools: &Tools,
    out: &OutputPlan,
    infos: &[MediaInfo],
    settings: &ExportSettings,
    opts: &ExecOptions,
    dir: &Path,
    partial: &Path,
    cancel: &CancelToken,
    log: &LogSink,
    tr: &mut Tracker<'_>,
) -> Result<OutputResult> {
    let mut timings = Vec::new();
    let mut video_items = Vec::new();
    let nseg = out.segments.len();

    // 1. video pieces: encodes first (their decode delay is only known afterwards),
    //    then copies with their DTS shifted so all pieces share the largest delay.
    //    Otherwise DTS would go backwards at a seam into a piece with a larger
    //    B-frame delay, and the muxer would rewrite the timestamps.
    let (fmt, ext) = out.intermediate();
    let mut files: Vec<Vec<PathBuf>> = Vec::new();
    let mut delays: Vec<Vec<f64>> = Vec::new();
    for (si, sp) in out.segments.iter().enumerate() {
        files.push(
            (0..sp.pieces.len())
                .map(|pi| dir.join(format!("v{si:04}_{pi:02}.{ext}")))
                .collect(),
        );
        delays.push(
            sp.pieces
                .iter()
                .map(|p| {
                    if let PieceKind::Copy { delay, .. } = p.kind {
                        delay
                    } else {
                        0.0
                    }
                })
                .collect(),
        );
    }
    for pass_copy in [false, true] {
        if pass_copy {
            for (si, sp) in out.segments.iter().enumerate() {
                for (pi, piece) in sp.pieces.iter().enumerate() {
                    if !piece.is_copy() {
                        delays[si][pi] = measure_delay(tools, &files[si][pi], log)?;
                    }
                }
            }
        }
        let max_delay = delays.iter().flatten().fold(0.0_f64, |a, b| a.max(*b));
        for (si, sp) in out.segments.iter().enumerate() {
            for (pi, piece) in sp.pieces.iter().enumerate() {
                if piece.is_copy() != pass_copy {
                    continue;
                }
                cancel.check()?;
                let file = &files[si][pi];
                let shift = if pass_copy {
                    max_delay - delays[si][pi]
                } else {
                    0.0
                };
                let cmd = plan::piece_cmd(tools, sp, piece, file, fmt, shift);
                let what = match &piece.kind {
                    PieceKind::Copy { .. } => "copying".to_string(),
                    PieceKind::Encode { enc, role, .. } => {
                        format!("encoding {:?} ({})", role, enc.encoder).to_lowercase()
                    }
                };
                let step = format!("Segment {}/{nseg}: {what}", si + 1);
                let w = piece_weight(&piece.kind);
                let len = (piece.end - piece.start).max(1e-6);
                tr.report(&step, 0.0);
                let t0 = Instant::now();
                {
                    let mut cb = |t: f64| tr.report(&step, (t / len).clamp(0.0, 1.0) * len * w);
                    ffmpeg::run_checked(
                        &cmd,
                        RunOpts {
                            cancel: Some(cancel),
                            log: Some(log),
                            on_progress: Some(&mut cb),
                            ..Default::default()
                        },
                    )?;
                }
                timings.push((
                    format!(
                        "{} {} [{:.3}, {:.3})",
                        sp.src.file_name().unwrap_or_default().to_string_lossy(),
                        what,
                        piece.start,
                        piece.end
                    ),
                    t0.elapsed().as_secs_f64(),
                ));
                tr.done += len * w;
                check_piece(tools, file, piece, log)?;
            }
        }
        if pass_copy {
            // encoded pieces with a smaller delay than the largest one: remux with shifted DTS
            for (si, sp) in out.segments.iter().enumerate() {
                for (pi, piece) in sp.pieces.iter().enumerate() {
                    let shift = max_delay - delays[si][pi];
                    if piece.is_copy() || shift < 1e-6 {
                        continue;
                    }
                    let shifted = files[si][pi].with_extension(format!("shift.{ext}"));
                    let cmd = plan::dts_shift_cmd(tools, &files[si][pi], shift, &shifted, fmt);
                    ffmpeg::run_checked(
                        &cmd,
                        RunOpts {
                            cancel: Some(cancel),
                            log: Some(log),
                            ..Default::default()
                        },
                    )?;
                    files[si][pi] = shifted;
                }
            }
        }
    }
    for (si, sp) in out.segments.iter().enumerate() {
        for (pi, piece) in sp.pieces.iter().enumerate() {
            video_items.push((files[si][pi].clone(), piece.out_duration));
        }
    }

    // 2. audio pieces (duration follows the produced video)
    let mut audio_items = Vec::new();
    for (si, sp) in out.segments.iter().enumerate() {
        cancel.check()?;
        let d = sp.duration();
        let file = dir.join(format!("a{si:04}.flac"));
        let step = format!("Segment {}/{nseg}: audio", si + 1);
        tr.report(&step, 0.0);
        let cmd = plan::audio_piece_cmd(tools, sp, d, &file);
        {
            let mut cb =
                |t: f64| tr.report(&step, (t / d.max(1e-6)).clamp(0.0, 1.0) * d * AUDIO_WEIGHT);
            ffmpeg::run_checked(
                &cmd,
                RunOpts {
                    cancel: Some(cancel),
                    log: Some(log),
                    on_progress: Some(&mut cb),
                    ..Default::default()
                },
            )?;
        }
        tr.done += d * AUDIO_WEIGHT;
        audio_items.push((file, d));
    }

    // 3. chapters
    let chapters = build_chapters(out, settings.chapters);
    let chapters_file = if chapters.is_empty() {
        None
    } else {
        let p = dir.join("chapters.ffmeta");
        std::fs::write(&p, chapters::ffmetadata(&chapters))?;
        Some(p)
    };

    // 4. final mux
    let vlist = dir.join("video.ffconcat");
    let alist = dir.join("audio.ffconcat");
    std::fs::write(&vlist, plan::concat_list(&video_items))?;
    std::fs::write(&alist, plan::concat_list(&audio_items))?;
    let total = out.duration();
    let cmd = plan::final_mux_cmd(
        tools,
        out,
        &vlist,
        &alist,
        chapters_file.as_deref(),
        settings.audio_bitrate_kbps,
        settings.loudness != Loudness::Off,
        partial,
    );
    let step = "Joining".to_string();
    tr.report(&step, 0.0);
    {
        let mut cb = |t: f64| {
            tr.report(
                &step,
                (t / total.max(1e-6)).clamp(0.0, 1.0) * total * MUX_WEIGHT,
            )
        };
        ffmpeg::run_checked(
            &cmd,
            RunOpts {
                cancel: Some(cancel),
                log: Some(log),
                on_progress: Some(&mut cb),
                ..Default::default()
            },
        )?;
    }
    tr.done += total * MUX_WEIGHT;

    // 5. verify
    tr.report("Verifying", 0.0);
    let expect = expectation(out, infos);
    let verification = verify::verify(
        tools,
        partial,
        &expect,
        opts.decode_check,
        Some(cancel),
        Some(log),
    )?;
    Ok(OutputResult {
        path: partial.to_path_buf(),
        verification,
        timings,
    })
}

/// Checks that a produced piece has the expected number of frames.
fn check_piece(tools: &Tools, file: &Path, piece: &plan::Piece, log: &LogSink) -> Result<()> {
    let expected = match &piece.kind {
        PieceKind::Copy { packets, .. } => *packets,
        PieceKind::Encode { frames, .. } => *frames,
    };
    let cmd = tools
        .ffprobe()
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_packets",
            "-show_entries",
            "stream=nb_read_packets",
            "-of",
            "csv=p=0",
        ])
        .arg(file);
    let o = ffmpeg::run_checked(
        &cmd,
        RunOpts {
            log: Some(log),
            ..Default::default()
        },
    )?;
    let n: usize = String::from_utf8_lossy(&o.stdout)
        .lines()
        .find_map(|l| l.trim().trim_end_matches(',').parse().ok())
        .unwrap_or(0);
    if n != expected {
        return Err(Error::Plan(format!(
            "piece {} has {n} frames, expected {expected}",
            file.display()
        )));
    }
    Ok(())
}

pub fn build_chapters(out: &OutputPlan, mode: Chapters) -> Vec<Chapter> {
    let mut ch: Vec<Chapter> = Vec::new();
    let mut t = 0.0;
    for sp in &out.segments {
        let d = sp.duration();
        match mode {
            Chapters::None => {}
            Chapters::PerSegment => ch.push(Chapter {
                start: t,
                end: t + d,
                title: sp.chapter_title.clone(),
            }),
            Chapters::PerClip => {
                let title = sp
                    .src
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match ch.last_mut() {
                    Some(last) if sp.seg_idx > 0 && last.title == title => last.end = t + d,
                    _ => ch.push(Chapter {
                        start: t,
                        end: t + d,
                        title,
                    }),
                }
            }
        }
        t += d;
    }
    ch
}

fn expectation<'a>(out: &'a OutputPlan, infos: &[MediaInfo]) -> Expect<'a> {
    let mut seams = Vec::new();
    let mut t = 0.0;
    let mut frames = 0;
    let mut max_delta: f64 = 0.0;
    let mut sources = Vec::new();
    let mut frame_times = Some(Vec::new());
    for sp in &out.segments {
        let info = &infos[sp.clip_idx];
        let seg_offset = t;
        if sp
            .pieces
            .iter()
            .any(|p| matches!(p.kind, PieceKind::Encode { fps: Some(_), .. }))
        {
            frame_times = None;
        }
        if let Some(ft) = frame_times.as_mut() {
            let fi = &info.frames;
            let (a, b) = (fi.idx_at_or_after(sp.start), fi.idx_at_or_after(sp.end));
            ft.extend(fi.frames[a..b].iter().map(|f| f.t - sp.start + seg_offset));
        }
        for p in &sp.pieces {
            if t > 0.0 {
                seams.push(t);
            }
            t += p.out_duration;
            frames += match &p.kind {
                PieceKind::Copy { packets, .. } => *packets,
                PieceKind::Encode { frames, .. } => *frames,
            };
        }
        max_delta = max_delta.max(info.frames.max_frame_delta());
        if let PieceKind::Encode { fps: Some(f), .. } = sp.pieces[0].kind {
            max_delta = max_delta.max(1.0 / f as f64);
        }
        // baseline: the first ≤ 4 s of each segment's source range
        sources.push((sp.src.as_path(), sp.start, sp.end.min(sp.start + 4.0)));
    }
    Expect {
        duration: t,
        frames,
        seams,
        max_frame_delta: max_delta,
        sources,
        frame_times,
    }
}

/// Decode delay of an intermediate: smallest PTS minus the first DTS (seconds).
fn measure_delay(tools: &Tools, file: &Path, log: &LogSink) -> Result<f64> {
    let cmd = tools
        .ffprobe()
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-read_intervals",
            "%+#32",
            "-show_entries",
            "packet=pts_time,dts_time",
            "-of",
            "compact=p=0",
        ])
        .arg(file);
    let o = ffmpeg::run_checked(
        &cmd,
        RunOpts {
            log: Some(log),
            quiet_stderr: true,
            ..Default::default()
        },
    )?;
    let mut first_dts = None;
    let mut min_pts = f64::INFINITY;
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        for kv in line.split('|') {
            match kv.split_once('=') {
                Some(("pts_time", v)) => min_pts = min_pts.min(v.parse().unwrap_or(f64::INFINITY)),
                Some(("dts_time", v)) if first_dts.is_none() => first_dts = v.parse::<f64>().ok(),
                _ => {}
            }
        }
    }
    Ok(match first_dts {
        Some(d) if min_pts.is_finite() => (min_pts - d).max(0.0),
        _ => 0.0,
    })
}
