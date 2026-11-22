//! Project data model (serialized as the `.keepcut` file and the CLI input).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PROJECT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    /// Order = output order.
    pub clips: Vec<Clip>,
    #[serde(default)]
    pub export: ExportSettings,
}

fn default_version() -> u32 {
    PROJECT_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    pub path: PathBuf,
    #[serde(default)]
    pub file_size: u64,
    #[serde(default)]
    pub file_mtime: i64,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Sorted by start, non-overlapping. `None` in input JSON = whole clip.
    #[serde(default)]
    pub segments: Option<Vec<Segment>>,
    #[serde(default)]
    pub chapter_title: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    /// Seconds relative to the file start; snapped to a frame timestamp.
    pub start: f64,
    /// Exclusive: the timestamp of the first excluded frame, or the clip end.
    pub end: f64,
    #[serde(default)]
    pub chapter_title: Option<String>,
}

impl Segment {
    pub fn new(start: f64, end: f64) -> Self {
        Segment {
            id: Uuid::new_v4(),
            start,
            end,
            chapter_title: None,
        }
    }
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    #[default]
    Joined,
    PerClip,
    PerSegment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncodingMode {
    #[default]
    Smart,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioTracks {
    #[default]
    MixAll,
    FirstOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Loudness {
    #[default]
    Off,
    Whole,
    PerClip,
    PerSegment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Chapters {
    #[default]
    None,
    PerClip,
    PerSegment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    #[default]
    H264,
    Hevc,
    Av1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    /// Decide from the source (encoder tag / VFR frame skipping).
    #[default]
    Auto,
    Screen,
    Camera,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum Quality {
    /// The calibrated "match source" presets.
    #[default]
    MatchSource,
    /// CRF (software) / CQ / QP (hardware).
    Crf(u32),
    BitrateKbps(u32),
}

/// Output resolution of a Full re-encode (§6.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum Resolution {
    /// Same as the target signature (joined) or the source (separate files).
    #[default]
    Same,
    /// A preset such as 1080 for "1080p": the *short* side, so portrait sources
    /// stay portrait; the long side follows the source's aspect ratio.
    ShortSide(u32),
    /// Exact width × height (the picture is scaled to fit and padded).
    Custom(u32, u32),
}

impl Resolution {
    /// Output frame size for a source/target of `w`×`h` (even dimensions).
    pub fn apply(self, w: u32, h: u32) -> (u32, u32) {
        let even = |x: f64| ((x / 2.0).round() as u32).max(1) * 2;
        match self {
            Resolution::Same => (w, h),
            Resolution::Custom(cw, ch) => (even(cw as f64), even(ch as f64)),
            Resolution::ShortSide(s) => {
                let s = s as f64;
                if w >= h {
                    (even(w as f64 * s / h.max(1) as f64), even(s))
                } else {
                    (even(s), even(h as f64 * s / w.max(1) as f64))
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FullEncodeSettings {
    pub codec: VideoCodec,
    /// `None` = Auto; otherwise an ffmpeg encoder name such as `h264_nvenc`.
    pub encoder: Option<String>,
    pub quality: Quality,
    pub resolution: Resolution,
    /// `None` = keep original timing (VFR passthrough).
    pub fps: Option<u32>,
    pub content: ContentType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportSettings {
    pub output_mode: OutputMode,
    pub encoding: EncodingMode,
    /// `None` = same folder as the source.
    pub output_dir: Option<PathBuf>,
    /// Joined mode file name (without folder); default `<first stem> - joined.mp4`.
    pub output_name: Option<String>,
    /// The format chosen as the joined target instead of the automatic one
    /// (ignored when no enabled clip has it any more).
    pub target_override: Option<crate::compat::Signature>,
    /// Encoder for whole re-encoded (mismatched) clips in Smart mode. `None` = Auto.
    pub smart_encoder: Option<String>,
    /// Quality of whole re-encoded clips in Smart mode (heads/tails always
    /// use the calibrated presets).
    pub smart_quality: Quality,
    pub full: FullEncodeSettings,
    pub audio_tracks: AudioTracks,
    pub audio_bitrate_kbps: u32,
    pub loudness: Loudness,
    pub loudness_target: f64,
    pub chapters: Chapters,
}

impl Default for ExportSettings {
    fn default() -> Self {
        ExportSettings {
            output_mode: OutputMode::Joined,
            encoding: EncodingMode::Smart,
            output_dir: None,
            output_name: None,
            target_override: None,
            smart_encoder: None,
            smart_quality: Quality::MatchSource,
            full: FullEncodeSettings::default(),
            audio_tracks: AudioTracks::MixAll,
            audio_bitrate_kbps: 192,
            loudness: Loudness::Off,
            loudness_target: -14.0,
            chapters: Chapters::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_presets_follow_the_short_side() {
        let r = Resolution::ShortSide(1080);
        assert_eq!(r.apply(3840, 2160), (1920, 1080));
        assert_eq!(r.apply(2160, 3840), (1080, 1920), "portrait stays portrait");
        assert_eq!(Resolution::ShortSide(720).apply(1366, 768), (1280, 720));
        assert_eq!(Resolution::ShortSide(1080).apply(2560, 1080), (2560, 1080));
        assert_eq!(Resolution::Same.apply(1919, 1079), (1919, 1079));
        assert_eq!(Resolution::Custom(1279, 721).apply(10, 10), (1280, 722));
    }
}
