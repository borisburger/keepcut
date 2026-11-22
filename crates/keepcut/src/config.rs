//! User settings: `<config dir>/config.toml` (§9). Missing or broken files fall
//! back to defaults; saving is best effort.

use std::path::PathBuf;

use keepcut_core::model::{
    AudioTracks, Chapters, EncodingMode, ExportSettings, FullEncodeSettings, Loudness, OutputMode,
    Quality,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputLocation {
    #[default]
    SameAsSource,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderPath {
    /// OpenGL render path, falling back to software automatically.
    #[default]
    Auto,
    Software,
}

/// Video decoding in the preview (mpv's `hwdec`); exports are not affected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HwDecoding {
    /// The GPU's decoder where mpv considers it safe, else the CPU.
    #[default]
    Auto,
    /// Always the CPU (a fallback for display problems such as black lines or blocks).
    Off,
}

impl HwDecoding {
    /// The `hwdec` value to give mpv instead of its default (`None`: the default).
    pub fn mpv_override(self) -> Option<&'static str> {
        match self {
            HwDecoding::Auto => None,
            HwDecoding::Off => Some("no"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub output_location: OutputLocation,
    /// The last custom output folder (remembered, F7).
    pub custom_output_dir: Option<PathBuf>,
    pub encoding: EncodingMode,
    pub output_mode: OutputMode,
    /// Encoder for whole re-encodes in Smart mode (`None` = Auto).
    pub smart_encoder: Option<String>,
    pub smart_quality: Quality,
    /// The last Full re-encode settings.
    pub full: FullEncodeSettings,
    pub audio_tracks: AudioTracks,
    pub audio_bitrate_kbps: u32,
    pub loudness: Loudness,
    /// Default loudness target (LUFS), also set in Settings.
    pub loudness_target: f64,
    pub chapters: Chapters,
    pub render: RenderPath,
    pub hw_decoding: HwDecoding,
    /// Folder with ffmpeg and ffprobe (`None`: bundled next to Keepcut, then PATH).
    pub ffmpeg_dir: Option<PathBuf>,
    /// Parent of the export temp folders (`None`: the system temp folder).
    pub temp_dir: Option<PathBuf>,
    /// Decode every output completely after an export (slow; §6.11).
    pub full_decode_check: bool,
    pub theme: Theme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub fn preference(self) -> egui::ThemePreference {
        match self {
            Theme::System => egui::ThemePreference::System,
            Theme::Light => egui::ThemePreference::Light,
            Theme::Dark => egui::ThemePreference::Dark,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            output_location: OutputLocation::SameAsSource,
            custom_output_dir: None,
            encoding: EncodingMode::Smart,
            output_mode: OutputMode::Joined,
            smart_encoder: None,
            smart_quality: Quality::MatchSource,
            full: FullEncodeSettings::default(),
            audio_tracks: AudioTracks::MixAll,
            audio_bitrate_kbps: 192,
            loudness: Loudness::Off,
            loudness_target: -14.0,
            chapters: Chapters::None,
            render: RenderPath::Auto,
            hw_decoding: HwDecoding::Auto,
            ffmpeg_dir: None,
            temp_dir: None,
            full_decode_check: false,
            theme: Theme::System,
        }
    }
}

pub fn dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "Keepcut")
}

fn path() -> Option<PathBuf> {
    dirs().map(|d| d.config_dir().join("config.toml"))
}

impl Config {
    pub fn load() -> Config {
        let Some(p) = path() else {
            return Config::default();
        };
        match std::fs::read_to_string(&p) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("ignoring broken {}: {e}", p.display());
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        let result = (|| -> anyhow::Result<()> {
            std::fs::create_dir_all(p.parent().unwrap_or(&p))?;
            let tmp = p.with_extension("toml.tmp");
            std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
            std::fs::rename(&tmp, &p)?;
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!("cannot save config: {e}");
        }
    }
}

impl Config {
    /// The remembered export choices → a project's export settings.
    pub fn apply_export_defaults(&self, x: &mut ExportSettings) {
        x.encoding = self.encoding;
        x.output_mode = self.output_mode;
        x.smart_encoder = self.smart_encoder.clone();
        x.smart_quality = self.smart_quality.clone();
        x.full = self.full.clone();
        x.audio_tracks = self.audio_tracks;
        x.audio_bitrate_kbps = self.audio_bitrate_kbps;
        x.loudness = self.loudness;
        x.loudness_target = self.loudness_target;
        x.chapters = self.chapters;
    }

    /// Remembers the export choices of `x` (saved with the config).
    pub fn remember_export(&mut self, x: &ExportSettings) {
        self.encoding = x.encoding;
        self.output_mode = x.output_mode;
        self.smart_encoder = x.smart_encoder.clone();
        self.smart_quality = x.smart_quality.clone();
        self.full = x.full.clone();
        self.audio_tracks = x.audio_tracks;
        self.audio_bitrate_kbps = x.audio_bitrate_kbps;
        self.loudness = x.loudness;
        self.loudness_target = x.loudness_target;
        self.chapters = x.chapters;
    }
}
