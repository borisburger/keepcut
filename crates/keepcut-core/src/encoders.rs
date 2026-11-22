//! Encoder detection (test encodes) and quality presets (§6.8).

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::ffmpeg::{self, CancelToken, LogSink, RunOpts, Tools};
use crate::model::{Quality, VideoCodec};

pub const CANDIDATES: &[&str] = &[
    "h264_nvenc",
    "hevc_nvenc",
    "h264_amf",
    "hevc_amf",
    "h264_qsv",
    "hevc_qsv",
    "h264_vaapi",
    "hevc_vaapi",
    "h264_videotoolbox",
    "hevc_videotoolbox",
    "av1_nvenc",
    "av1_amf",
    "av1_qsv",
    "av1_vaapi",
    "libx264",
    "libx265",
    "libaom-av1",
    "libsvtav1",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EncoderSet {
    /// Encoders that passed the test encode, in [`CANDIDATES`] order.
    pub working: Vec<String>,
    /// Encoders that failed, with the reason (last stderr line).
    pub failed: Vec<(String, String)>,
}

impl EncoderSet {
    pub fn has(&self, name: &str) -> bool {
        self.working.iter().any(|w| w == name)
    }
    /// First working hardware encoder for `codec`.
    pub fn first_hardware(&self, codec: VideoCodec) -> Option<&str> {
        let prefix = codec_prefix(codec);
        self.working
            .iter()
            .map(String::as_str)
            .find(|e| e.starts_with(prefix) && !is_software(e))
    }
    /// The software encoder for `codec`. AV1: libaom (calibrated), or SVT-AV1 where the
    /// ffmpeg build has no libaom (Homebrew's).
    pub fn software(&self, codec: VideoCodec) -> &'static str {
        match codec {
            VideoCodec::Av1 if !self.has("libaom-av1") && self.has("libsvtav1") => "libsvtav1",
            c => software_encoder(c),
        }
    }
    /// Only software encoders (for tests and when detection is skipped).
    pub fn software_only() -> EncoderSet {
        EncoderSet {
            working: vec!["libx264".into(), "libx265".into(), "libaom-av1".into()],
            failed: Vec::new(),
        }
    }
}

pub fn codec_prefix(c: VideoCodec) -> &'static str {
    match c {
        VideoCodec::H264 => "h264_",
        VideoCodec::Hevc => "hevc_",
        VideoCodec::Av1 => "av1_",
    }
}

pub fn software_encoder(c: VideoCodec) -> &'static str {
    match c {
        VideoCodec::H264 => "libx264",
        VideoCodec::Hevc => "libx265",
        VideoCodec::Av1 => "libaom-av1",
    }
}

pub fn is_software(enc: &str) -> bool {
    enc.starts_with("lib")
}

/// Output codec of an ffmpeg encoder name (`hevc_nvenc` → HEVC).
pub fn codec_of_encoder(enc: &str) -> Option<VideoCodec> {
    match enc {
        "libx264" => Some(VideoCodec::H264),
        "libx265" => Some(VideoCodec::Hevc),
        "libaom-av1" | "libsvtav1" => Some(VideoCodec::Av1),
        e if e.starts_with("h264_") => Some(VideoCodec::H264),
        e if e.starts_with("hevc_") => Some(VideoCodec::Hevc),
        e if e.starts_with("av1_") => Some(VideoCodec::Av1),
        _ => None,
    }
}

/// The same encoder family for another codec: `h264_nvenc` → `hevc_nvenc`,
/// `libx264` → `libx265`. `None` if the family has no encoder for `codec`.
pub fn counterpart(enc: &str, codec: VideoCodec) -> Option<String> {
    if codec_of_encoder(enc) == Some(codec) {
        return Some(enc.to_string());
    }
    if is_software(enc) {
        return Some(software_encoder(codec).to_string());
    }
    let (_, family) = enc.split_once('_')?;
    let cand = format!("{}{family}", codec_prefix(codec));
    CANDIDATES.contains(&cand.as_str()).then_some(cand)
}

/// Human name of an encoder for menus: "NVIDIA NVENC (h264_nvenc)".
pub fn describe(enc: &str) -> String {
    let vendor = match enc {
        "libx264" | "libx265" | "libaom-av1" | "libsvtav1" => "software",
        e if e.ends_with("_nvenc") => "NVIDIA NVENC",
        e if e.ends_with("_amf") => "AMD AMF",
        e if e.ends_with("_qsv") => "Intel Quick Sync",
        e if e.ends_with("_vaapi") => "VA-API",
        e if e.ends_with("_videotoolbox") => "VideoToolbox",
        _ => "",
    };
    if vendor.is_empty() {
        enc.to_string()
    } else {
        format!("{vendor} ({enc})")
    }
}

