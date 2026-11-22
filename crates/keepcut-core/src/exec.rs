//! Runs an [`ExportPlan`]: temp dir, jobs, progress, cancel, cleanup (§6.10).

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::chapters::{self, Chapter};
use crate::encoders;
use crate::fsutil;
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
    /// Things the user should know, e.g. an encoder fallback.
    #[serde(default)]
    pub notes: Vec<String>,
}

const SPACE_MARGIN: u64 = 50_000_000;

/// Re-tests every hardware encode of the plan with its real parameters and
/// switches failing ones to the software encoder (§6.8). Returns notes.
pub fn check_encoders(
    tools: &Tools,
    plan: &mut ExportPlan,
    cancel: &CancelToken,
    log: &LogSink,
) -> Result<Vec<String>> {
    let mut tested: Vec<(encoders::VideoEncode, Option<String>)> = Vec::new();
    let mut notes = Vec::new();
    for out in &mut plan.outputs {
        for sp in &mut out.segments {
            for p in &mut sp.pieces {
                let PieceKind::Encode { enc, .. } = &mut p.kind else {
                    continue;
                };
                if encoders::is_software(&enc.encoder) {
                    continue;
                }
                let failure = match tested.iter().find(|(e, _)| e == enc.as_ref()) {
                    Some((_, f)) => f.clone(),
                    None => {
                        cancel.check()?;
                        let f = encoders::test_encode(tools, enc, Some(cancel), Some(log))?;
                        if let Some(reason) = &f {
                            let sw = enc.to_software();
                            let note = format!(
                                "{} can't encode {}×{} {} ({reason}); using {} instead",
                                enc.encoder, enc.width, enc.height, enc.pix_fmt, sw.encoder
                            );
                            log(LogLine::Info(&note));
                            notes.push(note);
                        }
                        tested.push(((**enc).clone(), f.clone()));
                        f
                    }
                };
                if failure.is_some() {
                    **enc = enc.to_software();
                }
            }
        }
    }
    Ok(notes)
}

/// Picks the temp parent folder and checks free space on the temp and output
/// volumes (§6.10). Falls back to a hidden folder next to the first output when
/// the OS temp volume is too small.
fn prepare_space(plan: &ExportPlan, opts: &ExecOptions, log: &LogSink) -> Result<TempPlace> {
    let need_temp = plan.outputs.iter().map(|o| o.est_temp_bytes).max().unwrap_or(0);
    // estimates of re-encoded parts can be off by 2–4× (10-bit Quick Sync measured
    // 4× over the guess): only refuse when even 1.5× the estimate does not fit
    let need_temp = need_temp + need_temp / 2 + SPACE_MARGIN;
    let os_temp = opts.temp_root.clone().unwrap_or_else(std::env::temp_dir);
    // output space per folder
    let mut per_dir: Vec<(PathBuf, u64)> = Vec::new();
    for o in &plan.outputs {
        let d = o.path.parent().map(Path::to_path_buf).unwrap_or_default();
        match per_dir.iter_mut().find(|(p, _)| *p == d) {
            Some((_, n)) => *n += o.est_bytes,
            None => per_dir.push((d, o.est_bytes)),
        }
    }
    let temp_free = fsutil::free_space(&os_temp);
    let place = if opts.temp_root.is_some() || temp_free.is_none_or(|f| f >= need_temp) {
        TempPlace::Under(os_temp.join("keepcut"))
    } else {
        let dir = per_dir[0].0.clone();
        log(LogLine::Info(&format!(
            "not enough space in {} ({} free, about {} needed); temp files go to {}",
            os_temp.display(),
            fsutil::human_bytes(temp_free.unwrap_or(0)),
            fsutil::human_bytes(need_temp),
            dir.display()
        )));
        per_dir[0].1 += need_temp;
        TempPlace::OutputFolder(dir)
    };
    for (dir, need) in &per_dir {
        let need = need + need / 2 + SPACE_MARGIN;
        if let Some(free) = fsutil::free_space(dir)
            && free < need
        {
            return Err(Error::NoSpace(format!(
                "Not enough disk space in {}: the export needs about {}, only {} is free",
                dir.display(),
                fsutil::human_bytes(need),
                fsutil::human_bytes(free)
            )));
        }
    }
    Ok(place)
}

