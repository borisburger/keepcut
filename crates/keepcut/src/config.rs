//! User settings: `<config dir>/config.toml` (§9). Missing or broken files fall
//! back to defaults; saving is best effort.

use std::path::PathBuf;

use keepcut_core::model::{EncodingMode, OutputMode};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub output_location: OutputLocation,
    /// The last custom output folder (remembered, F7).
    pub custom_output_dir: Option<PathBuf>,
    pub encoding: EncodingMode,
    pub output_mode: OutputMode,
    pub render: RenderPath,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            output_location: OutputLocation::SameAsSource,
            custom_output_dir: None,
            encoding: EncodingMode::Smart,
            output_mode: OutputMode::Joined,
            render: RenderPath::Auto,
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
