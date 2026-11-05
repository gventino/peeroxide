//! The "Mute apps" dropdown: which apps a shared monitor's sound leaves out. It sits in the
//! source picker, above Quality, and under the live line while a monitor's sound is shared.
//! Changes apply within a second and are remembered per app.

use std::collections::BTreeMap;
use std::path::Path;

use eframe::egui::{self, RichText};
use peeroxide_audio::{AudioApp, MutedApps, app_name, is_voice_chat};

use crate::app_list::AppList;
use crate::settings::Settings;

/// One line of the checklist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub key: String,
    pub name: String,
    pub muted: bool,
    pub voice_chat: bool,
    /// Remembered as muted, but not playing sound now.
    pub absent: bool,
}

/// The apps playing sound, then the ones remembered as muted that aren't around now.
pub fn rows(apps: &[AudioApp], muted: &MutedApps, choices: &BTreeMap<String, bool>) -> Vec<Row> {
    let mut rows: Vec<Row> = apps
        .iter()
        .map(|a| Row {
            key: a.key.clone(),
            name: a.name.clone(),
            muted: muted.is_muted(&a.key),
            voice_chat: is_voice_chat(&a.key),
            absent: false,
        })
        .collect();
    for (key, &was_muted) in choices {
        if was_muted && !apps.iter().any(|a| &a.key == key) {
            rows.push(Row {
                key: key.clone(),
                name: app_name(key),
                muted: true,
                voice_chat: is_voice_chat(key),
                absent: true,
            });
        }
    }
    rows
}

/// "except Peeroxide, Discord and TeamSpeak 3 Client": what the live audio line adds.
pub fn except_text(apps: &[AudioApp], muted: &MutedApps) -> String {
    let mut names: Vec<&str> = vec!["Peeroxide"];
    names.extend(
        apps.iter()
            .filter(|a| muted.is_muted(&a.key))
            .map(|a| a.name.as_str()),
    );
    match names.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("except {} and {last}", rest.join(", ")),
        _ => "except Peeroxide".into(),
    }
}

/// Dropdown of apps to leave out of a monitor share. `applies` is false for a window share,
/// which only captures that one app. A change is applied to `muted` and saved.
pub fn ui(
    ui: &mut egui::Ui,
    list: &AppList,
    muted: &MutedApps,
    settings: &mut Settings,
    dir: &Path,
    applies: bool,
) {
    let apps = list.apps();
    let rows = rows(&apps, muted, &settings.audio_apps);
    let silenced = rows.iter().filter(|r| r.muted && !r.absent).count();
    let title = match silenced {
        0 => "Mute apps".to_string(),
        n => format!("Mute apps · {n}"),
    };
    let mut changed = None;
    egui::ComboBox::from_id_salt("mute-apps")
        .selected_text(title)
        .width(220.0)
        .height(240.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            ui.set_min_width(260.0);
            if rows.is_empty() {
                ui.weak("No app is playing sound right now.");
            }
            for row in &rows {
                let mut text = RichText::new(&row.name);
                if row.absent {
                    text = RichText::new(format!("{} (not running)", row.name)).weak();
                }
                let mut value = row.muted;
                let mut response = ui.checkbox(&mut value, text);
                if row.voice_chat {
                    response = response.on_hover_text(format!(
                        "{}: voice chat, muted unless you unmute it",
                        row.key
                    ));
                } else {
                    response = response.on_hover_text(&row.key);
                }
                if response.changed() {
                    changed = Some((row.key.clone(), value));
                }
            }
            ui.separator();
            if applies {
                ui.weak("Viewers don't hear muted apps; you still do.");
                if silenced > 0 {
                    ui.weak("Windows notification sounds aren't shared while an app is muted.");
                }
            } else {
                ui.weak("This share is one window. Mute apps applies when you share a monitor.");
            }
        });
    if let Some((key, value)) = changed {
        tracing::info!(app = %key, muted = value, "mute choice changed");
        muted.set(&key, value);
        settings.audio_apps = muted.choices();
        if let Err(e) = settings.save(dir) {
            tracing::warn!("could not save settings: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(key: &str, name: &str) -> AudioApp {
        AudioApp {
            key: key.into(),
            name: name.into(),
            pids: vec![1],
        }
    }

    #[test]
    fn playing_apps_come_first_then_remembered_mutes_that_are_away() {
        let apps = [
            app("discord.exe", "Discord"),
            app("chrome.exe", "Google Chrome"),
        ];
        let choices = BTreeMap::from([
            ("chrome.exe".to_string(), false),
            ("ts3client_win64.exe".to_string(), true),
            ("game.exe".to_string(), false),
        ]);
        let muted = MutedApps::new(choices.clone());
        let rows = rows(&apps, &muted, &choices);
        let summary: Vec<(&str, bool, bool, bool)> = rows
            .iter()
            .map(|r| (r.name.as_str(), r.muted, r.voice_chat, r.absent))
            .collect();
        assert_eq!(
            summary,
            [
                ("Discord", true, true, false),
                ("Google Chrome", false, false, false),
                ("Ts3client_win64", true, true, true),
            ]
        );
    }

    #[test]
    fn the_live_audio_line_names_what_is_left_out() {
        let apps = [
            app("discord.exe", "Discord"),
            app("chrome.exe", "Google Chrome"),
        ];
        assert_eq!(
            except_text(&apps, &MutedApps::default()),
            "except Peeroxide and Discord"
        );
        let muted = MutedApps::new([("chrome.exe".to_string(), true)]);
        assert_eq!(
            except_text(&apps, &muted),
            "except Peeroxide, Discord and Google Chrome"
        );
        let none = MutedApps::new([("discord.exe".to_string(), false)]);
        assert_eq!(except_text(&apps, &none), "except Peeroxide");
    }
}
