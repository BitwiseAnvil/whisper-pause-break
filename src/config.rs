use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub model: PathBuf,
    pub language: String,
    pub backend: Backend,
    pub hotkey: Hotkey,
    pub threads: usize,
    pub max_recording_seconds: u32,
    pub min_recording_ms: u32,
    pub silence_rms: f32,
    pub cue_volume: f32,
    pub inference_timeout_seconds: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Auto,
    Cpu,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum Hotkey {
    #[default]
    Pause,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    ScrollLock,
}

impl Hotkey {
    pub fn vk(self) -> u32 {
        match self {
            Self::Pause => 0x13,
            Self::ScrollLock => 0x91,
            Self::F13 => 0x7c,
            Self::F14 => 0x7d,
            Self::F15 => 0x7e,
            Self::F16 => 0x7f,
            Self::F17 => 0x80,
            Self::F18 => 0x81,
            Self::F19 => 0x82,
            Self::F20 => 0x83,
            Self::F21 => 0x84,
            Self::F22 => 0x85,
            Self::F23 => 0x86,
            Self::F24 => 0x87,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: "models/ggml-base.en.bin".into(),
            language: "en".into(),
            backend: Backend::Auto,
            hotkey: Hotkey::Pause,
            threads: std::thread::available_parallelism().map_or(4, |n| n.get().min(8)),
            max_recording_seconds: 120,
            min_recording_ms: 200,
            silence_rms: 0.003,
            cue_volume: 0.10,
            inference_timeout_seconds: 180,
        }
    }
}

impl Config {
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("config.toml");
        let config: Self = if path.exists() {
            toml::from_str(&fs::read_to_string(&path)?)
                .with_context(|| format!("Invalid configuration: {}", path.display()))?
        } else {
            Self::default()
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!((1..=64).contains(&self.threads), "threads must be 1..64");
        ensure!(
            (1..=300).contains(&self.max_recording_seconds),
            "max_recording_seconds must be 1..300"
        );
        ensure!(
            (50..=2000).contains(&self.min_recording_ms),
            "min_recording_ms must be 50..2000"
        );
        ensure!(
            self.min_recording_ms < self.max_recording_seconds * 1000,
            "Minimum recording must be shorter than maximum"
        );
        ensure!(
            self.silence_rms.is_finite() && (0.0..=0.2).contains(&self.silence_rms),
            "silence_rms must be 0..0.2"
        );
        ensure!(
            self.cue_volume.is_finite() && (0.0..=0.5).contains(&self.cue_volume),
            "cue_volume must be 0..0.5"
        );
        ensure!(
            (10..=600).contains(&self.inference_timeout_seconds),
            "inference_timeout_seconds must be 10..600"
        );
        ensure!(
            self.language == "auto" || whisper_rs::get_lang_id(&self.language).is_some(),
            "Unknown Whisper language"
        );
        ensure!(
            !self.model.as_os_str().is_empty(),
            "Model path cannot be empty"
        );
        Ok(())
    }

    pub fn save_if_missing(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        let path = dir.join("config.toml");
        if !path.exists() {
            fs::write(path, toml::to_string_pretty(self)?)?;
        }
        Ok(())
    }

    pub fn model_path(&self, dir: &Path) -> PathBuf {
        if self.model.is_absolute() {
            self.model.clone()
        } else {
            dir.join(&self.model)
        }
    }
}

pub fn data_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    let path = match explicit {
        Some(path) => path,
        None => {
            PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unavailable")?)
                .join("WhisperPauseBreak")
        }
    };
    Ok(std::path::absolute(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_invalid_bounds() {
        let mut c = Config {
            max_recording_seconds: 0,
            ..Config::default()
        };
        assert!(c.validate().is_err());
        c.max_recording_seconds = 120;
        c.cue_volume = f32::NAN;
        assert!(c.validate().is_err());
    }
    #[test]
    fn unknown_options_are_errors() {
        assert!(toml::from_str::<Config>("backed = 'cpu'").is_err());
    }
}
