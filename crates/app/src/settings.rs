use std::path::Path;

use peeroxide_codec::Preset;
use serde::{Deserialize, Serialize};

const FILE: &str = "settings.toml";

/// User preferences persisted per profile.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    pub display_name: Option<String>,
    pub preset: Option<String>,
    /// Reused on every broadcast so connect strings and saved contacts stay valid.
    pub broadcast_port: Option<u16>,
    /// Share the source's audio when broadcasting. Off unless the user turns it on.
    pub share_audio: bool,
    /// Playback volume of watched streams, in percent (100 when unset).
    pub volume: Option<u8>,
    pub muted: bool,
}

impl Settings {
    /// Missing or unreadable settings fall back to defaults.
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(FILE))
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(dir.join(FILE), text)
    }

    /// Playback volume as 0.0–1.0.
    pub fn volume(&self) -> f32 {
        f32::from(self.volume.unwrap_or(100).min(100)) / 100.0
    }

    pub fn preset(&self) -> Preset {
        self.preset
            .as_deref()
            .and_then(|name| Preset::ALL.into_iter().find(|p| p.name == name))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_and_tolerates_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(dir.path()), Settings::default());

        let s = Settings {
            display_name: Some("Ana".into()),
            preset: Some(Preset::P720.name.into()),
            broadcast_port: Some(50123),
            share_audio: true,
            volume: Some(40),
            muted: true,
        };
        s.save(dir.path()).unwrap();
        let loaded = Settings::load(dir.path());
        assert_eq!(loaded, s);
        assert_eq!(loaded.preset(), Preset::P720);

        std::fs::write(dir.path().join(FILE), "not = [valid").unwrap();
        assert_eq!(Settings::load(dir.path()), Settings::default());
    }

    #[test]
    fn audio_defaults_to_off_at_full_volume_for_older_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(FILE),
            "display_name = \"Ana\"
",
        )
        .unwrap();
        let s = Settings::load(dir.path());
        assert_eq!(s.display_name.as_deref(), Some("Ana"));
        assert!(!s.share_audio);
        assert!(!s.muted);
        assert_eq!(s.volume(), 1.0);
        let loud = Settings {
            volume: Some(250),
            ..Default::default()
        };
        assert_eq!(loud.volume(), 1.0);
    }

    #[test]
    fn unknown_preset_falls_back_to_default() {
        let s = Settings {
            preset: Some("8K".into()),
            ..Default::default()
        };
        assert_eq!(s.preset(), Preset::default());
    }
}
