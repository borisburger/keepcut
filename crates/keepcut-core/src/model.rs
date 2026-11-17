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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FullEncodeSettings {
    pub codec: VideoCodec,
    /// `None` = Auto; otherwise an ffmpeg encoder name such as `h264_nvenc`.
    pub encoder: Option<String>,
    pub quality: Quality,
    /// Target short-side height preset (e.g. 1080) or `None` = same as target.
    pub resolution: Option<(u32, u32)>,
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
    /// Index into the signature list to override the automatic target.
    pub target_override: Option<usize>,
    /// Encoder for whole re-encoded (mismatched) clips in Smart mode. `None` = Auto.
    pub smart_encoder: Option<String>,
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
            full: FullEncodeSettings::default(),
            audio_tracks: AudioTracks::MixAll,
            audio_bitrate_kbps: 192,
            loudness: Loudness::Off,
            loudness_target: -14.0,
            chapters: Chapters::None,
        }
    }
}