enum TempPlace {
    /// `<dir>/export-XXXX` (dir = `<OS temp>/keepcut`).
    Under(PathBuf),
    /// `<output folder>/.keepcut-tmp-XXXX`.
    OutputFolder(PathBuf),
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
/// ebur128 over the FLAC pieces runs at ~500× real time.
const LOUDNESS_WEIGHT: f64 = 0.01;
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
    progress(Progress {
        fraction: 0.0,
        step: "Checking encoders and disk space".into(),
    });
    let mut plan = plan.clone();
    let notes = check_encoders(tools, &mut plan, cancel, log)?;
    let plan = &plan;
    let tmp = match prepare_space(plan, opts, log)? {
        TempPlace::Under(root) => {
            std::fs::create_dir_all(&root)?;
            tempfile::Builder::new()
                .prefix("export-")
                .tempdir_in(&root)?
        }
        TempPlace::OutputFolder(dir) => {
            std::fs::create_dir_all(&dir)?;
            tempfile::Builder::new()
                .prefix(".keepcut-tmp-")
                .tempdir_in(&dir)?
        }
    };

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
                + if settings.loudness == Loudness::Off {
                    0.0
                } else {
                    o.duration() * LOUDNESS_WEIGHT
                }
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
                // export-wide notes (encoder fallbacks) go with the first output
                if oi == 0 {
                    res.notes.splice(0..0, notes.iter().cloned());
                }
                results.push(res);
                // free this output's intermediates before the next one (peak temp)
                if !opts.keep_temp {
                    let _ = std::fs::remove_dir_all(&dir);
                }
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
    let mut notes = Vec::new();
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
                let mut piece = piece.clone();
                let what = |piece: &plan::Piece| match &piece.kind {
                    PieceKind::Copy { .. } => "copying".to_string(),
                    PieceKind::Encode { enc, role, .. } => {
                        format!("encoding {:?} ({})", role, enc.encoder).to_lowercase()
                    }
                };
                let w = piece_weight(&piece.kind);
                let len = (piece.end - piece.start).max(1e-6);
                let t0 = Instant::now();
                loop {
                    let step = format!("Segment {}/{nseg}: {}", si + 1, what(&piece));
                    tr.report(&step, 0.0);
                    let cmd = plan::piece_cmd(tools, sp, &piece, file, fmt, shift);
                    let mut cb = |t: f64| tr.report(&step, (t / len).clamp(0.0, 1.0) * len * w);
                    let r = ffmpeg::run_checked(
                        &cmd,
                        RunOpts {
                            cancel: Some(cancel),
                            log: Some(log),
                            on_progress: Some(&mut cb),
                            ..Default::default()
                        },
                    );
                    match (r, &mut piece.kind) {
                        (Ok(_), _) => break,
                        // a hardware encoder can still fail on real content (driver,
                        // session limits): retry this piece once in software
                        (Err(Error::CommandFailed { .. }), PieceKind::Encode { enc, .. })
                            if !encoders::is_software(&enc.encoder) =>
                        {
                            let sw = enc.to_software();
                            let note = format!(
                                "{} failed on {} [{:.3}, {:.3}); re-encoded that part with {}",
                                enc.encoder,
                                sp.src.file_name().unwrap_or_default().to_string_lossy(),
                                piece.start,
                                piece.end,
                                sw.encoder
                            );
                            log(LogLine::Info(&note));
                            notes.push(note);
                            **enc = sw;
                        }
                        (Err(e), _) => return Err(e),
                    }
                }
                timings.push((
                    format!(
                        "{} {} [{:.3}, {:.3})",
                        sp.src.file_name().unwrap_or_default().to_string_lossy(),
                        what(&piece),
                        piece.start,
                        piece.end
                    ),
                    t0.elapsed().as_secs_f64(),
                ));
                tr.done += len * w;
                check_piece(tools, file, &piece, log)?;
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

    // 3. loudness (§6.9): measure, then apply the gains in the final mux
    let audio_filter = if settings.loudness == Loudness::Off {
        None
    } else {
        tr.report("Measuring loudness", 0.0);
        let gains = loudness_gains(tools, out, &audio_items, settings, dir, cancel, log)?;
        tr.done += out.duration() * LOUDNESS_WEIGHT;
        let mut t = 0.0;
        let starts: Vec<(f64, f64)> = out
            .segments
            .iter()
            .zip(&gains)
            .map(|(sp, g)| {
                let s = t;
                t += sp.duration();
                (s, *g)
            })
            .collect();
        let graph = plan::loudness_filter(&starts);
        log(LogLine::Info(&format!("audio filter: {graph}")));
        let p = dir.join("audio.filter");
        std::fs::write(&p, graph)?;
        Some(p)
    };

    // 4. chapters
    let chapters = build_chapters(out, settings.chapters);
    let chapters_file = if chapters.is_empty() {
        None
    } else {
        let p = dir.join("chapters.ffmeta");
        std::fs::write(&p, chapters::ffmetadata(&chapters))?;
        Some(p)
    };

