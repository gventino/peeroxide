use std::path::Path;

use p2pss_codec::Preset;
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
        };
        s.save(dir.path()).unwrap();
        let loaded = Settings::load(dir.path());
        assert_eq!(loaded, s);
        assert_eq!(loaded.preset(), Preset::P720);

        std::fs::write(dir.path().join(FILE), "not = [valid").unwrap();
        assert_eq!(Settings::load(dir.path()), Settings::default());
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