pub fn codec_of_name(codec_name: &str) -> Option<VideoCodec> {
    match codec_name {
        "h264" => Some(VideoCodec::H264),
        "hevc" => Some(VideoCodec::Hevc),
        "av1" => Some(VideoCodec::Av1),
        _ => None,
    }
}

fn test_cmd(tools: &Tools, enc: &str, w: u32, h: u32, pix_fmt: &str) -> ffmpeg::Cmd {
    let mut c = tools
        .ffmpeg()
        .args(["-hide_banner", "-nostdin", "-v", "error"]);
    if enc.ends_with("_vaapi") {
        c = c.args(["-vaapi_device", "/dev/dri/renderD128"]);
    }
    c = c
        .args(["-f", "lavfi", "-i"])
        .arg(format!("color=black:s={w}x{h}:r=15:d=0.2,format={pix_fmt}"));
    if enc.ends_with("_vaapi") {
        c = c.args(["-vf", "format=nv12,hwupload"]);
    }
    c.args(["-c:v", enc, "-f", "null", "-"])
}

/// Test-encodes every candidate; usable = exit code 0.
pub fn detect(
    tools: &Tools,
    cancel: Option<&CancelToken>,
    log: Option<&LogSink>,
) -> Result<EncoderSet> {
    detect_among(tools, CANDIDATES.iter().copied(), cancel, log)
}

/// Test-encodes only the software candidates (tests: fast, and true to the local
/// ffmpeg build, which may lack libaom or have SVT-AV1).
pub fn detect_software(tools: &Tools) -> Result<EncoderSet> {
    detect_among(
        tools,
        CANDIDATES.iter().copied().filter(|e| is_software(e)),
        None,
        None,
    )
}

fn detect_among<'a>(
    tools: &Tools,
    candidates: impl Iterator<Item = &'a str>,
    cancel: Option<&CancelToken>,
    log: Option<&LogSink>,
) -> Result<EncoderSet> {
    let mut set = EncoderSet::default();
    for enc in candidates {
        let out = ffmpeg::run(
            &test_cmd(tools, enc, 320, 240, "yuv420p"),
            RunOpts {
                cancel,
                log,
                ..Default::default()
            },
        )?;
        if out.success() {
            set.working.push(enc.to_string());
        } else {
            set.failed
                .push((enc.to_string(), failure_reason(&out.stderr)));
        }
    }
    Ok(set)
}

/// Re-tests an encode with its real parameters (some hardware encoders reject
/// 4K, 10-bit or certain profiles; §6.8): a 1-frame encode of a black frame of
/// the output size with exactly the arguments the pieces will use.
/// Returns `None` when it works, else the reason.
pub fn test_encode(
    tools: &Tools,
    enc: &VideoEncode,
    cancel: Option<&CancelToken>,
    log: Option<&LogSink>,
) -> Result<Option<String>> {
    let (w, h) = (enc.width.max(64), enc.height.max(64));
    let mut c = tools
        .ffmpeg()
        .args(["-hide_banner", "-nostdin", "-v", "error"]);
    let vaapi = enc.encoder.ends_with("_vaapi");
    if vaapi {
        c = c.args(["-vaapi_device", "/dev/dri/renderD128"]);
    }
    c = c
        .args(["-f", "lavfi", "-i"])
        .arg(format!("color=black:s={w}x{h}:r=15:d=0.2"));
    if vaapi {
        c = c.args(["-vf", "format=nv12,hwupload"]);
    }
    let out = ffmpeg::run(
        &c.args(enc.args())
            .args(["-frames:v", "1", "-f", "null", "-"]),
        RunOpts {
            cancel,
            log,
            ..Default::default()
        },
    )?;
    Ok((!out.success()).then(|| failure_reason(&out.stderr)))
}

/// The useful part of a failed test encode: ffmpeg's *first* message is the
/// cause ("DLL amfrt64.dll failed to open"); the last one is a generic
/// "Nothing was written…". The `[name @ 0x…] ` prefixes are removed.
pub fn failure_reason(stderr: &str) -> String {
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("failed");
    let mut rest = line;
    while let Some(r) = rest
        .strip_prefix('[')
        .and_then(|r| r.split_once("] "))
        .map(|(_, r)| r)
    {
        rest = r.trim_start();
    }
    rest.to_string()
}

