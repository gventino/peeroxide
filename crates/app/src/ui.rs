use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Color32, RichText};
use peeroxide_capture::{Source, SourceKind, list_sources};
use peeroxide_codec::Preset;
use peeroxide_discovery::Peer;
use peeroxide_net::{Fingerprint, Identity, SessionEvent, SessionId, StopReason};

use crate::Args;
use crate::audio_encoder::{audio_source, describe as describe_audio};
use crate::contacts::{Contact, Contacts, ago};
use crate::controller::{Controller, Event, PeerTarget, local_ipv4s};
use crate::encoder::EncoderEnd;
use crate::settings::Settings;
use crate::video::VideoView;
use crate::viewer_state::{PeerRef, ViewerInput, ViewerState, describe};

const LIVE_RED: Color32 = Color32::from_rgb(230, 70, 70);
const MAX_NAME_CHARS: usize = 40;

pub struct App {
    ctrl: Controller,
    settings: Settings,
    dir: PathBuf,
    name_edit: Option<String>,
    sources: Vec<Source>,
    selected: usize,
    preset: Preset,
    viewer_count: usize,
    broadcast_note: Option<String>,
    viewer: ViewerState,
    session: Option<SessionId>,
    peers: Vec<Peer>,
    contacts: Contacts,
    watch_target: Option<PeerTarget>,
    connect_input: String,
    watch_note: Option<String>,
    /// Whether the broadcaster we're watching shares audio.
    stream_audio: bool,
    video: VideoView,
    autostart: bool,
    autowatch: Option<String>,
}

impl App {
    pub fn new(
        args: Args,
        identity: Identity,
        display_name: String,
        settings: Settings,
        dir: PathBuf,
        ctx: egui::Context,
    ) -> anyhow::Result<Self> {
        let mut app = Self {
            ctrl: Controller::new(identity, display_name, ctx)?,
            preset: settings.preset(),
            settings,
            contacts: Contacts::load(&dir),
            watch_target: None,
            dir,
            name_edit: None,
            sources: Vec::new(),
            selected: 0,
            viewer_count: 0,
            broadcast_note: None,
            viewer: ViewerState::Idle,
            session: None,
            peers: Vec::new(),
            connect_input: String::new(),
            watch_note: None,
            stream_audio: false,
            video: VideoView::default(),
            autostart: false,
            autowatch: args.watch.as_ref().map(|w| w.to_lowercase()),
        };
        app.ctrl.output.set_volume(app.settings.volume());
        app.ctrl.output.set_muted(app.settings.muted);
        app.refresh_sources();
        if args.share_audio {
            app.settings.share_audio = true;
        }
        if let Some(query) = &args.broadcast {
            match find_source(&app.sources, query) {
                Some(i) => {
                    app.selected = i;
                    app.autostart = true;
                }
                None => app.broadcast_note = Some(format!("No source matches \"{query}\"")),
            }
        }
        if let Some(connect) = &args.connect {
            app.connect_input = connect.clone();
            match parse_connect(connect) {
                Ok(target) => app.watch(target),
                Err(e) => app.watch_note = Some(e),
            }
        }
        Ok(app)
    }

    fn save_settings(&self) {
        if let Err(e) = self.settings.save(&self.dir) {
            tracing::warn!("could not save settings: {e}");
        }
    }

