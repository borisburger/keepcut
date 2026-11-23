//! Project + MediaInfo + settings → [`ExportPlan`] (pure; no processes run here),
//! and the ffmpeg command builders for each job (§6.0–6.7).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::compat::{self, Signature};
use crate::encoders::{self, EncoderSet, Role, VideoEncode};
use crate::error::{Error, Result};
use crate::ffmpeg::{Cmd, Tools, ffmpeg_base};
use crate::model::{
    AudioTracks, ContentType, EncodingMode, OutputMode, Project, Quality, Segment, Soundtrack,
    VideoCodec,
};
use crate::probe::{EPS, MediaInfo, VideoStream};
use crate::timefmt::ff_secs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPlan {
    pub encoding: EncodingMode,
    /// Distinct signatures of enabled clips with their kept duration (joined mode).
    pub signatures: Vec<(Signature, f64)>,
    pub target_idx: Option<usize>,
    pub clips: Vec<ClipVerdict>,
    pub outputs: Vec<OutputPlan>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipVerdict {
    pub clip_idx: usize,
    /// Stream copy possible (Smart mode).
    pub copyable: bool,
    pub reasons: Vec<String>,
    pub soft_warnings: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputPlan {
    pub path: PathBuf,
    pub segments: Vec<SegmentPlan>,
    /// `-video_track_timescale` for the final MP4.
    pub timescale: Option<i64>,
    pub creation_time: Option<String>,
    /// Video codec of the output (`h264`, `hevc`, `av1`).
    pub video_codec: String,
    /// Rough size of the finished file (bytes), for the plan summary and the
    /// free-space check (§6.10, §6.14).
    #[serde(default)]
    pub est_bytes: u64,
    /// Rough peak temp space while producing this output (intermediates + FLAC).
    #[serde(default)]
    pub est_temp_bytes: u64,
    /// Audio that replaces the clips' sound (joined output only).
    #[serde(default)]
    pub soundtrack: Option<Soundtrack>,
}

impl OutputPlan {
    /// Intermediate container: MPEG-TS (Annex B, in-band parameter sets) for
    /// H.264/HEVC; NUT for other codecs (MPEG-TS can't carry AV1 here, and NUT
    /// stores codec headers without parsing them, see §3.4).
    pub fn intermediate(&self) -> (&'static str, &'static str) {
        match self.video_codec.as_str() {
            "h264" | "hevc" => ("mpegts", "ts"),
            _ => ("nut", "nut"),
        }
    }
    pub fn duration(&self) -> f64 {
        self.segments.iter().map(SegmentPlan::duration).sum()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentPlan {
    pub clip_idx: usize,
    pub seg_idx: usize,
    pub src: PathBuf,
    /// Absolute stream index of the source video stream.
    pub video_stream: u32,
    pub start: f64,
    pub end: f64,
    pub pieces: Vec<Piece>,
    /// Absolute indices of the audio streams to mix; empty = silence.
    pub audio_streams: Vec<u32>,
    pub chapter_title: String,
    /// Chapter title of the source clip (per-clip chapters).
    #[serde(default)]
    pub clip_title: String,
}

impl SegmentPlan {
    /// Duration of the produced video (and therefore of its audio).
    pub fn duration(&self) -> f64 {
        self.pieces.iter().map(|p| p.out_duration).sum()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Piece {
    /// Source range `[start, end)` in relative seconds.
    pub start: f64,
    pub end: f64,
    /// Duration of the produced piece (= `end - start` unless the frame rate changes).
    pub out_duration: f64,
    pub kind: PieceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncodeRole {
    Head,
    Tail,
    Whole,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PieceKind {
    Copy {
        seek: f64,
        packets: usize,
        bsf: Option<String>,
        /// Decode delay of the source at the start keyframe (pts - dts), seconds.
        delay: f64,
        /// Number of leading pictures after the start keyframe to drop (open GOP).
        drop_leading: usize,
    },
    Encode {
        seek: Option<f64>,
        frames: usize,
        filter: Option<String>,
        /// Constant output frame rate (`None` = passthrough timing).
        fps: Option<u32>,
        role: EncodeRole,
        enc: Box<VideoEncode>,
    },
}

impl Piece {
    pub fn is_copy(&self) -> bool {
        matches!(self.kind, PieceKind::Copy { .. })
    }
}

/// Segments of a clip that will be exported (enabled clip; `None` = whole clip).
pub fn clip_segments(project_clip: &crate::model::Clip, info: &MediaInfo) -> Vec<Segment> {
    if !project_clip.enabled {
        return Vec::new();
    }
    let end = info.video_end();
    match &project_clip.segments {
        None => vec![Segment::new(0.0, end)],
        Some(segs) => segs
            .iter()
            .map(|s| {
                // snap to frames; clamp to the clip
                let st = info.frames.snap(s.start.clamp(0.0, end));
                let en = info.frames.snap(s.end.clamp(0.0, end));
                Segment {
                    start: st,
                    end: en,
                    ..s.clone()
                }
            })
            .filter(|s| s.end - s.start > EPS)
            .collect(),
    }
}

fn kept_duration(segs: &[Segment]) -> f64 {
    segs.iter().map(Segment::duration).sum()
}

fn is_screen_content(info: &MediaInfo, v: &VideoStream) -> bool {
    let enc = info.encoder.as_deref().unwrap_or("").to_ascii_lowercase();
    if enc.contains("bandicam") || enc.contains("obs") {
        return true;
    }
    let r = v.r_frame_rate.as_f64();
    let avg = v.avg_frame_rate.as_f64();
    r > 0.0 && r <= 30.0 && avg > 0.0 && (r - avg).abs() / r > 0.02
}

/// Builds the export plan. `infos[i]` belongs to `project.clips[i]`.
pub fn build(project: &Project, infos: &[MediaInfo], encoders: &EncoderSet) -> Result<ExportPlan> {
    if infos.len() != project.clips.len() {
        return Err(Error::Plan(
            "probe results do not match the clip list".into(),
        ));
    }
    let s = &project.export;
    let mut warnings = Vec::new();

    // Enabled clips with video and at least one kept segment
    let mut active: Vec<(usize, Vec<Segment>)> = Vec::new();
    for (i, (clip, info)) in project.clips.iter().zip(infos).enumerate() {
        if info.video.is_none() {
            if clip.enabled {
                warnings.push(format!("{}: no video stream; skipped", info.path.display()));
            }
            continue;
        }
        let segs = clip_segments(clip, info);
        if !segs.is_empty() {
            active.push((i, segs));
        }
    }
    if active.is_empty() {
        return Err(Error::Plan(
            "nothing to export (no enabled clips with kept segments)".into(),
        ));
    }
    let video = |i: usize| infos[i].video.as_ref().expect("checked above");
    let sig_of = Signature::of;

    // Target signature (joined mode)
    let sig_input: Vec<(Signature, f64)> = active
        .iter()
        .map(|(i, segs)| (sig_of(video(*i)), kept_duration(segs)))
        .collect();
    let (signatures, mut target_idx) = compat::choose_target(&sig_input);
    if let Some(i) = s
        .target_override
        .as_ref()
        .and_then(|o| signatures.iter().position(|(sg, _)| sg == o))
    {
        target_idx = i;
    }
    let joined = s.output_mode == OutputMode::Joined;
    if project.soundtrack.is_some() && !joined {
        warnings.push(
            "the soundtrack is used only for a joined output; these files keep their own audio"
                .into(),
        );
    }
    let target_sig = &signatures[target_idx].0;
    // A reference stream that has the target signature (source of profile/level/colors)
    let target_ref = active
        .iter()
        .map(|(i, _)| *i)
        .find(|i| &sig_of(video(*i)) == target_sig)
        .expect("target comes from an active clip");

    // Verdicts
    let mut verdicts = Vec::new();
    for (i, _) in &active {
        let v = video(*i);
        let (reasons, soft) = if joined {
            let reasons = sig_of(v).mismatch_reasons(target_sig);
            // soft-key notes only matter for clips that are actually copied
            let soft = if reasons.is_empty() {
                compat::soft_warnings(v, video(target_ref))
            } else {
                Vec::new()
            };
            (reasons, soft)
        } else {
            (Vec::new(), Vec::new())
        };
        let copyable = reasons.is_empty() && s.encoding == EncodingMode::Smart;
        if v.is_hdr() && joined && !video(target_ref).is_hdr() {
            warnings.push(format!(
                "{}: HDR source joined into SDR output; colors will look washed out",
                infos[*i].file_stem()
            ));
        }
        verdicts.push(ClipVerdict {
            clip_idx: *i,
            copyable,
            reasons,
            soft_warnings: soft,
            summary: String::new(),
        });
    }

    // Group segments into outputs
    // (output path, [(clip index, segment index, segment)])
    type Group = (PathBuf, Vec<(usize, usize, Segment)>);
    let mut groups: Vec<Group> = Vec::new();
    let out_dir_for = |i: usize| -> PathBuf {
        s.output_dir.clone().unwrap_or_else(|| {
            infos[i]
                .path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default()
        })
    };
    match s.output_mode {
        OutputMode::Joined => {
            let first = active[0].0;
            let name = s
                .output_name
                .clone()
                .unwrap_or_else(|| format!("{} - joined.mp4", infos[first].file_stem()));
            let items = active
                .iter()
                .flat_map(|(i, segs)| {
                    segs.iter()
                        .enumerate()
                        .map(move |(k, sg)| (*i, k, sg.clone()))
                })
                .collect();
            groups.push((out_dir_for(first).join(name), items));
        }
        OutputMode::PerClip => {
            for (i, segs) in &active {
                let p = out_dir_for(*i).join(format!("{} - cut.mp4", infos[*i].file_stem()));
                groups.push((
                    p,
                    segs.iter()
                        .enumerate()
                        .map(|(k, sg)| (*i, k, sg.clone()))
                        .collect(),
                ));
            }
        }
        OutputMode::PerSegment => {
            for (i, segs) in &active {
                for (k, sg) in segs.iter().enumerate() {
                    let p = out_dir_for(*i).join(format!(
                        "{} - part {:02}.mp4",
                        infos[*i].file_stem(),
                        k + 1
                    ));
                    groups.push((p, vec![(*i, k, sg.clone())]));
                }
            }
        }
    }

    // Unique, non-source output paths
    let sources: Vec<PathBuf> = infos.iter().map(|m| m.path.clone()).collect();
    let mut taken: Vec<PathBuf> = Vec::new();
    for (p, _) in groups.iter_mut() {
        *p = unique_path(p, &sources, &taken);
        taken.push(p.clone());
    }

    let mut outputs = Vec::new();
    let mut full_desc: Vec<(usize, String)> = Vec::new();
    for (path, items) in groups {
        let mut segments = Vec::new();
        // Full re-encode: one set of encoder settings per output, from the output's
        // reference clip (joined → the target; separate files → the clip itself), so
        // every piece of an output has the same pixel format, profile and GOP.
        let ref_clip = if joined { target_ref } else { items[0].0 };
        let full = (s.encoding == EncodingMode::Full).then(|| {
            let r = video(ref_clip);
            let (w, h) = s.full.resolution.apply(r.width, r.height);
            let mut enc = full_encode(&infos[ref_clip], r, project, encoders, w, h);
            // no B-frames when any clip of the output is screen content, not only the
            // reference: across a screen recording's long frame gaps they cut the MP4
            // track end short (a camera + screen join lost up to 0.3 s, every encoder)
            let screen = |ci: usize| match s.full.content {
                ContentType::Screen => true,
                ContentType::Camera => false,
                ContentType::Auto => is_screen_content(&infos[ci], video(ci)),
            };
            if items.iter().any(|(ci, _, _)| screen(*ci)) {
                enc.bframes = 0;
            }
            enc
        });
        // per-output reference stream: joined → target; separate → the clip itself
        for (ci, si, seg) in &items {
            let info = &infos[*ci];
            let v = video(*ci);
            let reference = if joined { video(target_ref) } else { v };
            let copyable =
                s.encoding == EncodingMode::Smart && (!joined || sig_of(v) == *target_sig);
            let pieces = match s.encoding {
                EncodingMode::Smart if copyable => smart_pieces(info, v, seg, reference, encoders)?,
                EncodingMode::Smart => {
                    let enc = mismatch_encode(
                        info,
                        v,
                        reference,
                        s.smart_encoder.as_deref(),
                        &s.smart_quality,
                        encoders,
                    );
                    let filter = Some(scale_filter(
                        reference.width,
                        reference.height,
                        &reference.pix_fmt,
                    ));
                    vec![encode_piece(
                        info,
                        seg.start,
                        seg.end,
                        EncodeRole::Whole,
                        enc,
                        filter,
                        None,
                    )]
                }
                EncodingMode::Full => {
                    let enc = full.clone().expect("set in Full mode");
                    let (w, h) = (enc.width, enc.height);
                    let filter = if (v.width, v.height) != (w, h) || v.sar != "1:1" {
                        Some(scale_filter(w, h, &enc.pix_fmt))
                    } else {
                        None
                    };
                    if !full_desc.iter().any(|(c, _)| c == ci) {
                        let size = if (v.width, v.height) == (w, h) {
                            format!("{w}×{h}")
                        } else {
                            format!("{}×{} scaled to {w}×{h}", v.width, v.height)
                        };
                        let fps = s.full.fps.map_or(String::new(), |f| format!(", {f} fps"));
                        full_desc.push((
                            *ci,
                            format!(
                                "re-encoded: {size}{fps}, {} ({})",
                                enc.codec_label(),
                                enc.encoder
                            ),
                        ));
                    }
                    vec![encode_piece(
                        info,
                        seg.start,
                        seg.end,
                        EncodeRole::Whole,
                        enc,
                        filter,
                        s.full.fps,
                    )]
                }
            };
            let audio_streams = match s.audio_tracks {
                AudioTracks::MixAll => info.audio.iter().map(|a| a.index).collect(),
                AudioTracks::FirstOnly => info.audio.iter().take(1).map(|a| a.index).collect(),
            };
            let clip = &project.clips[*ci];
            // titles are stored as typed in the UI
            let trimmed = |t: &Option<String>| {
                t.as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
            };
            let stem = trimmed(&clip.chapter_title).unwrap_or_else(|| info.file_stem());
            let chapter_title = trimmed(&seg.chapter_title).unwrap_or_else(|| {
                let kept = clip.segments.as_ref().map_or(1, Vec::len);
                crate::chapters::default_segment_title(&stem, *si, kept)
            });
            segments.push(SegmentPlan {
                clip_idx: *ci,
                seg_idx: *si,
                src: info.path.clone(),
                video_stream: v.index,
                start: seg.start,
                end: seg.end,
                pieces,
                audio_streams,
                chapter_title,
                clip_title: stem.clone(),
            });
        }
        // timescale: the common source time base, else 90 kHz; CFR output lets the muxer choose
        let tbs: Vec<_> = items
            .iter()
            .map(|(ci, _, _)| video(*ci).time_base)
            .collect();
        let timescale = if s.encoding == EncodingMode::Full && s.full.fps.is_some() {
            None
        } else if tbs.iter().all(|t| *t == tbs[0]) && tbs[0].num == 1 {
            Some(tbs[0].den)
        } else {
            Some(90000)
        };
        let video_codec = match s.encoding {
            EncodingMode::Full => match s.full.codec {
                VideoCodec::H264 => "h264",
                VideoCodec::Hevc => "hevc",
                VideoCodec::Av1 => "av1",
            }
            .to_string(),
            EncodingMode::Smart if joined => target_sig.codec_name.clone(),
            EncodingMode::Smart => video(items[0].0).codec_name.clone(),
        };
        let creation_time = infos[items[0].0].creation_time.clone();
        let mut out = OutputPlan {
            path,
            segments,
            timescale,
            creation_time,
            video_codec,
            est_bytes: 0,
            est_temp_bytes: 0,
            soundtrack: project.soundtrack.clone().filter(|_| joined),
        };
        (out.est_bytes, out.est_temp_bytes) = estimate(&out, infos, s.audio_bitrate_kbps);
        outputs.push(out);
    }

    // Verdict summaries
    for vd in &mut verdicts {
        let segs: Vec<&SegmentPlan> = outputs
            .iter()
            .flat_map(|o| &o.segments)
            .filter(|sp| sp.clip_idx == vd.clip_idx)
            .collect();
        vd.summary = match full_desc.iter().find(|(c, _)| *c == vd.clip_idx) {
            Some((_, d)) => d.clone(),
            None => summarize(&segs, vd),
        };
    }

    Ok(ExportPlan {
        encoding: s.encoding,
        signatures,
        target_idx: Some(target_idx),
        clips: verdicts,
        outputs,
        warnings,
    })
}

/// Bytes per second of a source's video (stream bit rate, else the whole file's).
fn video_rate(info: &MediaInfo) -> f64 {
    info.video
        .as_ref()
        .and_then(|v| v.bit_rate)
        .map(|b| b as f64 / 8.0)
        .or_else(|| (info.duration > 0.0).then(|| info.file_size as f64 / info.duration))
        .unwrap_or(0.0)
}

/// Relative bits needed for the same quality (H.264 = 1).
fn codec_cost(codec: &str) -> f64 {
    match codec {
        "hevc" => 0.7,
        "av1" => 0.55,
        _ => 1.0,
    }
}

/// (output bytes, peak temp bytes). Copied parts cost what they cost in the
/// source; re-encoded parts at "match source" are assumed to cost the source's
/// rate scaled by pixel count and codec efficiency; a bitrate setting is exact.
fn estimate(out: &OutputPlan, infos: &[MediaInfo], audio_kbps: u32) -> (u64, u64) {
    let mut video = 0.0;
    for sp in &out.segments {
        let info = &infos[sp.clip_idx];
        let rate = video_rate(info);
        let Some(v) = info.video.as_ref() else {
            continue;
        };
        for p in &sp.pieces {
            let d = p.end - p.start;
            video += match &p.kind {
                PieceKind::Copy { .. } => rate * d,
                PieceKind::Encode { enc, .. } => match enc.quality {
                    Quality::BitrateKbps(b) => b as f64 * 125.0 * p.out_duration,
                    _ => {
                        let px = (enc.width.max(1) as f64 * enc.height.max(1) as f64)
                            / (v.width.max(1) as f64 * v.height.max(1) as f64);
                        let out_codec =
                            encoders::codec_of_encoder(&enc.encoder).map_or("h264", |c| match c {
                                VideoCodec::H264 => "h264",
                                VideoCodec::Hevc => "hevc",
                                VideoCodec::Av1 => "av1",
                            });
                        rate * d * px * codec_cost(out_codec) / codec_cost(&v.codec_name)
                    }
                },
            };
        }
    }
    let dur = out.duration();
    let audio = audio_kbps as f64 * 125.0 * dur;
    let flac = 100_000.0 * dur;
    // MPEG-TS intermediates carry ~5% container overhead
    ((video + audio) as u64, (video * 1.05 + flac) as u64)
}

fn summarize(segs: &[&SegmentPlan], vd: &ClipVerdict) -> String {
    let pieces: Vec<&Piece> = segs.iter().flat_map(|s| &s.pieces).collect();
    if !vd.copyable {
        return format!("fully re-encoded: {}", vd.reasons.join("; "));
    }
    let enc: Vec<&&Piece> = pieces.iter().filter(|p| !p.is_copy()).collect();
    if enc.is_empty() {
        "copied".into()
    } else {
        let t: f64 = enc.iter().map(|p| p.end - p.start).sum();
        format!(
            "copied; {} part(s) re-encoded ({:.1} s total)",
            enc.len(),
            t
        )
    }
}

fn unique_path(p: &Path, sources: &[PathBuf], taken: &[PathBuf]) -> PathBuf {
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = p
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mp4".into());
    let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
    let same = |a: &Path, b: &Path| {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    };
    let mut n = 1;
    loop {
        let cand = if n == 1 {
            p.to_path_buf()
        } else {
            dir.join(format!("{stem} ({n}).{ext}"))
        };
        let clash = cand.exists()
            || sources.iter().any(|s| same(s, &cand))
            || taken.iter().any(|t| same(t, &cand));
        if !clash {
            return cand;
        }
        n += 1;
    }
}

/// Smart-cut pieces for a segment of a clip that matches the output target (§6.3).
fn smart_pieces(
    info: &MediaInfo,
    v: &VideoStream,
    seg: &Segment,
    reference: &VideoStream,
    encoders: &EncoderSet,
) -> Result<Vec<Piece>> {
    let fi = &info.frames;
    let (s, e) = (seg.start, seg.end);
    let head_enc = || head_encode(v, reference, encoders);
    let at_end = (e - fi.end).abs() < EPS;

    let whole = || {
        Ok(vec![encode_piece(
            info,
            s,
            e,
            EncodeRole::Whole,
            head_enc(),
            None,
            None,
        )])
    };
    let Some(k1) = fi.next_key_at_or_after(s).filter(|k| *k < e - EPS) else {
        return whole();
    };
    // Open GOP: the start keyframe's leading pictures (decoded after it, shown
    // before it) reference the previous GOP; they are dropped from the copy and
    // covered by the head instead.
    let (lead1, _) = fi.leading(k1);

    // End of the copied body: `body_end` in presentation time, `end_pos` = the
    // frame whose packet ends the copy in decode order.
    let (body_end, end_pos) = if v.has_b_frames == 0 || at_end {
        (e, e)
    } else {
        // Copy up to (excluding) a keyframe's packet. Its leading pictures come
        // after it in decode order, so they are missing from the copy and start
        // the re-encoded tail.
        let ke = if fi.is_key_at(e) {
            e
        } else {
            fi.last_key_at_or_before(e).unwrap_or(k1)
        };
        let (_, lead_start) = fi.leading(ke);
        (lead_start.min(ke), ke)
    };
    if body_end - k1 <= EPS {
        return whole();
    }

    let mut pieces = Vec::new();
    if k1 - s > EPS {
        pieces.push(encode_piece(
            info,
            s,
            k1,
            EncodeRole::Head,
            head_enc(),
            None,
            None,
        ));
    }
    let bsf = match v.codec_name.as_str() {
        "h264" if !info.format_name.contains("mpegts") => Some("h264_mp4toannexb".to_string()),
        "hevc" if !info.format_name.contains("mpegts") => Some("hevc_mp4toannexb".to_string()),
        _ => None,
    };
    let i = fi.idx_at_or_after(k1);
    let half = (fi.time_of(i + 1) - k1) / 2.0;
    pieces.push(Piece {
        start: k1,
        end: body_end,
        out_duration: body_end - k1,
        kind: PieceKind::Copy {
            seek: k1 + half.min(0.0005),
            packets: fi.decode_count(k1, end_pos) - lead1,
            bsf,
            delay: fi.frames.get(i).map_or(0.0, |f| f.t - f.dts),
            drop_leading: lead1,
        },
    });
    if e - body_end > EPS {
        pieces.push(encode_piece(
            info,
            body_end,
            e,
            EncodeRole::Tail,
            head_enc(),
            None,
            None,
        ));
    }
    Ok(pieces)
}

fn encode_piece(
    info: &MediaInfo,
    a: f64,
    b: f64,
    role: EncodeRole,
    enc: VideoEncode,
    filter: Option<String>,
    fps: Option<u32>,
) -> Piece {
    let fi = &info.frames;
    let seek = fi.prev_frame_time(a).map(|p| (p + a) / 2.0);
    let (frames, out_duration) = match fps {
        None => (fi.count_in(a, b), b - a),
        Some(f) => {
            let n = (((b - a) * f as f64).round() as usize).max(1);
            (n, n as f64 / f as f64)
        }
    };
    Piece {
        start: a,
        end: b,
        out_duration,
        kind: PieceKind::Encode {
            seek,
            frames,
            filter,
            fps,
            role,
            enc: Box::new(enc),
        },
    }
}

fn base_encode(
    encoder: &str,
    role: Role,
    quality: Quality,
    like: &VideoStream,
    width: u32,
    height: u32,
) -> VideoEncode {
    let codec = encoders::codec_of_name(&like.codec_name).unwrap_or(VideoCodec::H264);
    VideoEncode {
        encoder: encoder.to_string(),
        role,
        quality,
        pix_fmt: like.pix_fmt.clone(),
        profile: like.profile.as_deref().and_then(encoders::ffmpeg_profile),
        level: like.level.and_then(|l| encoders::ffmpeg_level(codec, l)),
        color_primaries: like.color_primaries.clone(),
        color_trc: like.color_transfer.clone(),
        colorspace: like.color_space.clone(),
        color_range: like.color_range.clone(),
        bframes: if like.has_b_frames == 0 { 0 } else { 2 },
        gop: None,
        tune: None,
        screen: false,
        width,
        height,
    }
}

/// Head/tail encode: software encoder, parameters copied from the target.
fn head_encode(_src: &VideoStream, reference: &VideoStream, set: &EncoderSet) -> VideoEncode {
    let codec = encoders::codec_of_name(&reference.codec_name).unwrap_or(VideoCodec::H264);
    base_encode(
        set.software(codec),
        Role::Head,
        Quality::MatchSource,
        reference,
        reference.width,
        reference.height,
    )
}

/// Smart mode: whole re-encode of a clip that doesn't match the target.
fn mismatch_encode(
    info: &MediaInfo,
    src: &VideoStream,
    reference: &VideoStream,
    choice: Option<&str>,
    quality: &Quality,
    encoders: &EncoderSet,
) -> VideoEncode {
    let codec = encoders::codec_of_name(&reference.codec_name).unwrap_or(VideoCodec::H264);
    let screen = is_screen_content(info, src);
    let enc = pick_encoder(codec, choice, screen, reference.height, encoders);
    let mut ve = base_encode(
        &enc,
        Role::Long,
        quality.clone(),
        reference,
        reference.width,
        reference.height,
    );
    if !encoders::is_software(&enc) {
        ve.level = None;
    }
    ve.screen = screen;
    ve
}

/// Encoder for a whole re-encode: the user's choice if it works and fits the
/// codec; Auto = software for screen recordings below 4K (sharper text, VFR
/// friendly), else the first working hardware encoder (software x265 at 4K is
/// very slow, §6.14), else software.
pub fn pick_encoder(
    codec: VideoCodec,
    choice: Option<&str>,
    screen: bool,
    height: u32,
    encoders: &EncoderSet,
) -> String {
    let sw = encoders.software(codec);
    // a choice made for the other codec means the same family (NVENC → NVENC)
    let choice = choice.and_then(|c| encoders::counterpart(c, codec));
    match choice {
        Some(e) if encoders.has(&e) => e,
        _ if screen && height < 2160 => sw.to_string(),
        _ => encoders.first_hardware(codec).unwrap_or(sw).to_string(),
    }
}

/// Full re-encode settings for an output whose reference clip is `src`.
fn full_encode(
    info: &MediaInfo,
    src: &VideoStream,
    project: &Project,
    encoders: &EncoderSet,
    out_w: u32,
    out_h: u32,
) -> VideoEncode {
    let f = &project.export.full;
    let screen = match f.content {
        ContentType::Screen => true,
        ContentType::Camera => false,
        ContentType::Auto => is_screen_content(info, src),
    };
    let enc = pick_encoder(f.codec, f.encoder.as_deref(), screen, out_h, encoders);
    let ten_bit = f.codec != VideoCodec::H264 && src.bit_depth() >= 10;
    let fps_for_gop = f
        .fps
        .unwrap_or_else(|| src.r_frame_rate.as_f64().round().max(1.0) as u32);
    VideoEncode {
        encoder: enc,
        role: Role::Long,
        quality: f.quality.clone(),
        pix_fmt: if ten_bit {
            "yuv420p10le".into()
        } else {
            "yuv420p".into()
        },
        profile: Some(match (f.codec, ten_bit) {
            (VideoCodec::H264, _) => "high".into(),
            (VideoCodec::Hevc, true) => "main10".into(),
            (VideoCodec::Hevc, false) => "main".into(),
            (VideoCodec::Av1, _) => "main".into(),
        }),
        level: None,
        color_primaries: src.color_primaries.clone(),
        color_trc: src.color_transfer.clone(),
        colorspace: src.color_space.clone(),
        color_range: src.color_range.clone(),
        // no B-frames for screen content: they save ~3% there, but on VFR footage the
        // decode delay spans long frame gaps and the MP4 track end came out a frame short
        bframes: if screen { 0 } else { 2 },
        // keyframe interval: 2 s for camera footage; 10 s for screen content, where
        // keyframes are expensive (2 s GOPs measured +64% size on Bandicam footage)
        gop: Some(fps_for_gop * if screen { 10 } else { 2 }),
        tune: None,
        screen,
        width: out_w,
        height: out_h,
    }
}

/// Scale + pad to `w×h`, keeping the aspect ratio (§6.5).
pub fn scale_filter(w: u32, h: u32, pix_fmt: &str) -> String {
    format!(
        "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,format={pix_fmt}"
    )
}

// ---------------------------------------------------------------------------
// Command builders
// ---------------------------------------------------------------------------

/// Video piece → MPEG-TS intermediate (§6.4). `dts_shift` (seconds) lowers the DTS of
/// copied packets so every piece of an output has the same decode delay.
pub fn piece_cmd(
    tools: &Tools,
    sp: &SegmentPlan,
    piece: &Piece,
    out: &Path,
    format: &str,
    dts_shift: f64,
) -> Cmd {
    let mut c = ffmpeg_base(tools);
    let mut chain: Vec<String> = Vec::new();
    match &piece.kind {
        PieceKind::Copy {
            seek,
            packets,
            bsf,
            drop_leading,
            ..
        } => {
            c = c.arg("-ss").arg(ff_secs(*seek)).arg("-i").arg(&sp.src);
            c = c
                .arg("-map")
                .arg(format!("0:{}", sp.video_stream))
                .args(["-an", "-sn", "-dn", "-c", "copy"]);
            c = c.arg("-frames:v").arg(packets.to_string());
            chain.extend(bsf.iter().cloned());
            if *drop_leading > 0 {
                chain.push(format!(r"noise=drop=gte(n\,1)*lte(n\,{drop_leading})"));
            }
            if dts_shift > 1e-7 {
                chain.push(setts_shift(dts_shift));
            }
            c = c.args(["-avoid_negative_ts", "make_zero"]);
        }
        PieceKind::Encode {
            seek,
            frames,
            filter,
            fps,
            enc,
            ..
        } => {
            if enc.encoder.ends_with("_vaapi") {
                c = c.args(["-vaapi_device", "/dev/dri/renderD128"]);
            }
            if let Some(s) = seek {
                c = c.arg("-ss").arg(ff_secs(*s));
            }
            c = c.arg("-noautorotate").arg("-i").arg(&sp.src);
            c = c
                .arg("-map")
                .arg(format!("0:{}", sp.video_stream))
                .args(["-an", "-sn", "-dn"]);
            c = c.arg("-frames:v").arg(frames.to_string());
            match fps {
                None => c = c.args(["-fps_mode", "passthrough", "-enc_time_base:v", "demux"]),
                Some(f) => c = c.args(["-fps_mode", "cfr", "-r"]).arg(f.to_string()),
            }
            let mut vf: Vec<String> = filter.iter().cloned().collect();
            if enc.encoder.ends_with("_vaapi") {
                vf.push("format=nv12,hwupload".into());
            }
            if !vf.is_empty() {
                c = c.arg("-vf").arg(vf.join(","));
            }
            c = c.args(enc.args());
        }
    }
    // NUT keeps each source's time base (1/15000, 1/60000, …), and the concat demuxer
    // squashes the packets of a file whose time base differs from the first one's into
    // microseconds. One time base for every piece, as MPEG-TS has: 90 kHz.
    if format == "nut" {
        chain.push(NUT_TIME_BASE.into());
    }
    if !chain.is_empty() {
        c = c.arg("-bsf:v").arg(chain.join(","));
    }
    c.arg("-f").arg(format).arg(out)
}

/// Bitstream filter giving a NUT intermediate the common 90 kHz time base.
const NUT_TIME_BASE: &str = "setts=time_base=1/90000";

/// `setts` filter lowering DTS by `secs` (PTS unchanged).
pub fn setts_shift(secs: f64) -> String {
    format!("setts=pts=PTS:dts=DTS-{secs:.9}/TB")
}

/// Remuxes an intermediate with its DTS lowered by `secs`.
pub fn dts_shift_cmd(tools: &Tools, input: &Path, secs: f64, out: &Path, format: &str) -> Cmd {
    ffmpeg_base(tools)
        .arg("-i")
        .arg(input)
        .args(["-map", "0:v:0", "-c", "copy", "-bsf:v"])
        .arg(setts_shift(secs))
        .arg("-f")
        .arg(format)
        .arg(out)
}

/// Extracts a segment's audio as 48 kHz stereo FLAC of exactly `dur` seconds (§6.6).
pub fn audio_piece_cmd(tools: &Tools, sp: &SegmentPlan, dur: f64, out: &Path) -> Cmd {
    let mut c = ffmpeg_base(tools);
    let d = ff_secs(dur);
    if sp.audio_streams.is_empty() {
        c = c
            .args(["-f", "lavfi", "-t"])
            .arg(&d)
            .args(["-i", "anullsrc=r=48000:cl=stereo"]);
        c = c.args(["-map", "0:a:0"]);
    } else {
        if sp.start > 0.0 {
            c = c.arg("-ss").arg(ff_secs(sp.start));
        }
        c = c.arg("-t").arg(ff_secs(dur + 1.0)).arg("-i").arg(&sp.src);
        let inputs: String = sp
            .audio_streams
            .iter()
            .map(|i| format!("[0:{i}]"))
            .collect();
        let mix = if sp.audio_streams.len() > 1 {
            format!(
                "{inputs}amix=inputs={}:normalize=0:duration=longest,",
                sp.audio_streams.len()
            )
        } else {
            inputs
        };
        let graph = format!(
            "{mix}aresample=48000:async=1:first_pts=0,aformat=sample_rates=48000:channel_layouts=stereo,apad=whole_dur={d},atrim=duration={d},asetnsamples=n=4608:p=0[a]"
        );
        c = c.arg("-filter_complex").arg(graph).args(["-map", "[a]"]);
    }
    // identical sample format in every piece, or the concat demuxer stops at a change
    c.args([
        "-vn",
        "-ar",
        "48000",
        "-ac",
        "2",
        "-sample_fmt",
        "s16",
        "-c:a",
        "flac",
    ])
    .arg(out)
}

/// The soundtrack's audio for an output of `dur` seconds as one FLAC piece, in the same
/// format as [`audio_piece_cmd`]: from `offset` in the file, cut at `dur`, padded with
/// silence if the file ends earlier.
pub fn soundtrack_piece_cmd(tools: &Tools, st: &Soundtrack, dur: f64, out: &Path) -> Cmd {
    let mut c = ffmpeg_base(tools);
    let d = ff_secs(dur);
    if st.offset > 0.0 {
        c = c.arg("-ss").arg(ff_secs(st.offset));
    }
    c = c.arg("-t").arg(ff_secs(dur + 1.0)).arg("-i").arg(&st.path);
    let graph = format!(
        "[0:a:0]aresample=48000:async=1:first_pts=0,aformat=sample_rates=48000:channel_layouts=stereo,apad=whole_dur={d},atrim=duration={d},asetnsamples=n=4608:p=0[a]"
    );
    c.arg("-filter_complex")
        .arg(graph)
        .args([
            "-map",
            "[a]",
            "-vn",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-sample_fmt",
            "s16",
            "-c:a",
            "flac",
        ])
        .arg(out)
}

/// A concat-demuxer list with exact `duration` directives.
pub fn concat_list(items: &[(PathBuf, f64)]) -> String {
    let mut s = String::from("ffconcat version 1.0\n");
    for (p, d) in items {
        s.push_str(&format!(
            "file '{}'\nduration {}\n",
            concat_escape(&p.to_string_lossy()),
            ff_secs(*d)
        ));
    }
    s
}

/// `file:` prefix and `'` → `'\''` escaping for concat lists.
pub fn concat_escape(path: &str) -> String {
    format!("file:{}", path.replace('\'', "'\\''"))
}

/// Audio filter of the final mux with loudness normalization (§6.9): a gain per
/// run of segments, `(output start time, gain dB)` in output order, applied in
/// float (so nothing clips before the limiter), then the −1 dBFS limiter.
///
/// The gain switches on 64-sample frames (1.3 ms), with the boundary placed half
/// a frame early so rounding can't leave a whole frame on the wrong side.
/// `latency=1` makes the limiter compensate its look-ahead; without it the audio
/// would come out 5 ms late.
pub fn loudness_filter(gains: &[(f64, f64)]) -> String {
    const HALF_FRAME: f64 = 32.0 / 48000.0;
    let mut runs: Vec<(f64, f64)> = Vec::new();
    for &(t, g) in gains {
        if runs.last().is_none_or(|r| (r.1 - g).abs() > 1e-6) {
            runs.push((t, g));
        }
    }
    let factor = |g: f64| format!("{:.6}", 10f64.powf(g / 20.0));
    let expr = if runs.len() <= 1 {
        factor(runs.first().map_or(0.0, |r| r.1))
    } else {
        (0..runs.len())
            .map(|k| {
                let mut term = factor(runs[k].1);
                if k > 0 {
                    term += &format!("*gte(t,{:.6})", runs[k].0 - HALF_FRAME);
                }
                if let Some(next) = runs.get(k + 1) {
                    term += &format!("*lt(t,{:.6})", next.0 - HALF_FRAME);
                }
                term
            })
            .collect::<Vec<_>>()
            .join("+")
    };
    format!(
        "asetnsamples=n=64:p=0,volume=volume='{expr}':eval=frame,alimiter=limit=0.891:level=disabled:latency=1"
    )
}

/// Final mux of an output (§6.7).
#[allow(clippy::too_many_arguments)]
pub fn final_mux_cmd(
    tools: &Tools,
    out: &OutputPlan,
    video_list: &Path,
    audio_list: &Path,
    chapters: Option<&Path>,
    audio_bitrate_kbps: u32,
    audio_filter: Option<&Path>,
    target: &Path,
) -> Cmd {
    let mut c = ffmpeg_base(tools);
    c = c.args(["-f", "concat", "-safe", "0", "-i"]).arg(video_list);
    c = c.args(["-f", "concat", "-safe", "0", "-i"]).arg(audio_list);
    if let Some(ch) = chapters {
        c = c.arg("-i").arg(ch);
    }
    c = c.args(["-map", "0:v:0", "-map", "1:a:0"]);
    if chapters.is_some() {
        c = c.args(["-map_metadata", "2", "-map_chapters", "2"]);
    } else {
        c = c.args(["-map_metadata", "-1", "-map_chapters", "-1"]);
    }
    c = c
        .args(["-c:v", "copy", "-c:a", "aac", "-b:a"])
        .arg(format!("{audio_bitrate_kbps}k"));
    // MPEG-TS stores no packet durations, so the muxer would guess 1/fps for the
    // last frame; with VFR sources its real duration is often longer. Give the
    // last packet (decode order) the duration that ends the track exactly.
    // AV1: rebuild a valid av1C from the in-band sequence header (Bandicam writes a
    // malformed one that every muxer rejects; §3.4).
    let extract = if out.video_codec == "av1" {
        "extract_extradata,"
    } else {
        ""
    };
    c = c.arg("-bsf:v").arg(format!(
        r"{extract}setts=pts=PTS:dts=DTS:duration=if(eq(NEXT_DTS\,NOPTS)\,{:.6}/TB-(PTS-STARTPTS)\,DURATION)",
        out.duration()
    ));
    if let Some(f) = audio_filter {
        // the graph can be long (one term per gain change): read it from a file
        c = c.arg("-/af").arg(f);
    }
    if let Some(ts) = out.timescale {
        c = c.arg("-video_track_timescale").arg(ts.to_string());
    }
    if let Some(ct) = &out.creation_time {
        c = c.arg("-metadata").arg(format!("creation_time={ct}"));
    }
    c = c
        .arg("-metadata")
        .arg(format!("encoder=Keepcut {}", env!("CARGO_PKG_VERSION")));
    c.args(["-movflags", "+faststart", "-ignore_unknown", "-f", "mp4"])
        .arg(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Clip;
    use crate::probe::{FrameIndex, Rational, parse_packets};

    fn info(path: &str, w: u32, h: u32, packets: &str, bframes: u32) -> MediaInfo {
        let tb = Rational { num: 1, den: 15 };
        let frames: FrameIndex = parse_packets(packets, tb, 0.0);
        MediaInfo {
            path: PathBuf::from(path),
            file_size: 0,
            file_mtime: 0,
            format_name: "mov,mp4,m4a,3gp,3g2,mj2".into(),
            duration: frames.end,
            start_time: 0.0,
            bit_rate: None,
            creation_time: None,
            encoder: Some("Bandicam".into()),
            video: Some(VideoStream {
                index: 0,
                codec_name: "h264".into(),
                profile: Some("High".into()),
                level: Some(40),
                width: w,
                height: h,
                sar: "1:1".into(),
                pix_fmt: "yuv420p".into(),
                color_primaries: None,
                color_transfer: None,
                color_space: None,
                color_range: None,
                field_order: None,
                time_base: tb,
                r_frame_rate: Rational { num: 15, den: 1 },
                avg_frame_rate: Rational { num: 15, den: 1 },
                has_b_frames: bframes,
                rotation: 0,
                bit_rate: None,
                encoder: None,
            }),
            audio: vec![],
            data_streams: 0,
            frames,
        }
    }

    /// 30 frames at 15 fps (pts in 1/15 units), keyframes at 0, 10, 20.
    fn packets() -> String {
        (0..30)
            .map(|i| {
                format!(
                    "pts={i}|dts={i}|duration=1|flags={}\n",
                    if i % 10 == 0 { "K_" } else { "__" }
                )
            })
            .collect()
    }

    type ClipSpec<'a> = (&'a str, Option<Vec<(f64, f64)>>);

    fn project(clips: Vec<ClipSpec<'_>>) -> Project {
        Project {
            version: 1,
            id: uuid::Uuid::nil(),
            clips: clips
                .into_iter()
                .map(|(p, segs)| Clip {
                    id: uuid::Uuid::nil(),
                    path: PathBuf::from(p),
                    file_size: 0,
                    file_mtime: 0,
                    enabled: true,
                    segments: segs
                        .map(|v| v.into_iter().map(|(a, b)| Segment::new(a, b)).collect()),
                    chapter_title: None,
                })
                .collect(),
            export: Default::default(),
            soundtrack: None,
        }
    }

    #[test]
    fn smart_cut_no_bframes() {
        let pk = packets();
        let infos = vec![info("/x/a.mp4", 1920, 1080, &pk, 0)];
        let p = project(vec![("/x/a.mp4", Some(vec![(3.0 / 15.0, 25.0 / 15.0)]))]);
        let plan = build(&p, &infos, &EncoderSet::software_only()).unwrap();
        let pieces = &plan.outputs[0].segments[0].pieces;
        assert_eq!(pieces.len(), 2);
        match &pieces[0].kind {
            PieceKind::Encode {
                frames, role, seek, ..
            } => {
                assert_eq!(*frames, 7);
                assert_eq!(*role, EncodeRole::Head);
                assert!((seek.unwrap() - 2.5 / 15.0).abs() < 1e-9);
            }
            _ => panic!("head expected"),
        }
        match &pieces[1].kind {
            PieceKind::Copy { packets, .. } => assert_eq!(*packets, 15),
            _ => panic!("copy expected"),
        }
        let d = plan.outputs[0].duration();
        assert!((d - 22.0 / 15.0).abs() < 1e-9);
    }

    #[test]
    fn smart_cut_bframes_tail() {
        let pk = packets();
        let infos = vec![info("/x/a.mp4", 1920, 1080, &pk, 2)];
        let p = project(vec![("/x/a.mp4", Some(vec![(3.0 / 15.0, 25.0 / 15.0)]))]);
        let plan = build(&p, &infos, &EncoderSet::software_only()).unwrap();
        let kinds: Vec<String> = plan.outputs[0].segments[0]
            .pieces
            .iter()
            .map(|p| match &p.kind {
                PieceKind::Copy { packets, .. } => format!("copy{packets}"),
                PieceKind::Encode { frames, .. } => format!("enc{frames}"),
            })
            .collect();
        assert_eq!(kinds, ["enc7", "copy10", "enc5"]);
    }

    #[test]
    fn whole_encode_without_keyframe_and_mismatch() {
        let pk = packets();
        let infos = vec![
            info("/x/a.mp4", 1920, 1080, &pk, 0),
            info("/x/b.mp4", 1280, 720, &pk, 0),
        ];
        let p = project(vec![
            ("/x/a.mp4", Some(vec![(1.0 / 15.0, 8.0 / 15.0)])),
            ("/x/b.mp4", None),
        ]);
        let plan = build(&p, &infos, &EncoderSet::software_only()).unwrap();
        // b is longer (whole clip) but... a=7 frames, b=30 frames → target is 720p
        assert_eq!(plan.signatures[0].0.width, 1280);
        assert!(!plan.clips[0].copyable);
        assert!(plan.clips[1].copyable);
        let s0 = &plan.outputs[0].segments[0];
        assert!(matches!(
            &s0.pieces[0].kind,
            PieceKind::Encode {
                filter: Some(_),
                ..
            }
        ));
        assert_eq!(plan.outputs[0].path, PathBuf::from("/x/a - joined.mp4"));
    }

    #[test]
    fn full_reencode_one_setting_per_output() {
        use crate::model::Resolution;
        let pk = packets();
        let mut b = info("/x/b.mp4", 1280, 720, &pk, 0);
        b.encoder = None;
        if let Some(v) = b.video.as_mut() {
            v.codec_name = "hevc".into();
            v.pix_fmt = "yuv420p10le".into();
        }
        let infos = vec![info("/x/a.mp4", 1920, 1080, &pk, 0), b];
        let mut p = project(vec![
            ("/x/a.mp4", None),
            ("/x/b.mp4", Some(vec![(0.0, 1.0)])),
        ]);
        p.export.encoding = EncodingMode::Full;
        p.export.full.codec = VideoCodec::Hevc;
        p.export.full.resolution = Resolution::ShortSide(720);
        let plan = build(&p, &infos, &EncoderSet::software_only()).unwrap();
        let encs: Vec<&VideoEncode> = plan.outputs[0]
            .segments
            .iter()
            .flat_map(|s| &s.pieces)
            .map(|p| match &p.kind {
                PieceKind::Encode { enc, .. } => enc.as_ref(),
                _ => panic!("Full mode copies nothing"),
            })
            .collect();
        // the target (a, the longer kept part) is 8-bit: every piece is 8-bit 1280×720
        assert!(encs.iter().all(|e| **e == *encs[0]));
        assert_eq!((encs[0].width, encs[0].height), (1280, 720));
        assert_eq!(encs[0].pix_fmt, "yuv420p");
        assert_eq!(encs[0].encoder, "libx265");
        assert!(
            plan.clips[0]
                .summary
                .contains("1920×1080 scaled to 1280×720")
        );
        assert!(plan.outputs[0].est_bytes > 0 && plan.outputs[0].est_temp_bytes > 0);

        // separate files: each output follows its own source (b stays 10-bit)
        p.export.output_mode = OutputMode::PerClip;
        let plan = build(&p, &infos, &EncoderSet::software_only()).unwrap();
        let first_enc = |o: &OutputPlan| match &o.segments[0].pieces[0].kind {
            PieceKind::Encode { enc, .. } => enc.as_ref().clone(),
            _ => unreachable!(),
        };
        assert_eq!(first_enc(&plan.outputs[1]).pix_fmt, "yuv420p10le");
        assert_eq!(first_enc(&plan.outputs[0]).pix_fmt, "yuv420p");
    }

    #[test]
    fn auto_encoder_choice() {
        let set = EncoderSet {
            working: vec![
                "h264_nvenc".into(),
                "hevc_qsv".into(),
                "libx264".into(),
                "libx265".into(),
            ],
            failed: vec![],
        };
        // screen recordings below 4K: software; camera or 4K: hardware
        assert_eq!(
            pick_encoder(VideoCodec::H264, None, true, 1080, &set),
            "libx264"
        );
        assert_eq!(
            pick_encoder(VideoCodec::H264, None, true, 2160, &set),
            "h264_nvenc"
        );
        assert_eq!(
            pick_encoder(VideoCodec::Hevc, None, false, 1080, &set),
            "hevc_qsv"
        );
        // a choice for the other codec means the same family, if that one works
        assert_eq!(
            pick_encoder(VideoCodec::Hevc, Some("h264_qsv"), true, 1080, &set),
            "hevc_qsv"
        );
        assert_eq!(
            pick_encoder(VideoCodec::Hevc, Some("libx264"), false, 1080, &set),
            "libx265"
        );
        // ... otherwise Auto (hevc_nvenc is not in the set)
        assert_eq!(
            pick_encoder(VideoCodec::Hevc, Some("h264_nvenc"), false, 1080, &set),
            "hevc_qsv"
        );
        assert_eq!(
            pick_encoder(VideoCodec::H264, Some("libx264"), false, 2160, &set),
            "libx264"
        );
    }

    #[test]
    fn concat_escaping() {
        assert_eq!(
            concat_escape("C:\\a b\\it's.ts"),
            "file:C:\\a b\\it'\\''s.ts"
        );
        let l = concat_list(&[(PathBuf::from("/t/p.ts"), 1.5)]);
        assert!(l.contains("file 'file:/t/p.ts'\nduration 1.500000\n"));
    }

    #[test]
    fn loudness_filter_merges_equal_gains() {
        let f = loudness_filter(&[(0.0, 0.0)]);
        assert!(f.contains("volume=volume='1.000000':eval=frame"), "{f}");
        assert!(f.ends_with("alimiter=limit=0.891:level=disabled:latency=1"));
        // three segments, the first two from one clip (same gain)
        let f = loudness_filter(&[(0.0, 6.0), (10.0, 6.0), (15.0, -6.0)]);
        let expr = f.split('\'').nth(1).unwrap();
        assert_eq!(expr, "1.995262*lt(t,14.999333)+0.501187*gte(t,14.999333)");
    }
}