/// What a re-encoded piece is used for; selects the preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// Head/tail of a smart cut (short, ≤ 1 GOP).
    Head,
    /// Whole-segment re-encode.
    Long,
}

/// Full description of how to encode a video piece.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoEncode {
    pub encoder: String,
    pub role: Role,
    pub quality: Quality,
    pub pix_fmt: String,
    /// ffmpeg profile name (`high`, `main10`, …).
    pub profile: Option<String>,
    /// Level as ffmpeg expects it (`4.0`, `5.1`).
    pub level: Option<String>,
    pub color_primaries: Option<String>,
    pub color_trc: Option<String>,
    pub colorspace: Option<String>,
    pub color_range: Option<String>,
    pub bframes: u32,
    /// Keyframe interval in frames.
    pub gop: Option<u32>,
    pub tune: Option<String>,
    /// Output frame size (the height selects faster presets at ≥ 2160p).
    #[serde(default)]
    pub width: u32,
    pub height: u32,
}

/// ffprobe profile name → ffmpeg encoder profile name.
pub fn ffmpeg_profile(probe_profile: &str) -> Option<String> {
    let p = probe_profile.to_ascii_lowercase();
    let p = match p.as_str() {
        "constrained baseline" | "baseline" => "baseline".to_string(),
        "high 4:2:2" | "high 4:2:2 intra" => "high422".to_string(),
        "high 4:4:4 predictive" => "high444".to_string(),
        "rext" => return None,
        other => other.replace(' ', ""),
    };
    Some(p)
}

/// ffprobe level number → ffmpeg level string.
pub fn ffmpeg_level(codec: VideoCodec, level: i64) -> Option<String> {
    if level <= 0 {
        return None;
    }
    Some(match codec {
        VideoCodec::H264 => format!("{:.1}", level as f64 / 10.0),
        VideoCodec::Hevc => format!("{:.1}", level as f64 / 30.0),
        VideoCodec::Av1 => return None,
    })
}

impl VideoEncode {
    /// "H.264", "HEVC 10-bit", …
    pub fn codec_label(&self) -> String {
        let c = match codec_of_encoder(&self.encoder) {
            Some(VideoCodec::H264) => "H.264",
            Some(VideoCodec::Hevc) => "HEVC",
            Some(VideoCodec::Av1) => "AV1",
            None => "video",
        };
        if self.pix_fmt.contains("10") {
            format!("{c} 10-bit")
        } else {
            c.to_string()
        }
    }

    /// The same encode with the codec's software encoder (fallback when a
    /// hardware encoder fails; §6.8).
    pub fn to_software(&self) -> VideoEncode {
        let codec = codec_of_encoder(&self.encoder).unwrap_or(VideoCodec::H264);
        VideoEncode {
            encoder: software_encoder(codec).to_string(),
            ..self.clone()
        }
    }