    fn identity_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(edit) = &mut self.name_edit {
            let response = ui.add(
                egui::TextEdit::singleline(edit)
                    .char_limit(MAX_NAME_CHARS)
                    .desired_width(220.0),
            );
            response.request_focus();
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if ui.button("Save").clicked() || enter {
                let name = edit.trim().to_string();
                if !name.is_empty() {
                    self.ctrl.set_display_name(name.clone());
                    self.settings.display_name = Some(name);
                    self.save_settings();
                }
                self.name_edit = None;
            } else if escape {
                self.name_edit = None;
            }
        } else {
            ui.label(RichText::new(self.ctrl.display_name()).strong());
            let live = self.ctrl.broadcast.is_some();
            let edit = ui
                .add_enabled(!live, egui::Button::new("✏").small())
                .on_hover_text("Change the name others see")
                .on_disabled_hover_text("Stop broadcasting to change your name");
            if edit.clicked() {
                self.name_edit = Some(self.ctrl.display_name().to_string());
            }
        }
        ui.weak(format!("ID {}", self.ctrl.fingerprint().short()))
            .on_hover_text(
                "Your fingerprint. Viewers see it next to your name and can compare it \
                 with you to rule out impersonation.",
            );
    }

    fn refresh_sources(&mut self) {
        let previous = self.sources.get(self.selected).cloned();
        let mut sources = match list_sources() {
            Ok(s) => s,
            Err(e) => {
                self.broadcast_note = Some(format!("Could not list sources: {e}"));
                Vec::new()
            }
        };
        sources.push(Source::test_pattern());
        self.selected = previous
            .and_then(|p| sources.iter().position(|s| *s == p))
            .unwrap_or(0);
        self.sources = sources;
    }

    fn start_broadcast(&mut self) {
        let Some(source) = self.sources.get(self.selected).cloned() else {
            return;
        };
        self.broadcast_note = None;
        self.viewer_count = 0;
        match self.ctrl.start_broadcast(
            source,
            self.preset,
            self.settings.broadcast_port,
            self.settings.share_audio,
        ) {
            Ok(port) if self.settings.broadcast_port != Some(port) => {
                self.settings.broadcast_port = Some(port);
                self.save_settings();
            }
            Ok(_) => {}
            Err(e) => self.broadcast_note = Some(format!("Could not start broadcasting: {e}")),
        }
    }

    fn watch(&mut self, target: PeerTarget) {
        self.watch_note = None;
        self.video.clear();
        self.stream_audio = false;
        self.viewer = self.viewer.transition(ViewerInput::Select(PeerRef {
            fingerprint: target.fingerprint,
            name: target.name.clone(),
        }));
        self.watch_target = Some(target.clone());
        self.session = Some(self.ctrl.watch(target));
    }

    /// The peer with a live or pending session (highlighted; clicking it again does nothing).
    fn active_peer(&self) -> Option<String> {
        self.viewer
            .wants_session()
            .then(|| self.viewer.peer())
            .flatten()
            .map(|p| p.fingerprint.to_hex())
    }

    fn remember_contact(&mut self, name: &str, remote: SocketAddr) {
        let Some(target) = &self.watch_target else {
            return;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.contacts
            .remember(&target.fingerprint, name, remote, &target.addrs, now);
        if let Err(e) = self.contacts.save(&self.dir) {
            tracing::warn!("could not save contacts: {e}");
        }
    }

    fn apply(&mut self, input: ViewerInput) {
        self.viewer = self.viewer.transition(input);
        if !self.viewer.wants_session() && self.session.take().is_some() {
            self.ctrl.stop_watching();
            self.video.clear();
        }
    }

    fn handle_events(&mut self) {
        while let Ok(event) = self.ctrl.events.try_recv() {
            match event {
                Event::ViewerCount(n) => self.viewer_count = n,
                Event::Peers(peers) => {
                    self.peers = peers;
                    let wanted = self.autowatch.as_deref().and_then(|q| {
                        self.peers
                            .iter()
                            .find(|p| p.name.to_lowercase().contains(q))
                            .and_then(PeerTarget::from_peer)
                    });
                    if let Some(target) = wanted {
                        self.autowatch = None;
                        self.watch(target);
                    }
                }
                Event::BroadcastEnded { generation, end } => {
                    let current = self.ctrl.broadcast.as_ref().map(|b| b.generation);
                    if current != Some(generation) || end == EncoderEnd::Stopped {
                        continue;
                    }
                    let (reason, note) = match end {
                        EncoderEnd::SourceClosed => (
                            StopReason::SourceClosed,
                            "The shared source was closed".to_string(),
                        ),
                        EncoderEnd::Failed(e) => {
                            (StopReason::Stopped, format!("Capture failed: {e}"))
                        }
                        EncoderEnd::Stopped => unreachable!(),
                    };
                    self.ctrl.stop_broadcast(reason);
                    self.viewer_count = 0;
                    self.broadcast_note = Some(note);
                }
                Event::AudioEnded { generation, error } => {
                    if let Some(b) = &mut self.ctrl.broadcast
                        && b.generation == generation
                    {
                        b.audio = None;
                        b.audio_note =
                            Some(format!("Audio stopped ({error}); still sharing video"));
                    }
                }
                Event::Session(id, event) => {
                    if self.session != Some(id) {
                        continue;
                    }
                    let input = match event {
                        SessionEvent::Connected {
                            broadcaster_name,
                            remote,
                            audio,
                        } => {
                            tracing::info!(broadcaster = %broadcaster_name, audio, "watching");
                            self.stream_audio = audio;
                            self.remember_contact(&broadcaster_name, remote);
                            ViewerInput::Connected { broadcaster_name }
                        }
                        SessionEvent::Ended(end) => {
                            if matches!(self.viewer, ViewerState::Connecting { .. }) {
                                self.watch_note = Some(describe(&end));
                            }
                            ViewerInput::Ended(end)
                        }
                    };
                    self.apply(input);
                }
            }
        }
    }

    fn broadcast_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Broadcast");
        ui.add_space(4.0);
        let live = self.ctrl.broadcast.is_some();
        ui.add_enabled_ui(!live, |ui| {
            ui.horizontal(|ui| {
                ui.label("Source");
                if ui.small_button("⟳").on_hover_text("Refresh").clicked() {
                    self.refresh_sources();
                }
            });
            let current = self
                .sources
                .get(self.selected)
                .map(|s| s.name.clone())
                .unwrap_or_default();
            egui::ComboBox::from_id_salt("source")
                .width(ui.available_width())
                .selected_text(truncate(&current, 38))
                .show_ui(ui, |ui| {
                    for (i, s) in self.sources.iter().enumerate() {
                        let icon = match s.kind {
                            SourceKind::Monitor => "🖥",
                            SourceKind::Window => "🗔",
                            SourceKind::TestPattern => "▦",
                        };
                        ui.selectable_value(
                            &mut self.selected,
                            i,
                            format!("{icon} {}", truncate(&s.name, 60)),
                        );
                    }
                });
            ui.label("Quality");
            let before = self.preset;
            egui::ComboBox::from_id_salt("preset")
                .width(ui.available_width())
                .selected_text(self.preset.name)
                .show_ui(ui, |ui| {
                    for p in Preset::ALL {
                        let hint = format!(
                            "Up to {}x{}, about {} Mbps of upload{}",
                            p.max_width,
                            p.max_height,
                            p.bitrate_bps / 1_000_000,
                            if p == Preset::INTERNET {
                                ". Recommended for Radmin VPN, Hamachi and other internet links."
                            } else {
                                ""
                            }
                        );
                        ui.selectable_value(&mut self.preset, p, p.name)
                            .on_hover_text(hint);
                    }
                });
            if self.preset != before {
                self.settings.preset = Some(self.preset.name.into());
                self.save_settings();
            }
            if !live {
                self.share_audio_ui(ui);
            }
        });
        ui.add_space(6.0);

        if let Some(b) = &self.ctrl.broadcast {
            ui.horizontal(|ui| {
                ui.label(RichText::new("● LIVE").color(LIVE_RED).strong());
                let n = self.viewer_count;
                ui.label(format!("{n} viewer{}", if n == 1 { "" } else { "s" }));
            });
            ui.label(format!("Sharing {}", truncate(&b.source_name, 40)));
            match &b.audio {
                Some(a) => ui.label(format!("🔊 With audio: {}", describe_audio(&a.source))),
                None => ui.weak("🔇 No audio"),
            };
            if self.viewer_count == 0 {
                ui.weak("Capture paused until someone watches");
            } else {
                let mut stats = b.encoder.stats.lock().unwrap();
                let r = stats.meter.rates();
                let size = stats
                    .canvas
                    .map(|(w, h)| format!("{w}x{h} · "))
                    .unwrap_or_default();
                let audio = b
                    .audio
                    .as_ref()
                    .map(|a| {
                        let kbps = a.stats.lock().unwrap().meter.rates().kbps;
                        format!(" · audio {kbps:.0} kbps")
                    })
                    .unwrap_or_default();
                ui.weak(format!(
                    "{size}{:.0} fps · {:.0} kbps · encode {:.1} ms{audio}",
                    r.fps, r.kbps, r.avg_ms
                ));
            }
            if let Some(note) = &b.audio_note {
                ui.colored_label(ui.visuals().warn_fg_color, note);
            }
            let (port, fingerprint) = (b.port, self.ctrl.fingerprint().to_hex());
            ui.horizontal(|ui| {
                ui.weak(format!("UDP port {port}"));
                ui.menu_button("Copy connect string", |ui| {
                    ui.weak("For viewers who can't see you in the list.\nPick the network they share with you:");
                    let adapters = local_ipv4s();
                    if adapters.is_empty() {
                        ui.weak("No network adapters found");
                    }
                    for (adapter, ip) in adapters {
                        let connect = format!("{ip}:{port}#{fingerprint}");
                        if ui
                            .button(format!("{adapter} · {ip}"))
                            .on_hover_text(&connect)
                            .clicked()
                        {
                            ui.ctx().copy_text(connect);
                            ui.close();
                        }
                    }
                });
            });
            ui.add_space(4.0);
            if ui.button("■ Stop broadcasting").clicked() {
                self.ctrl.stop_broadcast(StopReason::Stopped);
                self.viewer_count = 0;
            }
        } else if ui.button("▶ Start broadcasting").clicked() {
            self.start_broadcast();
        }
        if let Some(note) = &self.broadcast_note {
            ui.colored_label(ui.visuals().warn_fg_color, note);
        }
    }

    /// "Share audio" checkbox, with exactly what it would capture for the selected source.
    fn share_audio_ui(&mut self, ui: &mut egui::Ui) {
        let audio = self.sources.get(self.selected).and_then(audio_source);
        let supported = audio.as_ref().is_some_and(peeroxide_audio::is_supported);
        ui.add_space(2.0);
        let checkbox = ui
            .add_enabled(
                supported,
                egui::Checkbox::new(&mut self.settings.share_audio, "Share audio"),
            )
            .on_disabled_hover_text(if audio.is_none() {
                "Can't tell which app plays this window's sound"
            } else {
                "Audio sharing isn't available on this system yet"
            });
        if checkbox.changed() {
            self.save_settings();
        }
        if let Some(audio) = audio.filter(|_| supported && self.settings.share_audio) {
            let scope = describe_audio(&audio);
            let mut chars = scope.chars();
            let scope: String = chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default();
            ui.weak(format!("{scope}. Your microphone is never shared."));
        }
    }

    fn peer_list_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Broadcasting on this network").strong());
        if self.peers.is_empty() {
            let text = self
                .ctrl
                .discovery_error
                .as_deref()
                .unwrap_or("No one is broadcasting yet");
            ui.weak(text);
            return;
        }
        let watched = self.active_peer();
        let mut picked = None;
        egui::ScrollArea::vertical()
            .max_height(240.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for peer in &self.peers {
                    let short = Fingerprint::from_hex(&peer.fingerprint)
                        .map(|f| f.short())
                        .unwrap_or_default();
                    let same_name = self.peers.iter().filter(|p| p.name == peer.name).count() > 1;
                    let selected = watched.as_deref() == Some(peer.fingerprint.as_str());
                    let saved = self.contacts.get(&peer.fingerprint).is_some();
                    let changed_id = self.contacts.name_conflict(&peer.name, &peer.fingerprint);
                    ui.horizontal(|ui| {
                        let addrs: Vec<String> =
                            peer.addrs.iter().map(ToString::to_string).collect();
                        let icon = if saved { "★" } else { "🖵" };
                        let row = ui
                            .selectable_label(
                                selected,
                                format!("{icon} {}", truncate(&peer.name, 24)),
                            )
                            .on_hover_text(format!(
                                "ID {short}\nFingerprint {}\n{}",
                                peer.fingerprint,
                                addrs.join("\n")
                            ));
                        ui.weak(&short);
                        if let Some(old) = changed_id {
                            let old_id = Fingerprint::from_hex(&old.fingerprint)
                                .map(|f| f.short())
                                .unwrap_or_default();
                            ui.colored_label(ui.visuals().error_fg_color, "⚠")
                                .on_hover_text(format!(
                                    "Not the {} you saved: their ID changed (was {old_id}).\n\
                                     This happens after a reinstall, but it can also be someone \
                                     impersonating them. Confirm the ID with the person.",
                                    old.name
                                ));
                        } else if same_name && !saved {
                            ui.colored_label(ui.visuals().warn_fg_color, "⚠")
                                .on_hover_text(
                                    "Another broadcaster uses the same name. Check the ID with the \
                                 person you expect before trusting what you see.",
                                );
                        }
                        if row.clicked() && !selected {
                            picked = PeerTarget::from_peer(peer);
                        }
                    });
                }
            });
        if let Some(target) = picked {
            self.watch(target);
        }
    }

    /// Contacts that are not currently announcing themselves; reachable by their saved address.
    fn saved_ui(&mut self, ui: &mut egui::Ui) {
        let offline: Vec<Contact> = self
            .contacts
            .list()
            .iter()
            .filter(|c| !self.peers.iter().any(|p| p.fingerprint == c.fingerprint))
            .cloned()
            .collect();
        if offline.is_empty() {
            return;
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Saved").strong())
            .on_hover_text("People you watched before. Click to connect at their last address.");
        let watched = self.active_peer();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let (mut picked, mut removed) = (None, None);
        egui::ScrollArea::vertical()
            .id_salt("saved")
            .max_height(200.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for c in &offline {
                    let Some(fingerprint) = Fingerprint::from_hex(&c.fingerprint) else {
                        continue;
                    };
                    let selected = watched.as_deref() == Some(c.fingerprint.as_str());
                    ui.horizontal(|ui| {
                        let addrs: Vec<String> = c.addrs.iter().map(ToString::to_string).collect();
                        let row = ui
                            .selectable_label(selected, format!("☆ {}", truncate(&c.name, 22)))
                            .on_hover_text(format!(
                                "ID {}\nLast watched {}\n{}",
                                fingerprint.short(),
                                ago(now, c.last_seen),
                                addrs.join("\n")
                            ));
                        ui.weak(fingerprint.short());
                        if ui.small_button("🗑").on_hover_text("Forget").clicked() {
                            removed = Some(c.fingerprint.clone());
                        }
                        if row.clicked() && !selected {
                            picked = Some(PeerTarget {
                                fingerprint,
                                name: c.name.clone(),
                                addrs: c.addrs.clone(),
                            });
                        }
                    });
                }
            });
        if let Some(fp) = removed {
            self.contacts.remove(&fp);
            if let Err(e) = self.contacts.save(&self.dir) {
                tracing::warn!("could not save contacts: {e}");
            }
        }
        if let Some(target) = picked {
            self.watch(target);
        }
    }

    fn watch_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Watch");
        ui.add_space(4.0);
        self.peer_list_ui(ui);
        self.saved_ui(ui);
        ui.add_space(8.0);

        match self.viewer.clone() {
            ViewerState::Idle => {
                ui.weak("Not watching");
            }
            ViewerState::Connecting { peer } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Connecting to {}…", peer.name));
                });
                if ui.button("Cancel").clicked() {
                    self.apply(ViewerInput::StopWatching);
                }
            }
            ViewerState::Streaming {
                broadcaster_name, ..
            } => {
                ui.label(format!("Watching {broadcaster_name}"));
                self.volume_ui(ui);
                if ui.button("■ Stop watching").clicked() {
                    self.apply(ViewerInput::StopWatching);
                }
            }
            ViewerState::Disconnected { reason, .. } => {
                ui.colored_label(ui.visuals().warn_fg_color, reason);
                if ui.button("OK").clicked() {
                    self.apply(ViewerInput::Acknowledge);
                }
            }
        }
        if let Some(note) = &self.watch_note {
            ui.colored_label(ui.visuals().warn_fg_color, note);
        }

        ui.add_space(8.0);
        egui::CollapsingHeader::new("Connect manually").show(ui, |ui| {
            ui.weak("IP:PORT#FINGERPRINT, for networks where discovery is blocked");
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.connect_input)
                    .desired_width(ui.available_width())
                    .hint_text("192.168.0.10:50123#3f9a…"),
            );
            let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Watch").clicked() || submitted {
                match parse_connect(&self.connect_input) {
                    Ok(target) => self.watch(target),
                    Err(e) => self.watch_note = Some(e),
                }
            }
        });
    }

    /// Mute button and volume slider, or why there is nothing to hear.
    fn volume_ui(&mut self, ui: &mut egui::Ui) {
        if !self.stream_audio {
            ui.weak("🔇 The broadcaster isn't sharing audio");
            return;
        }
        ui.horizontal(|ui| {
            let muted = self.settings.muted;
            let mut percent = self.settings.volume.unwrap_or(100).min(100);
            let icon = if muted || percent == 0 {
                "🔇"
            } else {
                "🔊"
            };
            if ui
                .button(icon)
                .on_hover_text(if muted { "Unmute" } else { "Mute" })
                .clicked()
            {
                self.settings.muted = !muted;
                self.ctrl.output.set_muted(!muted);
                self.save_settings();
            }
            let slider = ui
                .add(egui::Slider::new(&mut percent, 0..=100).suffix("%"))
                .on_hover_text("Volume of this stream, on this computer only");
            if slider.changed() {
                self.settings.volume = Some(percent);
                self.ctrl.output.set_volume(f32::from(percent) / 100.0);
                if muted && percent > 0 {
                    self.settings.muted = false;
                    self.ctrl.output.set_muted(false);
                }
            }
            // Save once the drag ends rather than on every step of it.
            if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                self.save_settings();
            }
        });
        let error = self
            .ctrl
            .watching
            .as_ref()
            .and_then(|w| w.audio_stats.lock().unwrap().output_error.clone());
        if let Some(e) = error {
            ui.colored_label(ui.visuals().warn_fg_color, format!("Can't play audio: {e}"));
        }
    }

    fn watch_overlay(&self) -> String {
        let Some(w) = &self.ctrl.watching else {
            return String::new();
        };
        let mut s = w.decoder_stats.lock().unwrap();
        let r = s.meter.rates();
        let latency = s
            .latency_ms
            .map(|l| format!("\ncapture→decode {l:.0} ms (valid only on the same machine)"))
            .unwrap_or_default();
        let audio = if self.stream_audio {
            let mut a = w.audio_stats.lock().unwrap();
            let kbps = a.meter.rates().kbps;
            let sync = a
                .playout
                .av_offset_ms
                .map(|ms| format!("  A/V {ms:+.0} ms"))
                .unwrap_or_default();
            format!(
                "\naudio {kbps:.0} kbps  buffer {:.0} ms{sync}  underruns {}  bad {}",
                a.playout.buffer_ms, a.underruns, a.bad_packets
            )
        } else {
            String::new()
        };
        format!(
            "{:.1} fps  {:.0} kbps  decode {:.1} ms  dropped {}{latency}{audio}",
            r.fps, r.kbps, r.avg_ms, s.dropped
        )
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if std::mem::take(&mut self.autostart) {
            self.start_broadcast();
        }
        self.handle_events();
        if self.ctrl.broadcast.is_some() || self.ctrl.watching.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }

        egui::Panel::top("identity").show(ui, |ui| {
            ui.horizontal(|ui| self.identity_ui(ui));
        });

        egui::Panel::left("controls")
            .resizable(false)
            .exact_size(320.0)
            .show(ui, |ui| {
                ui.add_space(8.0);
                self.broadcast_ui(ui);
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);
                self.watch_ui(ui);
            });

        egui::CentralPanel::default().show(ui, |ui| match self.viewer.clone() {
            ViewerState::Streaming { .. } => {
                let overlay = self.watch_overlay();
                self.video.ui(ui, &self.ctrl.video, &overlay);
            }
            ViewerState::Connecting { peer } => {
                VideoView::placeholder(ui, &format!("Connecting to {}…", peer.name));
            }
            ViewerState::Disconnected { peer, reason } => {
                VideoView::placeholder(ui, &format!("{}: {reason}", peer.name));
            }
            ViewerState::Idle => VideoView::placeholder(ui, "Pick a broadcaster to watch"),
        });
    }
}