    // 5. final mux
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
        audio_filter.as_deref(),
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

    // 6. verify
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
        notes,
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

/// Chapters of an output, timed from the produced segment durations (§6.13).
pub fn build_chapters(out: &OutputPlan, mode: Chapters) -> Vec<Chapter> {
    let mut ch: Vec<Chapter> = Vec::new();
    let mut t = 0.0;
    let mut prev_clip = None;
    for sp in &out.segments {
        let d = sp.duration();
        match mode {
            Chapters::None => {}
            Chapters::PerSegment => ch.push(Chapter {
                start: t,
                end: t + d,
                title: sp.chapter_title.clone(),
            }),
            Chapters::PerClip => match ch.last_mut() {
                Some(last) if prev_clip == Some(sp.clip_idx) => last.end = t + d,
                _ => ch.push(Chapter {
                    start: t,
                    end: t + d,
                    title: sp.clip_title.clone(),
                }),
            },
        }
        prev_clip = Some(sp.clip_idx);
        t += d;
    }
    ch
}

/// Integrated loudness (LUFS) from ebur128's summary on stderr.
pub fn parse_ebur128(stderr: &str) -> Option<f64> {
    let mut lines = stderr.lines().skip_while(|l| !l.contains("Integrated loudness:"));
    lines.find_map(|l| {
        let v = l.trim().strip_prefix("I:")?;
        v.trim().trim_end_matches("LUFS").trim().parse().ok()
    })
}

/// Below this, a unit counts as silence and is left alone (§6.9).
const SILENCE_LUFS: f64 = -70.0;
/// Integrated loudness of shorter segments is unreliable: they use their clip's value.
const MIN_MEASURE_SECS: f64 = 3.0;

/// Gain (dB) that brings `measured` to `target`, clamped to ±20 dB; 0 for silence.
pub fn loudness_gain(measured: Option<f64>, target: f64) -> f64 {
    match measured {
        Some(m) if m > SILENCE_LUFS + 0.05 => (target - m).clamp(-20.0, 20.0),
        _ => 0.0,
    }
}

/// Measures the audio pieces of an output and returns one gain (dB) per segment.
fn loudness_gains(
    tools: &Tools,
    out: &OutputPlan,
    audio: &[(PathBuf, f64)],
    settings: &ExportSettings,
    dir: &Path,
    cancel: &CancelToken,
    log: &LogSink,
) -> Result<Vec<f64>> {
    let measure = |items: &[(PathBuf, f64)], label: &str| -> Result<f64> {
        cancel.check()?;
        let m = measure_loudness(tools, items, dir, cancel, log)?;
        let g = loudness_gain(m, settings.loudness_target);
        let ms = m.map_or("silent".to_string(), |m| format!("{m:.1} LUFS"));
        log(LogLine::Info(&format!(
            "loudness of {label}: {ms}, gain {g:+.1} dB"
        )));
        Ok(g)
    };
    // segments per source clip, in output order
    let mut clips: Vec<(usize, Vec<usize>)> = Vec::new();
    for (i, sp) in out.segments.iter().enumerate() {
        match clips.iter_mut().find(|(c, _)| *c == sp.clip_idx) {
            Some((_, v)) => v.push(i),
            None => clips.push((sp.clip_idx, vec![i])),
        }
    }
    let mut gains = vec![0.0; out.segments.len()];
    if settings.loudness == Loudness::Whole {
        gains.fill(measure(audio, "the whole output")?);
        return Ok(gains);
    }
    for (_, segs) in &clips {
        // the clip's value: per clip, and for its segments that are too short
        let per_clip = settings.loudness == Loudness::PerClip
            || segs.iter().any(|&i| out.segments[i].duration() < MIN_MEASURE_SECS);
        let clip_gain = if per_clip {
            let items: Vec<_> = segs.iter().map(|&i| audio[i].clone()).collect();
            measure(&items, &out.segments[segs[0]].clip_title)?
        } else {
            0.0
        };
        for &i in segs {
            let sp = &out.segments[i];
            gains[i] = match settings.loudness {
                Loudness::PerSegment if sp.duration() >= MIN_MEASURE_SECS => {
                    measure(&audio[i..=i], &sp.chapter_title)?
                }
                Loudness::Off => 0.0,
                _ => clip_gain,
            };
        }
    }
    Ok(gains)
}

/// Integrated loudness of the concatenated audio pieces; `None` if ebur128
/// reported nothing.
fn measure_loudness(
    tools: &Tools,
    items: &[(PathBuf, f64)],
    dir: &Path,
    cancel: &CancelToken,
    log: &LogSink,
) -> Result<Option<f64>> {
    let mut cmd = ffmpeg::ffmpeg_base(tools);
    if let [(file, _)] = items {
        cmd = cmd.arg("-i").arg(file);
    } else {
        let list = dir.join("loudness.ffconcat");
        std::fs::write(&list, plan::concat_list(items))?;
        cmd = cmd.args(["-f", "concat", "-safe", "0", "-i"]).arg(list);
    }
    let cmd = cmd.args(["-af", "ebur128=framelog=quiet", "-f", "null", "-"]);
    let o = ffmpeg::run_checked(
        &cmd,
        RunOpts {
            cancel: Some(cancel),
            log: Some(log),
            quiet_stderr: true,
            ..Default::default()
        },
    )?;
    Ok(parse_ebur128(&o.stderr))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EncodingMode;

    fn plan_with(out: &Path, est: u64) -> ExportPlan {
        ExportPlan {
            encoding: EncodingMode::Smart,
            signatures: Vec::new(),
            target_idx: None,
            clips: Vec::new(),
            outputs: vec![OutputPlan {
                path: out.join("x.mp4"),
                segments: Vec::new(),
                timescale: None,
                creation_time: None,
                video_codec: "h264".into(),
                est_bytes: est,
                est_temp_bytes: 1000,
            }],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn refuses_when_the_output_does_not_fit() {
        let dir = std::env::temp_dir();
        let log: LogSink = std::sync::Arc::new(|_| {});
        let opts = ExecOptions::default();
        assert!(prepare_space(&plan_with(&dir, 1_000_000), &opts, &log).is_ok());
        match prepare_space(&plan_with(&dir, u64::MAX / 4), &opts, &log) {
            Err(Error::NoSpace(m)) => assert!(m.contains("Not enough disk space")),
            other => panic!("expected NoSpace, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn ebur128_summary_and_gains() {
        let err = "[Parsed_ebur128_0 @ 0x1] t: 4.9 M: -52.0 S: -51.9 I: -51.8 LUFS\n\
                   [Parsed_ebur128_0 @ 0x1] Summary:\n\n  Integrated loudness:\n    I:         -51.8 LUFS\n    Threshold: -61.8 LUFS\n\n  Loudness range:\n    LRA:         0.0 LU\n";
        assert_eq!(parse_ebur128(err), Some(-51.8));
        assert_eq!(parse_ebur128("nothing"), None);
        assert_eq!(loudness_gain(Some(-23.0), -14.0), 9.0);
        assert_eq!(loudness_gain(Some(-51.8), -14.0), 20.0, "clamped");
        assert_eq!(loudness_gain(Some(-70.0), -14.0), 0.0, "silence");
        assert_eq!(loudness_gain(None, -14.0), 0.0);
        assert_eq!(loudness_gain(Some(-3.0), -14.0), -11.0);
    }

    #[test]
    fn chapters_per_clip_use_the_clip_title_and_merge_its_segments() {
        use crate::plan::{Piece, SegmentPlan};
        let seg = |clip: usize, seg: usize, d: f64, title: &str| SegmentPlan {
            clip_idx: clip,
            seg_idx: seg,
            src: PathBuf::from(format!("c{clip}.mp4")),
            video_stream: 0,
            start: 0.0,
            end: d,
            pieces: vec![Piece {
                start: 0.0,
                end: d,
                out_duration: d,
                kind: PieceKind::Copy {
                    seek: 0.0,
                    packets: 1,
                    bsf: None,
                    delay: 0.0,
                    drop_leading: 0,
                },
            }],
            audio_streams: Vec::new(),
            chapter_title: format!("{title} – {}", seg + 1),
            clip_title: title.into(),
        };
        let mut plan = plan_with(Path::new("."), 0);
        plan.outputs[0].segments = vec![
            seg(0, 0, 10.0, "Intro"),
            seg(0, 2, 5.0, "Intro"),
            seg(1, 0, 20.0, "Match"),
        ];
        let out = &plan.outputs[0];
        let ch = build_chapters(out, Chapters::PerClip);
        assert_eq!(ch.len(), 2);
        assert_eq!((ch[0].start, ch[0].end, ch[0].title.as_str()), (0.0, 15.0, "Intro"));
        assert_eq!((ch[1].start, ch[1].end, ch[1].title.as_str()), (15.0, 35.0, "Match"));
        let ch = build_chapters(out, Chapters::PerSegment);
        assert_eq!(ch.len(), 3);
        assert_eq!(ch[1].title, "Intro – 3");
        assert!(build_chapters(out, Chapters::None).is_empty());
    }
}