    /// Encoder arguments (`-c:v … -pix_fmt … …`), without input/filter/output.
    pub fn args(&self) -> Vec<String> {
        let e = self.encoder.as_str();
        let mut a: Vec<String> = vec!["-c:v".into(), e.into()];
        let push = |a: &mut Vec<String>, k: &str, v: &str| {
            a.push(k.into());
            a.push(v.into());
        };
        let big = self.height >= 2160;
        let hevc = e.starts_with("hevc") || e == "libx265";
        let av1 = e.starts_with("av1") || e == "libaom-av1" || e == "libsvtav1";
        let mut x265_params: Vec<String> = Vec::new();

        match (e, &self.quality) {
            ("libx264", q) => {
                let preset = match (self.role, big) {
                    (Role::Head, false) => "slow",
                    _ => "medium",
                };
                push(&mut a, "-preset", preset);
                match q {
                    Quality::MatchSource => push(&mut a, "-crf", "18"),
                    Quality::Crf(c) => push(&mut a, "-crf", &c.to_string()),
                    Quality::BitrateKbps(b) => push(&mut a, "-b:v", &format!("{b}k")),
                }
                if let Some(t) = &self.tune {
                    push(&mut a, "-tune", t);
                }
            }
            ("libaom-av1", q) => {
                // calibrated on Bandicam AV1 4K: SSIM 0.9993 at 0.42x the source size, ~9 fps
                for (k, v) in [
                    ("-usage", "good"),
                    ("-cpu-used", "6"),
                    ("-row-mt", "1"),
                    ("-tiles", "2x2"),
                ] {
                    push(&mut a, k, v);
                }
                match q {
                    Quality::MatchSource => push(&mut a, "-crf", "28"),
                    Quality::Crf(c) => push(&mut a, "-crf", &c.to_string()),
                    Quality::BitrateKbps(b) => push(&mut a, "-b:v", &format!("{b}k")),
                }
                if !matches!(q, Quality::BitrateKbps(_)) {
                    push(&mut a, "-b:v", "0");
                }
            }
            ("libsvtav1", q) => {
                // not calibrated like libaom (no SVT-AV1 build at hand): presets 6–8 are
                // SVT's middle ground, crf a little below libaom's to err on quality
                let preset = match (self.role, big) {
                    (Role::Head, false) => "6",
                    (_, false) => "8",
                    (_, true) => "10",
                };
                push(&mut a, "-preset", preset);
                match q {
                    Quality::MatchSource => push(&mut a, "-crf", "26"),
                    Quality::Crf(c) => push(&mut a, "-crf", &c.to_string()),
                    Quality::BitrateKbps(b) => push(&mut a, "-b:v", &format!("{b}k")),
                }
                // keyframes start closed GOPs, as the copied parts expect
                push(&mut a, "-svtav1-params", "irefresh-type=2");
            }
            ("libx265", q) => {
                push(&mut a, "-preset", if big { "fast" } else { "medium" });
                match q {
                    Quality::MatchSource => push(&mut a, "-crf", "20"),
                    Quality::Crf(c) => push(&mut a, "-crf", &c.to_string()),
                    Quality::BitrateKbps(b) => push(&mut a, "-b:v", &format!("{b}k")),
                }
                x265_params.push("log-level=error".into());
            }
            (_, q) if e.ends_with("_nvenc") => {
                for (k, v) in [("-preset", "p5"), ("-tune", "hq"), ("-spatial-aq", "1")] {
                    push(&mut a, k, v);
                }
                match q {
                    Quality::BitrateKbps(b) => {
                        push(&mut a, "-rc", "vbr");
                        push(&mut a, "-b:v", &format!("{b}k"));
                    }
                    q => {
                        let cq = match q {
                            Quality::Crf(c) => *c,
                            // av1_nvenc cq 36: SSIM 0.998 at 1.17x the source size (Bandicam AV1 4K)
                            _ if av1 => 36,
                            _ if hevc => 21,
                            _ => 19,
                        };
                        for (k, v) in [
                            ("-rc", "vbr".to_string()),
                            ("-cq", cq.to_string()),
                            ("-b:v", "0".into()),
                        ] {
                            push(&mut a, k, &v);
                        }
                    }
                }
            }
            (_, q) if e.ends_with("_amf") => {
                push(&mut a, "-quality", "quality");
                match q {
                    Quality::BitrateKbps(b) => {
                        push(&mut a, "-rc", "vbr_peak");
                        push(&mut a, "-b:v", &format!("{b}k"));
                    }
                    q => {
                        let qp = if let Quality::Crf(c) = q { *c } else { 18 };
                        push(&mut a, "-rc", "cqp");
                        push(&mut a, "-qp_i", &qp.to_string());
                        push(&mut a, "-qp_p", &(qp + 2).to_string());
                    }
                }
            }
            (_, q) if e.ends_with("_qsv") => {
                push(&mut a, "-preset", "slower");
                match q {
                    Quality::BitrateKbps(b) => push(&mut a, "-b:v", &format!("{b}k")),
                    q => {
                        let gq = if let Quality::Crf(c) = q { *c } else { 20 };
                        push(&mut a, "-global_quality", &gq.to_string());
                    }
                }
            }
            (_, q) if e.ends_with("_vaapi") => match q {
                Quality::BitrateKbps(b) => push(&mut a, "-b:v", &format!("{b}k")),
                q => {
                    let qp = if let Quality::Crf(c) = q { *c } else { 20 };
                    push(&mut a, "-rc_mode", "CQP");
                    push(&mut a, "-qp", &qp.to_string());
                }
            },
            (_, q) => {
                // videotoolbox and others: quality-based where possible
                if let Quality::BitrateKbps(b) = q {
                    push(&mut a, "-b:v", &format!("{b}k"));
                } else {
                    push(&mut a, "-q:v", "65");
                }
            }
        }

        if !e.ends_with("_vaapi") {
            push(&mut a, "-pix_fmt", &self.pix_fmt);
        }
        // AV1 encoders pick the profile from the pixel format
        if let Some(p) = self.profile.as_ref().filter(|_| !av1) {
            push(&mut a, "-profile:v", p);
        }
        if let Some(l) = &self.level {
            if e == "libx265" {
                x265_params.push(format!("level-idc={l}"));
            } else if e == "libx264" {
                push(&mut a, "-level", l);
            }
            // hardware encoders pick the level themselves
        }
        for (k, v) in [
            ("-color_primaries", &self.color_primaries),
            ("-color_trc", &self.color_trc),
            ("-colorspace", &self.colorspace),
            ("-color_range", &self.color_range),
        ] {
            if let Some(v) = v {
                push(&mut a, k, v);
            }
        }
        push(&mut a, "-bf", &self.bframes.to_string());
        if let Some(g) = self.gop {
            push(&mut a, "-g", &g.to_string());
            if e == "libx265" {
                x265_params.push("no-open-gop=1".into());
            }
        }
        if !x265_params.is_empty() {
            push(&mut a, "-x265-params", &x265_params.join(":"));
        }
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_level_mapping() {
        assert_eq!(ffmpeg_profile("High").as_deref(), Some("high"));
        assert_eq!(ffmpeg_profile("Main 10").as_deref(), Some("main10"));
        assert_eq!(
            ffmpeg_profile("Constrained Baseline").as_deref(),
            Some("baseline")
        );
        assert_eq!(ffmpeg_level(VideoCodec::H264, 40).as_deref(), Some("4.0"));
        assert_eq!(ffmpeg_level(VideoCodec::Hevc, 150).as_deref(), Some("5.0"));
        assert_eq!(ffmpeg_level(VideoCodec::Hevc, 153).as_deref(), Some("5.1"));
    }

    #[test]
    fn counterparts() {
        assert_eq!(
            counterpart("hevc_nvenc", VideoCodec::H264).as_deref(),
            Some("h264_nvenc")
        );
        assert_eq!(
            counterpart("h264_qsv", VideoCodec::Hevc).as_deref(),
            Some("hevc_qsv")
        );
        assert_eq!(
            counterpart("libx265", VideoCodec::H264).as_deref(),
            Some("libx264")
        );
        assert_eq!(
            counterpart("h264_nvenc", VideoCodec::H264).as_deref(),
            Some("h264_nvenc")
        );
        assert_eq!(counterpart("h264_videotoolbox", VideoCodec::Av1), None);
        assert_eq!(
            failure_reason("[AMF @ 0000] DLL amfrt64.dll failed to open\n[h264_amf @ 01] x\n"),
            "DLL amfrt64.dll failed to open"
        );
    }

    #[test]
    fn software_av1_falls_back_to_svt() {
        let set = |names: &[&str]| EncoderSet {
            working: names.iter().map(|s| s.to_string()).collect(),
            failed: Vec::new(),
        };
        let both = set(&["libx264", "libaom-av1", "libsvtav1"]);
        assert_eq!(both.software(VideoCodec::Av1), "libaom-av1");
        let svt = set(&["libx264", "libsvtav1"]);
        assert_eq!(svt.software(VideoCodec::Av1), "libsvtav1");
        assert_eq!(svt.software(VideoCodec::H264), "libx264");
        assert_eq!(codec_of_encoder("libsvtav1"), Some(VideoCodec::Av1));
        let v = VideoEncode {
            encoder: "libsvtav1".into(),
            role: Role::Head,
            quality: Quality::MatchSource,
            pix_fmt: "yuv420p".into(),
            profile: Some("main".into()),
            level: None,
            color_primaries: None,
            color_trc: None,
            colorspace: None,
            color_range: None,
            bframes: 0,
            gop: None,
            tune: None,
            width: 1920,
            height: 1080,
        };
        assert_eq!(
            v.args().join(" "),
            "-c:v libsvtav1 -preset 6 -crf 26 -svtav1-params irefresh-type=2 -pix_fmt yuv420p -bf 0"
        );
    }

    #[test]
    fn x264_head_args() {
        let v = VideoEncode {
            encoder: "libx264".into(),
            role: Role::Head,
            quality: Quality::MatchSource,
            pix_fmt: "yuv420p".into(),
            profile: Some("high".into()),
            level: Some("4.0".into()),
            color_primaries: None,
            color_trc: None,
            colorspace: None,
            color_range: None,
            bframes: 0,
            gop: None,
            tune: None,
            width: 1920,
            height: 1080,
        };
        assert_eq!(
            v.args().join(" "),
            "-c:v libx264 -preset slow -crf 18 -pix_fmt yuv420p -profile:v high -level 4.0 -bf 0"
        );
    }
}