pub fn parse_connect(s: &str) -> Result<PeerTarget, String> {
    let (addr, fp) = s
        .trim()
        .split_once('#')
        .ok_or("Expected IP:PORT#FINGERPRINT")?;
    let addr: SocketAddr = addr.trim().parse().map_err(|_| "Invalid IP:PORT")?;
    let fingerprint = Fingerprint::from_hex(fp.trim())
        .ok_or("Invalid fingerprint (expected 64 hex characters)")?;
    Ok(PeerTarget {
        fingerprint,
        name: fingerprint.short(),
        addrs: vec![addr],
    })
}

fn find_source(sources: &[Source], query: &str) -> Option<usize> {
    let q = query.to_lowercase();
    sources.iter().position(|s| match q.as_str() {
        "test" => s.kind == SourceKind::TestPattern,
        "monitor" => s.kind == SourceKind::Monitor,
        _ => s.name.to_lowercase().contains(&q),
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_connect_strings() {
        let fp = Fingerprint::of(b"x");
        let t = parse_connect(&format!(" 127.0.0.1:5000#{} ", fp.to_hex())).unwrap();
        assert_eq!(
            t.addrs,
            vec!["127.0.0.1:5000".parse::<SocketAddr>().unwrap()]
        );
        assert_eq!(t.fingerprint, fp);
        assert!(parse_connect("127.0.0.1:5000").is_err());
        assert!(parse_connect(&format!("nonsense#{}", fp.to_hex())).is_err());
        assert!(parse_connect("127.0.0.1:5000#abcd").is_err());
    }

    #[test]
    fn truncates_on_char_boundaries() {
        assert_eq!(truncate("héllo wörld", 5), "héll…");
        assert_eq!(truncate("short", 10), "short");
    }
}
