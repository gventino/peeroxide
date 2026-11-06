use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use eframe::egui::{
    self, Align, Color32, ColorImage, FontId, Layout, Rect, RichText, Sense, Stroke, TextureHandle,
    TextureOptions, vec2,
};
use peeroxide_audio::{AudioSource, MutedApps};
use peeroxide_capture::{Source, SourceKind, list_sources};
use peeroxide_codec::{Codec, Preset, hardware_h265_encoder};
use peeroxide_discovery::{LOOKUP_PORT, Peer, merge};
use peeroxide_net::{Fingerprint, Identity, SessionEvent, SessionId, StopReason};

use crate::Args;
use crate::audio_encoder::{audio_source, describe as describe_audio};
use crate::contacts::{Contact, Contacts, ago};
use crate::controller::{Controller, Event, PeerTarget, local_ipv4s};
use crate::encoder::{EncoderEnd, PreviewFrame};
use crate::fullscreen;
use crate::mini_player::{self, Action};
use crate::mute_apps;
use crate::settings::{Palette, Settings, ThemeChoice};
use crate::theme::{self, Station};
use crate::thumbs::Thumbnails;
use crate::video::VideoView;
use crate::viewer_state::{PeerRef, ViewerInput, ViewerState};

const FULLSCREEN_BAR_HEIGHT: f32 = 56.0;
const COLUMN_WIDTH: f32 = 340.0;
const MAX_NAME_CHARS: usize = 40;

pub struct App {
    ctrl: Controller,
    settings: Settings,
    dir: PathBuf,
    name_edit: Option<String>,
    sources: Vec<Source>,
    selected: usize,
    preset: Preset,
    /// Preferred codec for broadcasts (`--codec`).
    codec: Codec,
    /// The GPU's H.265 encoder, if there is one.
    h265_encoder: Option<&'static str>,
    /// Apps left out of a shared monitor's sound; shared with a running broadcast.
    muted_apps: MutedApps,
    viewer_count: usize,
    broadcast_note: Option<String>,
    viewer: ViewerState,
    session: Option<SessionId>,
    /// Everyone broadcasting that we know of: `mdns_peers` and `found_peers` merged.
    peers: Vec<Peer>,
    mdns_peers: Vec<Peer>,
    /// Answering lookups: saved contacts and the tailnet's devices.
    found_peers: Vec<Peer>,
    contacts: Contacts,
    watch_target: Option<PeerTarget>,
    connect_input: String,
    watch_note: Option<String>,
    /// Whether the broadcaster we're watching shares audio.
    stream_audio: bool,
    /// Shared with the mini player, which draws the same texture.
    video: Arc<Mutex<VideoView>>,
    mini: Arc<mini_player::Shared>,
    /// Where the mini player's window was created; `Some` while a stream is showing.
    mini_rect: Option<egui::Rect>,
    mini_visible: bool,
    autostart: bool,
    autowatch: Option<String>,
    update_note: Option<UpdateNote>,
    /// Asked for this frame by a button or a double-click; applied at the end of the frame.
    fullscreen_toggle: bool,
    fullscreen_exit: bool,
    /// When this app last entered fullscreen.
    fullscreen_since: Option<Instant>,
    /// The source picker is open.
    picker: bool,
    /// The settings window is open.
    settings_open: bool,
    /// The broadcast port as typed in the settings window.
    port_input: String,
    thumbs: Thumbnails,
    /// Last copy of a frame this machine already captured, shown in the well while broadcasting.
    preview: Option<TextureHandle>,
}

/// What the updater has to say in the top bar, e.g. "Updated to 0.5.0".
#[derive(Clone, Debug)]
pub struct UpdateNote {
    pub text: String,
    /// A page to download the update by hand.
    pub link: Option<String>,
    pub warn: bool,
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
        egui_extras::install_image_loaders(&ctx);
        let thumbs = Thumbnails::new(ctx.clone());
        let ctrl = Controller::new(identity, display_name, !settings.skip_tailscale, ctx)?;
        let video = Arc::new(Mutex::new(VideoView::default()));
        let mini = mini_player::Shared::new(video.clone(), ctrl.video.clone(), ctrl.output.clone());
        let mut app = Self {
            ctrl,
            preset: settings.preset(),
            codec: args.codec.into(),
            h265_encoder: hardware_h265_encoder(),
            muted_apps: MutedApps::new(settings.audio_apps.clone()),
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
            mdns_peers: Vec::new(),
            found_peers: Vec::new(),
            connect_input: String::new(),
            watch_note: None,
            stream_audio: false,
            video,
            mini,
            mini_rect: None,
            mini_visible: false,
            autostart: false,
            autowatch: args.watch.as_ref().map(|w| w.to_lowercase()),
            update_note: None,
            fullscreen_toggle: false,
            fullscreen_exit: false,
            fullscreen_since: None,
            picker: false,
            settings_open: false,
            port_input: String::new(),
            thumbs,
            preview: None,
        };
        app.ctrl.output.set_volume(app.settings.volume());
        app.ctrl.output.set_muted(app.settings.muted);
        app.sync_lookup_contacts();
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
                Err(e) => app.watch_note = Some(e.to_string()),
            }
        }
        Ok(app)
    }

    pub fn set_update_note(&mut self, note: UpdateNote) {
        self.update_note = Some(note);
    }

    fn update_note_ui(&mut self, ui: &mut egui::Ui) {
        let Some(note) = &self.update_note else {
            return;
        };
        if note.warn {
            ui.colored_label(ui.visuals().warn_fg_color, &note.text);
        } else {
            ui.weak(&note.text);
        }
        if let Some(link) = &note.link {
            ui.hyperlink_to("Download", link);
        }
        if ui.small_button("×").on_hover_text("Dismiss").clicked() {
            self.update_note = None;
        }
    }

    fn save_settings(&self) {
        if let Err(e) = self.settings.save(&self.dir) {
            tracing::warn!("could not save settings: {e:#}");
        }
    }

    /// The first row of the column: the name and ID, and the settings key at the right end.
    fn identity_ui(&mut self, ui: &mut egui::Ui) {
        let station = station_of(ui);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if gear_button(ui, &station)
                    .on_hover_text("Settings")
                    .clicked()
                {
                    self.open_settings();
                }
                // What the gear leaves, read left to right.
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    self.nameplate(ui, &station);
                });
            });
        });
        if self.update_note.is_some() {
            ui.horizontal_wrapped(|ui| self.update_note_ui(ui));
        }
    }

    /// Display name and short ID. The ID steps aside while the name is edited.
    fn nameplate(&mut self, ui: &mut egui::Ui, station: &Station) {
        if let Some(edit) = &mut self.name_edit {
            let response = ui.add(
                egui::TextEdit::singleline(edit)
                    .char_limit(MAX_NAME_CHARS)
                    .desired_width((ui.available_width() - 60.0).max(80.0)),
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
            return;
        }
        let name = self.ctrl.display_name();
        ui.label(
            RichText::new(truncate(name, 18))
                .color(station.body)
                .strong(),
        )
        .on_hover_text(name);
        let live = self.ctrl.broadcast.is_some();
        let edit = ui
            .add_enabled(!live, egui::Button::new("Edit").small())
            .on_hover_text("Change the name others see")
            .on_disabled_hover_text("Stop broadcasting to change your name");
        if edit.clicked() {
            self.name_edit = Some(self.ctrl.display_name().to_string());
        }
        ui.label(RichText::new(self.ctrl.fingerprint().short()).color(station.muted))
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
            self.codec,
            self.settings.broadcast_port,
            self.settings.share_audio,
            self.muted_apps.clone(),
        ) {
            Ok(port) if self.settings.broadcast_port != Some(port) => {
                self.settings.broadcast_port = Some(port);
                self.save_settings();
            }
            Ok(_) => {}
            Err(e) => self.broadcast_note = Some(format!("Could not start broadcasting: {e:#}")),
        }
    }

    fn watch(&mut self, target: PeerTarget) {
        self.watch_note = None;
        self.video.lock().unwrap().clear();
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
            tracing::warn!("could not save contacts: {e:#}");
        }
        self.sync_lookup_contacts();
    }

    /// Saved contacts are asked at their saved addresses whether they are broadcasting.
    fn sync_lookup_contacts(&self) {
        let ips = self
            .contacts
            .list()
            .iter()
            .flat_map(|c| c.addrs.iter().map(SocketAddr::ip))
            .collect();
        self.ctrl.set_lookup_contacts(ips);
    }

    /// The list shown, from both sources; `--watch` starts as soon as its broadcaster shows up.
    fn refresh_peers(&mut self) {
        self.peers = merge(&self.mdns_peers, &self.found_peers);
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

    fn apply(&mut self, input: ViewerInput) {
        self.viewer = self.viewer.transition(input);
        if !self.viewer.wants_session() && self.session.take().is_some() {
            self.ctrl.stop_watching();
            self.video.lock().unwrap().clear();
        }
    }

    fn handle_events(&mut self) {
        while let Ok(event) = self.ctrl.events.try_recv() {
            match event {
                Event::ViewerCount(n) => self.viewer_count = n,
                Event::Peers(peers) => {
                    self.mdns_peers = peers;
                    self.refresh_peers();
                }
                Event::Found(peers) => {
                    self.found_peers = peers;
                    self.refresh_peers();
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
                                self.watch_note = Some(end.to_string());
                            }
                            ViewerInput::Ended(end)
                        }
                    };
                    self.apply(input);
                }
            }
        }
    }

    fn station_column(&mut self, ui: &mut egui::Ui) {
        self.identity_ui(ui);
        ui.add_space(14.0);
        let live = self.ctrl.broadcast.is_some();
        if live {
            self.live_signal(ui);
        } else {
            self.setup_column(ui);
        }
        ui.add_space(12.0);
        self.peer_list_ui(ui);
        self.saved_ui(ui);
        ui.add_space(8.0);
        self.watch_status(ui);
        if let Some(note) = &self.broadcast_note {
            ui.colored_label(ui.visuals().warn_fg_color, note);
        }
    }

    fn setup_column(&mut self, ui: &mut egui::Ui) {
        let station = station_of(ui);
        if column_key(ui, &station, "Start broadcasting")
            .on_hover_text("Choose a window and quality")
            .clicked()
        {
            self.open_picker();
        }
    }

    fn live_signal(&mut self, ui: &mut egui::Ui) {
        let station = station_of(ui);
        let viewers = self.viewer_count;
        let show_stats = self.settings.show_stats;
        let Some(signal) = self.ctrl.broadcast.as_ref().map(|broadcast| {
            let stats_line = (viewers > 0 && show_stats).then(|| {
                let mut stats = broadcast.encoder.stats.lock().unwrap();
                let rates = stats.meter.rates();
                let size = stats
                    .canvas
                    .map(|(w, h)| format!("{w}×{h} · "))
                    .unwrap_or_default();
                let codec = stats
                    .codec
                    .as_ref()
                    .map(|c| format!(" · {c}"))
                    .unwrap_or_default();
                let audio = broadcast
                    .audio
                    .as_ref()
                    .map(|a| {
                        let kbps = a.stats.lock().unwrap().meter.rates().kbps;
                        format!(" · audio {kbps:.0} kbps")
                    })
                    .unwrap_or_default();
                format!(
                    "{size}{:.0} fps · {:.0} kbps · encode {:.1} ms{codec}{audio}",
                    rates.fps, rates.kbps, rates.avg_ms
                )
            });
            // Listing the apps keeps the audio-apps thread refreshing, so only a monitor's
            // sound asks for it.
            let (audio, monitor_audio) = match &broadcast.audio {
                Some(audio) if matches!(audio.source, AudioSource::System { .. }) => {
                    let except =
                        mute_apps::except_text(&self.ctrl.audio_apps.apps(), &self.muted_apps);
                    (
                        Some(format!("With audio: all sound on this computer, {except}")),
                        true,
                    )
                }
                Some(audio) => (
                    Some(format!("With audio: {}", describe_audio(&audio.source))),
                    false,
                ),
                None => (None, false),
            };
            (
                broadcast.source_name.clone(),
                stats_line,
                audio,
                monitor_audio,
                broadcast.audio_note.clone(),
            )
        }) else {
            return;
        };
        let (name, stats_line, audio, monitor_audio, note) = signal;
        if column_key(ui, &station, "Stop broadcasting").clicked() {
            self.ctrl.stop_broadcast(StopReason::Stopped);
            self.viewer_count = 0;
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new(format!("Sharing {}", truncate(&name, 32)))
                .color(station.body)
                .size(16.0),
        );
        ui.label(
            RichText::new(format!(
                "{viewers} viewer{}",
                if viewers == 1 { "" } else { "s" }
            ))
            .color(station.muted),
        );
        if let Some(line) = stats_line {
            ui.label(
                RichText::new(line)
                    .color(station.muted)
                    .font(FontId::new(13.0, theme::condensed())),
            );
        } else if viewers == 0 {
            ui.label(RichText::new("Capture paused until someone watches").color(station.muted));
        }
        if let Some(audio) = audio {
            ui.label(RichText::new(audio).color(station.muted));
        }
        if monitor_audio {
            // A muted app drops out of the shared sound within a second, without a restart.
            mute_apps::ui(
                ui,
                &self.ctrl.audio_apps,
                &self.muted_apps,
                &mut self.settings,
                &self.dir,
                true,
            );
        }
        if let Some(note) = note {
            ui.colored_label(ui.visuals().warn_fg_color, note);
        }
    }

    fn open_picker(&mut self) {
        self.refresh_sources();
        self.thumbs.ensure(&self.sources);
        self.picker = true;
    }

    fn close_picker(&mut self) {
        self.picker = false;
    }

    fn live_bar(&mut self, ui: &mut egui::Ui) {
        let Some((source_name, port)) = self
            .ctrl
            .broadcast
            .as_ref()
            .map(|broadcast| (broadcast.source_name.clone(), broadcast.port))
        else {
            return;
        };
        let station = station_of(ui);
        let source = truncate(&source_name, 36);
        let quality = preset_short(self.preset);
        let fingerprint = self.ctrl.fingerprint().to_hex();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Source").color(station.muted));
            ui.label(RichText::new(source).color(station.body));
            ui.add_space(10.0);
            ui.label(RichText::new("Quality").color(station.muted));
            ui.label(RichText::new(quality).color(station.body));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let copy = station_key(&station, "Copy connect string", station.dark, 48.0, 18.0);
                egui::containers::menu::MenuButton::from_button(copy).ui(ui, |ui| {
                    ui.label(
                        "For viewers who can't see you in the list. \
                         Pick the network they share with you:",
                    );
                    let adapters = local_ipv4s();
                    if adapters.is_empty() {
                        ui.label("No network adapters found");
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
        });
    }

    /// Share audio sits above Quality in the source modal, which is closed while the line is open,
    /// so the broadcast keeps the choice it started with.
    fn share_audio_toggle(&mut self, ui: &mut egui::Ui) {
        let audio = self.sources.get(self.selected).and_then(audio_source);
        let supported = audio.as_ref().is_some_and(peeroxide_audio::is_supported);
        let station = station_of(ui);
        let mut share = self.settings.share_audio;
        let mut response = ui.add_enabled(
            supported,
            egui::Checkbox::new(
                &mut share,
                RichText::new("Share audio").size(15.0).color(station.body),
            ),
        );
        if !supported {
            response = response.on_disabled_hover_text(if audio.is_none() {
                "Can't tell which app plays this window's sound"
            } else {
                "Audio sharing isn't available on this system yet"
            });
        } else if let Some(audio) = &audio {
            let scope = describe_audio(audio);
            let mut chars = scope.chars();
            let scope: String = chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default();
            response = response.on_hover_text(format!("{scope}. Your microphone is never shared."));
        }
        if response.changed() {
            self.settings.share_audio = share;
            self.save_settings();
        }
    }

    fn peer_list_ui(&mut self, ui: &mut egui::Ui) {
        let station = station_of(ui);
        ui.label(
            RichText::new("Broadcasting on this network")
                .color(station.body)
                .font(FontId::new(15.0, theme::condensed())),
        );
        if self.peers.is_empty() {
            let text = self
                .ctrl
                .discovery_error
                .as_deref()
                .unwrap_or("No one is broadcasting yet");
            ui.label(RichText::new(text).color(station.muted));
            return;
        }
        let watched = self.active_peer();
        let mut picked = None;
        for peer in &self.peers {
            let short = Fingerprint::from_hex(&peer.fingerprint)
                .map(|f| f.short())
                .unwrap_or_default();
            let same_name = self.peers.iter().filter(|p| p.name == peer.name).count() > 1;
            let selected = watched.as_deref() == Some(peer.fingerprint.as_str());
            let saved = self.contacts.get(&peer.fingerprint).is_some();
            let changed_id = self.contacts.name_conflict(&peer.name, &peer.fingerprint);
            let addrs: Vec<String> = peer.addrs.iter().map(ToString::to_string).collect();
            let mut hover = format!(
                "ID {short}\nFingerprint {}\n{}",
                peer.fingerprint,
                addrs.join("\n")
            );
            if let Some(old) = &changed_id {
                let old_id = Fingerprint::from_hex(&old.fingerprint)
                    .map(|f| f.short())
                    .unwrap_or_default();
                hover.push_str(&format!(
                    "\nNot the {} you saved: their ID changed (was {old_id}). This happens after a \
                     reinstall, but it can also be someone impersonating them. Confirm the ID with \
                     the person.",
                    old.name
                ));
            } else if same_name && !saved {
                hover.push_str(
                    "\nAnother broadcaster uses the same name. Check the ID with the person you \
                     expect before trusting what you see.",
                );
            }
            let mark = if changed_id.is_some() {
                PlateMark::ChangedId
            } else if same_name && !saved {
                PlateMark::SameName
            } else {
                PlateMark::Live
            };
            let response = plate(ui, &station, &peer.name, &short, mark, selected);
            if response.on_hover_text(hover).clicked() && !selected {
                picked = PeerTarget::from_peer(peer);
            }
        }
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
        let station = station_of(ui);
        ui.label(
            RichText::new("Saved")
                .color(station.muted)
                .font(FontId::new(13.0, theme::condensed())),
        )
        .on_hover_text("People you watched before. Click to connect at their last address.");
        let watched = self.active_peer();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let (mut picked, mut removed) = (None, None);
        for c in &offline {
            let Some(fingerprint) = Fingerprint::from_hex(&c.fingerprint) else {
                continue;
            };
            let selected = watched.as_deref() == Some(c.fingerprint.as_str());
            let addrs: Vec<String> = c.addrs.iter().map(ToString::to_string).collect();
            let short = fingerprint.short();
            let mut forget = false;
            let mut open = false;
            ui.horizontal(|ui| {
                // Room for Forget after the plate.
                let room = (ui.available_width() - 64.0).max(0.0);
                let row = ui
                    .scope(|ui| {
                        ui.set_max_width(room);
                        plate(ui, &station, &c.name, &short, PlateMark::Saved, selected)
                    })
                    .inner
                    .on_hover_text(format!(
                        "ID {short}\nLast watched {}\n{}",
                        ago(now, c.last_seen),
                        addrs.join("\n")
                    ));
                open = row.clicked() && !selected;
                if ui.small_button("Forget").clicked() {
                    forget = true;
                }
            });
            if forget {
                removed = Some(c.fingerprint.clone());
            }
            if open {
                picked = Some(PeerTarget {
                    fingerprint,
                    name: c.name.clone(),
                    addrs: c.addrs.clone(),
                });
            }
        }
        if let Some(fp) = removed {
            self.contacts.remove(&fp);
            if let Err(e) = self.contacts.save(&self.dir) {
                tracing::warn!("could not save contacts: {e:#}");
            }
            self.sync_lookup_contacts();
        }
        if let Some(target) = picked {
            self.watch(target);
        }
    }

    fn watch_status(&mut self, ui: &mut egui::Ui) {
        let station = station_of(ui);
        match self.viewer.clone() {
            ViewerState::Idle => {}
            ViewerState::Connecting { peer } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Connecting to {}…", peer.name));
                });
                if ui.button("Cancel").clicked() {
                    self.apply(ViewerInput::StopWatching);
                }
            }
            ViewerState::Streaming { .. } => self.volume_ui(ui),
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
        egui::CollapsingHeader::new(RichText::new("Connect manually").color(station.muted)).show(
            ui,
            |ui| {
                ui.label(
                    RichText::new("IP:PORT#FINGERPRINT, for networks where discovery is blocked")
                        .color(station.muted),
                );
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.connect_input)
                        .desired_width(ui.available_width())
                        .hint_text("192.168.0.10:50123#3f9a…"),
                );
                let submitted =
                    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Watch").clicked() || submitted {
                    match parse_connect(&self.connect_input) {
                        Ok(target) => self.watch(target),
                        Err(e) => self.watch_note = Some(e.to_string()),
                    }
                }
            },
        );
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

    /// The watched stream alone, with a control bar that hides (with the cursor) when the
    /// pointer rests.
    fn fullscreen_ui(&mut self, ui: &mut egui::Ui) {
        let ViewerState::Streaming {
            broadcaster_name, ..
        } = self.viewer.clone()
        else {
            return;
        };
        let since_entered = self
            .fullscreen_since
            .map_or(f32::INFINITY, |t| t.elapsed().as_secs_f32());
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ui, |ui| {
                let video = self.video.lock().unwrap().ui(ui, &self.ctrl.video, "");
                if video.double_clicked() {
                    self.fullscreen_toggle = true;
                }
                let bar = egui::Rect::from_min_max(
                    egui::pos2(
                        video.rect.left(),
                        video.rect.bottom() - FULLSCREEN_BAR_HEIGHT,
                    ),
                    video.rect.right_bottom(),
                );
                let (since_moved, on_bar) = ui.input(|i| {
                    let on_bar = i.pointer.hover_pos().is_some_and(|p| bar.contains(p));
                    (i.pointer.time_since_last_movement(), on_bar)
                });
                if fullscreen::controls_visible(since_moved, since_entered, on_bar) {
                    self.fullscreen_bar(ui, bar, &broadcaster_name);
                } else {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::None);
                }
            });
    }

    fn fullscreen_bar(&mut self, ui: &mut egui::Ui, bar: egui::Rect, broadcaster_name: &str) {
        ui.painter()
            .rect_filled(bar, 0.0, Color32::from_black_alpha(190));
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar.shrink2(egui::vec2(16.0, 8.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        // Dark whatever the theme: the bar sits on the video.
        ui.style_mut().visuals = egui::Visuals::dark();
        ui.label(RichText::new(format!("Watching {broadcaster_name}")).strong());
        ui.add_space(16.0);
        self.volume_ui(&mut ui);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("× Exit fullscreen (Esc)").clicked() {
                self.fullscreen_exit = true;
            }
            mini_player_button(ui);
        });
    }

    /// Keeps the mini player's window while a stream is showing, visible while the main window
    /// is minimized; carries out what was clicked in it.
    fn update_mini_player(&mut self, ctx: &egui::Context, minimized: bool) {
        for action in self.mini.take_actions() {
            match action {
                Action::Stop => self.apply(ViewerInput::StopWatching),
                Action::Muted(muted) => {
                    self.settings.muted = muted;
                    self.save_settings();
                }
            }
        }
        let ViewerState::Streaming {
            broadcaster_name, ..
        } = &self.viewer
        else {
            self.drop_mini_player(ctx, minimized);
            return;
        };
        self.mini.set_stream(broadcaster_name, self.stream_audio);
        let rect = match self.mini_rect {
            Some(rect) => rect,
            None => {
                let monitor = ctx
                    .input(|i| i.viewport().monitor_size)
                    .unwrap_or(egui::vec2(1920.0, 1080.0));
                let saved = self.settings.mini_player.map(|[x, y, w, h]| {
                    egui::Rect::from_min_size(
                        egui::pos2(x as f32, y as f32),
                        egui::vec2(w as f32, h as f32),
                    )
                });
                *self
                    .mini_rect
                    .insert(mini_player::placement(monitor, saved))
            }
        };
        let visible = mini_player::wanted(true, minimized);
        self.set_mini_visible(visible);
        mini_player::show(ctx, &self.mini, rect, visible);
    }

    fn set_mini_visible(&mut self, visible: bool) {
        if visible == self.mini_visible {
            return;
        }
        self.mini_visible = visible;
        self.ctrl.mini_open.store(visible, Ordering::Relaxed);
        if visible {
            mini_player::opened(&self.mini);
            tracing::info!("mini player shown");
        } else {
            self.remember_mini_player();
            tracing::info!("mini player hidden");
        }
    }

    fn remember_mini_player(&mut self) {
        if let Some(r) = self.mini.last_rect() {
            let whole = |v: f32| v.round() as i32;
            let rect = [
                whole(r.min.x),
                whole(r.min.y),
                whole(r.width()),
                whole(r.height()),
            ];
            if self.settings.mini_player != Some(rect) {
                self.settings.mini_player = Some(rect);
                self.save_settings();
            }
        }
    }

    /// The stream is over: the mini player's window goes (it's no longer registered).
    fn drop_mini_player(&mut self, ctx: &egui::Context, minimized: bool) {
        if self.mini_rect.take().is_none() {
            return;
        }
        let was_visible = self.mini_visible;
        self.set_mini_visible(false);
        // The stream ended while Peeroxide is minimized: flash it in the taskbar.
        if was_visible && minimized && matches!(self.viewer, ViewerState::Disconnected { .. }) {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        }
    }

    /// Applies this frame's fullscreen requests (keys, buttons, double-clicks) to the window.
    fn update_fullscreen(&mut self, ctx: &egui::Context, fullscreen: bool) {
        let streaming = matches!(self.viewer, ViewerState::Streaming { .. });
        let (f11, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::F11),
                i.key_pressed(egui::Key::Escape),
            )
        });
        let toggle = std::mem::take(&mut self.fullscreen_toggle) || (streaming && f11);
        let exit = std::mem::take(&mut self.fullscreen_exit) || (fullscreen && escape);
        let want = fullscreen::wanted(fullscreen, streaming, toggle, exit);
        if want != fullscreen {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(want));
            self.fullscreen_since = want.then(Instant::now);
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
        let codec = s
            .codec
            .map(|c| format!("{}  ", c.name()))
            .unwrap_or_default();
        format!(
            "{codec}{:.1} fps  {:.0} kbps  decode {:.1} ms  dropped {}{latency}{audio}",
            r.fps, r.kbps, r.avg_ms, s.dropped
        )
    }
    fn well(&mut self, ui: &mut egui::Ui) {
        match self.viewer.clone() {
            ViewerState::Streaming { .. } => {
                let overlay = if self.settings.show_stats {
                    self.watch_overlay()
                } else {
                    String::new()
                };
                let bar_h = 40.0;
                let gap = 10.0;
                let video_h = (ui.available_height() - bar_h - gap).max(1.0);
                let played = ui
                    .allocate_ui_with_layout(
                        vec2(ui.available_width(), video_h),
                        Layout::top_down(Align::Min),
                        |ui| {
                            self.video
                                .lock()
                                .unwrap()
                                .ui(ui, &self.ctrl.video, &overlay)
                        },
                    )
                    .inner;
                if played.double_clicked() {
                    self.fullscreen_toggle = true;
                }
                ui.add_space(gap);
                self.watch_actions(ui);
            }
            ViewerState::Connecting { peer } => {
                VideoView::placeholder(ui, &format!("Connecting to {}…", peer.name));
            }
            ViewerState::Disconnected { peer, reason } => {
                VideoView::placeholder(ui, &format!("{}: {reason}", peer.name));
            }
            ViewerState::Idle => self.own_well(ui),
        }
    }

    /// The three picture controls sit under the video, centered.
    /// The bottom bar belongs to the broadcast, and it is absent when this window is only watching.
    fn watch_actions(&mut self, ui: &mut egui::Ui) {
        let station = station_of(ui);
        let id = ui.id().with("watch-actions-width");
        ui.horizontal(|ui| {
            let known: Option<f32> = ui.data(|data| data.get_temp(id));
            if let Some(width) = known {
                ui.add_space((ui.available_width() - width).max(0.0) / 2.0);
            }
            let width = ui
                .scope(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.horizontal(|ui| {
                        if icon_key(
                            ui,
                            &station,
                            "Stop watching",
                            egui::include_image!("../../../assets/icons/monitor-off.svg"),
                        )
                        .clicked()
                        {
                            self.apply(ViewerInput::StopWatching);
                        }
                        if icon_key(
                            ui,
                            &station,
                            "Fullscreen. Or press F11, or double-click the video. Esc leaves.",
                            egui::include_image!("../../../assets/icons/monitor-screenshot.svg"),
                        )
                        .clicked()
                        {
                            self.fullscreen_toggle = true;
                        }
                        if icon_key(
                            ui,
                            &station,
                            "Mini player. Keep watching in a small window on top. Minimizing does the same.",
                            egui::include_image!("../../../assets/icons/pip.svg"),
                        )
                        .clicked()
                        {
                            ui.ctx()
                                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                    });
                })
                .response
                .rect
                .width();
            ui.data_mut(|data| data.insert_temp(id, width));
            if known.is_none_or(|old| (old - width).abs() > 0.5) {
                ui.ctx().request_repaint();
            }
        });
    }

    /// The station's own screen: empty before Start, a copy of a captured frame while someone
    /// is watching, and the pause note when capture is waiting for a viewer.
    fn own_well(&mut self, ui: &mut egui::Ui) {
        let station = station_of(ui);
        let viewers = self.viewer_count;
        let title = self
            .ctrl
            .broadcast
            .as_ref()
            .map(|broadcast| broadcast.source_name.clone());
        let frame = if viewers > 0 {
            self.ctrl
                .broadcast
                .as_ref()
                .and_then(|broadcast| broadcast.encoder.take_preview())
        } else {
            None
        };
        if viewers == 0 {
            // Drop the last copy so the next session cannot flash it.
            self.preview = None;
        } else if let Some(frame) = frame {
            let previous = self.preview.take();
            self.preview = Some(preview_texture(ui.ctx(), frame, previous));
        }
        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, Sense::hover());
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 2.0, station.well);
        let Some(title) = title else {
            return;
        };
        if viewers == 0 {
            painter.text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                "Capture paused until someone watches",
                FontId::proportional(15.0),
                station.stamp,
            );
            return;
        }
        let caption = "Preview · copy of a captured frame";
        painter.text(
            area.left_top() + vec2(12.0, 10.0),
            egui::Align2::LEFT_TOP,
            caption,
            FontId::new(12.0, theme::condensed()),
            station.stamp,
        );
        let picture = Rect::from_min_max(
            area.left_top() + vec2(12.0, 32.0),
            area.right_bottom() - vec2(12.0, 12.0),
        );
        if picture.width() <= 0.0 || picture.height() <= 0.0 {
            return;
        }
        let bar = Rect::from_min_size(picture.left_top(), vec2(picture.width(), 28.0));
        painter.rect_filled(bar, 0.0, station.title_bar);
        painter.text(
            bar.left_center() + vec2(10.0, 0.0),
            egui::Align2::LEFT_CENTER,
            truncate(&title, 48),
            FontId::proportional(13.0),
            station.title_ink,
        );
        let screen = Rect::from_min_max(bar.left_bottom(), picture.right_bottom());
        if let Some(texture) = &self.preview {
            let size = texture.size_vec2();
            let scale = (screen.width() / size.x).min(screen.height() / size.y);
            let rect = Rect::from_center_size(screen.center(), size * scale);
            painter.image(
                texture.id(),
                rect,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }

    fn source_modal(&mut self, ctx: &egui::Context) {
        // Apply stills that arrived before this paint, so the cards don't open empty.
        self.thumbs.poll();
        let station = theme::station(ctx.global_style().visuals.dark_mode);
        let mut close = false;
        let mut refresh = false;
        let mut begin = false;
        // Everything but the cards takes about 300 px. The cards give way on a short or narrow
        // window, so Start broadcasting stays on screen.
        let screen = ctx.content_rect();
        let cards_height = (screen.height() - 300.0).clamp(120.0, 420.0);
        let min_width = (screen.width() - 96.0).clamp(400.0, 820.0);
        let response = egui::Modal::new(egui::Id::new("source-picker"))
            .frame(
                egui::Frame::NONE
                    .fill(station.key)
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.set_min_width(min_width);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Source")
                            .color(station.body)
                            .font(FontId::new(22.0, theme::condensed())),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("×").size(22.0).color(station.muted),
                                )
                                .frame(false),
                            )
                            .on_hover_text("Close")
                            .clicked()
                        {
                            close = true;
                        }
                        if refresh_button(ui, station.muted)
                            .on_hover_text("Look for windows again")
                            .clicked()
                        {
                            refresh = true;
                        }
                    });
                });
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_height(cards_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = vec2(12.0, 14.0);
                        let width = ((ui.available_width() - 24.0) / 3.0).max(160.0);
                        ui.horizontal_wrapped(|ui| {
                            let count = self.sources.len();
                            for index in 0..count {
                                self.source_card(ui, &station, index, width);
                            }
                        });
                    });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    self.share_audio_toggle(ui);
                    ui.add_space(16.0);
                    let monitor = self
                        .sources
                        .get(self.selected)
                        .and_then(audio_source)
                        .is_some_and(|audio| matches!(audio, AudioSource::System { .. }));
                    mute_apps::ui(
                        ui,
                        &self.ctrl.audio_apps,
                        &self.muted_apps,
                        &mut self.settings,
                        &self.dir,
                        monitor,
                    );
                });
                ui.add_space(10.0);
                ui.label(
                    RichText::new("Quality")
                        .color(station.muted)
                        .font(FontId::new(13.0, theme::condensed())),
                );
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    for preset in Preset::ALL {
                        self.preset_chip(ui, &station, preset);
                    }
                });
                ui.add_space(16.0);
                if column_key(ui, &station, "Start broadcasting").clicked() {
                    begin = true;
                }
            });
        if begin {
            self.close_picker();
            self.start_broadcast();
        } else if close || response.should_close() {
            self.close_picker();
        } else if refresh {
            self.refresh_sources();
            self.thumbs.ensure(&self.sources);
        }
    }

    fn open_settings(&mut self) {
        self.port_input = self
            .settings
            .broadcast_port
            .map(|port| port.to_string())
            .unwrap_or_default();
        self.settings_open = true;
    }

    /// The theme, and what settings.toml holds that has no control elsewhere: the debug
    /// figures, the broadcast port, looking on Tailscale and where the mini player opens.
    fn settings_modal(&mut self, ctx: &egui::Context) {
        let station = theme::station(ctx.global_style().visuals.dark_mode);
        let width = (ctx.content_rect().width() - 96.0).clamp(300.0, 528.0);
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("settings"))
            .frame(
                egui::Frame::NONE
                    .fill(station.key)
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Settings")
                            .color(station.body)
                            .font(FontId::new(22.0, theme::condensed())),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("×").size(22.0).color(station.muted),
                                )
                                .frame(false),
                            )
                            .on_hover_text("Close")
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
                ui.add_space(8.0);
                section_label(ui, &station, "Theme");
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(12.0, 12.0);
                    for palette in Palette::ALL {
                        self.palette_card(ui, &station, palette);
                    }
                });
                ui.add_space(12.0);
                section_label(ui, &station, "Appearance");
                ui.horizontal(|ui| {
                    for choice in ThemeChoice::ALL {
                        let selected = self.settings.theme == choice;
                        let hover = match choice {
                            ThemeChoice::System => "Follows Windows",
                            ThemeChoice::Dark => "Always the dark side",
                            ThemeChoice::Light => "Always the light side",
                        };
                        if station_button(ui, &station, choice.label(), selected, 30.0, 14.0)
                            .on_hover_text(hover)
                            .clicked()
                            && !selected
                        {
                            self.settings.theme = choice;
                            self.save_settings();
                        }
                    }
                });
                ui.add_space(12.0);
                section_label(ui, &station, "Stream");
                let mut show = self.settings.show_stats;
                if ui
                    .checkbox(
                        &mut show,
                        RichText::new("Show debug figures").color(station.body),
                    )
                    .on_hover_text(
                        "Codec, frame rate, bitrate and timings over a watched stream and \
                         under the live line",
                    )
                    .changed()
                {
                    self.settings.show_stats = show;
                    self.save_settings();
                }
                ui.add_space(12.0);
                section_label(ui, &station, "Network");
                let live = self.ctrl.broadcast.is_some();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Broadcast port").color(station.body));
                    let field = ui
                        .add_enabled(
                            !live,
                            egui::TextEdit::singleline(&mut self.port_input)
                                .desired_width(72.0)
                                .hint_text("any"),
                        )
                        .on_hover_text(
                            "Kept for every broadcast, so connect strings and saved contacts \
                             stay valid. Empty picks a free port.",
                        )
                        .on_disabled_hover_text("Stop broadcasting to change the port");
                    match parse_port(&self.port_input) {
                        Some(port) if field.changed() => {
                            self.settings.broadcast_port = port;
                            self.save_settings();
                        }
                        Some(_) => {}
                        None => {
                            ui.colored_label(ui.visuals().warn_fg_color, "Use 1 to 65535");
                        }
                    }
                });
                let mut tailscale = !self.settings.skip_tailscale;
                if ui
                    .checkbox(
                        &mut tailscale,
                        RichText::new("Find broadcasters on Tailscale").color(station.body),
                    )
                    .on_hover_text(format!(
                        "Multicast doesn't cross a tailnet, so the network list can't see who \
                         broadcasts there. Peeroxide asks the Tailscale app on this computer for \
                         the tailnet's devices (also with Headscale) and asks each one, on UDP \
                         port {LOOKUP_PORT}. Saved people are asked at their saved address \
                         either way."
                    ))
                    .changed()
                {
                    self.settings.skip_tailscale = !tailscale;
                    self.save_settings();
                    self.ctrl.set_tailscale(tailscale);
                }
                ui.add_space(12.0);
                section_label(ui, &station, "Mini player");
                let placed = self.settings.mini_player.is_some();
                if ui
                    .add_enabled(placed, egui::Button::new("Open it in the corner again"))
                    .on_hover_text("Forgets where it was left; it opens in the corner next time")
                    .on_disabled_hover_text("It already opens in the corner")
                    .clicked()
                {
                    self.settings.mini_player = None;
                    self.save_settings();
                }
                ui.add_space(16.0);
                ui.label(
                    RichText::new(format!(
                        "Saved in {}",
                        self.dir.join("settings.toml").display()
                    ))
                    .color(station.muted)
                    .size(12.0),
                );
            });
        if close || response.should_close() {
            self.settings_open = false;
        }
    }

    /// A card painted in the palette's own colors, on the side in use now.
    fn palette_card(&mut self, ui: &mut egui::Ui, station: &Station, palette: Palette) {
        let own = Station::of(palette, station.dark);
        let selected = self.settings.palette == palette;
        let (rect, response) = ui.allocate_exact_size(vec2(160.0, 72.0), Sense::click());
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, own.housing);
        painter.text(
            rect.left_top() + vec2(10.0, 8.0),
            egui::Align2::LEFT_TOP,
            palette.label(),
            FontId::proportional(14.0),
            own.body,
        );
        painter.text(
            rect.left_top() + vec2(10.0, 27.0),
            egui::Align2::LEFT_TOP,
            palette.sides(),
            FontId::new(12.0, theme::condensed()),
            own.muted,
        );
        // A nameplate with its lamp, and the strong key, as the column shows them.
        let row = rect.bottom() - 15.0;
        let plate =
            Rect::from_min_size(egui::pos2(rect.left() + 10.0, row - 6.0), vec2(40.0, 12.0));
        painter.rect_filled(plate, 2.0, own.paper);
        painter.circle_filled(plate.left_center() + vec2(7.0, 0.0), 2.5, own.lamp);
        let key = Rect::from_min_size(egui::pos2(plate.right() + 8.0, row - 6.0), vec2(40.0, 12.0));
        painter.rect_filled(key, 2.0, own.strong_fill);
        let stroke = if selected {
            station.selected_stroke()
        } else if response.hovered() {
            Stroke::new(1.0, station.body)
        } else {
            Stroke::new(1.0, own.line)
        };
        painter.rect_stroke(rect, 4.0, stroke, egui::StrokeKind::Inside);
        if response.clicked() && !selected {
            self.settings.palette = palette;
            self.save_settings();
        }
    }

    fn source_card(&mut self, ui: &mut egui::Ui, station: &Station, index: usize, width: f32) {
        let Some(source) = self.sources.get(index).cloned() else {
            return;
        };
        let (title, detail) = source_caption(&source);
        let selected = index == self.selected;
        let id = source.id();
        let (rect, response) = ui.allocate_exact_size(vec2(width, 172.0), Sense::click());
        let painter = ui.painter();
        if response.hovered() {
            painter.rect_filled(rect, 3.0, station.key_hover);
        }
        let image = Rect::from_min_size(rect.min, vec2(rect.width(), 112.0));
        painter.rect_filled(image, 2.0, station.well);
        if let Some(texture) = self.thumbs.texture(ui.ctx(), &id) {
            let size = texture.size_vec2();
            let scale = (image.width() / size.x).min(image.height() / size.y);
            let fitted = Rect::from_center_size(image.center(), size * scale);
            painter.image(
                texture.id(),
                fitted,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let stroke = if selected {
            station.selected_stroke()
        } else if response.hovered() {
            Stroke::new(1.0, station.line)
        } else {
            Stroke::NONE
        };
        if stroke != Stroke::NONE {
            painter.rect_stroke(rect, 3.0, stroke, egui::StrokeKind::Inside);
        }
        painter.text(
            image.left_bottom() + vec2(2.0, 6.0),
            egui::Align2::LEFT_TOP,
            truncate(&title, 28),
            FontId::proportional(14.0),
            station.body,
        );
        if !detail.is_empty() {
            painter.text(
                image.left_bottom() + vec2(2.0, 24.0),
                egui::Align2::LEFT_TOP,
                truncate(&detail, 32),
                FontId::new(12.0, theme::condensed()),
                station.muted,
            );
        }
        if response.clicked() {
            self.selected = index;
        }
    }

    fn preset_chip(&mut self, ui: &mut egui::Ui, station: &Station, preset: Preset) {
        let selected = self.preset == preset;
        let (label, note) = preset_lines(preset);
        let text = match note {
            Some(note) => format!("{label}\n{note}"),
            None => label.to_string(),
        };
        let response = station_button(ui, station, &text, selected, 36.0, 14.0);
        let mut hint = format!(
            "Up to {}×{}, at most {} Mbps of upload.",
            preset.max_width,
            preset.max_height,
            preset.bitrate_bps / 1_000_000,
        );
        if preset == Preset::INTERNET {
            hint.push_str(" Recommended for Radmin VPN, Hamachi and other internet links.");
        }
        if preset.fps >= 60 && self.h265_encoder.is_none() {
            hint.push_str(
                " No H.265 encoder was found on the graphics card, so 60 fps uses H.264 on the \
                 CPU and may not keep up.",
            );
        }
        if response.on_hover_text(hint).clicked() && !selected {
            self.preset = preset;
            self.settings.preset = Some(preset.id.into());
            self.save_settings();
        }
    }
}

impl eframe::App for App {
    /// Also runs while the main window is minimized, when `ui` doesn't: that's when the mini
    /// player has to appear.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.thumbs.poll();
        let minimized = ctx.input(|i| i.viewport().minimized) == Some(true);
        if !minimized {
            return;
        }
        let streaming = matches!(self.viewer, ViewerState::Streaming { .. });
        if streaming && self.mini_rect.is_some() && !self.mini_visible {
            mini_player::reveal(ctx);
            self.set_mini_visible(true);
        }
        // Keep checking, even while a still screen sends no frames.
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if std::mem::take(&mut self.autostart) {
            self.start_broadcast();
        }
        self.handle_events();
        if self.ctrl.broadcast.is_some() || self.ctrl.watching.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }

        let ctx = ui.ctx().clone();
        let minimized = ctx.input(|i| i.viewport().minimized) == Some(true);
        self.update_mini_player(&ctx, minimized);
        if minimized {
            // Nobody sees the main window; the mini player (if any) shows the stream.
            return;
        }
        let fullscreen = ctx.input(|i| i.viewport().fullscreen) == Some(true);
        if fullscreen && matches!(self.viewer, ViewerState::Streaming { .. }) {
            self.fullscreen_ui(ui);
            self.update_fullscreen(&ctx, fullscreen);
            return;
        }

        theme::apply(ui.ctx(), self.settings.palette, self.settings.theme);
        let live = self.ctrl.broadcast.is_some();
        // Source and quality stay as they were when the line opened.
        if live && self.picker {
            self.close_picker();
        }
        if live && self.viewer_count > 0 && matches!(self.viewer, ViewerState::Idle) {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        let frame = housing_frame(ui);

        if live {
            egui::Panel::bottom("live-bar")
                .frame(frame)
                .show(ui, |ui| self.live_bar(ui));
        }

        egui::Panel::left("controls")
            .resizable(false)
            .exact_size(COLUMN_WIDTH)
            .frame(frame)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.station_column(ui));
            });

        egui::CentralPanel::default()
            .frame(frame)
            .show(ui, |ui| self.well(ui));

        if self.picker {
            self.source_modal(ui.ctx());
        }
        if self.settings_open {
            self.settings_modal(ui.ctx());
        }
        self.update_fullscreen(&ctx, fullscreen);
    }
}

/// "Mini player": minimizing opens the mini player.
fn mini_player_button(ui: &mut egui::Ui) {
    if ui
        .button("🗗 Mini player")
        .on_hover_text("Keep watching in a small window on top. Minimizing does the same.")
        .clicked()
    {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }
}

/// The error's own text (without its cause) is short enough to show as a note.
pub fn parse_connect(s: &str) -> anyhow::Result<PeerTarget> {
    let (addr, fp) = s
        .trim()
        .split_once('#')
        .context("Expected IP:PORT#FINGERPRINT")?;
    let addr: SocketAddr = addr.trim().parse().context("Invalid IP:PORT")?;
    let fingerprint = Fingerprint::from_hex(fp.trim())
        .context("Invalid fingerprint (expected 64 hex characters)")?;
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

fn station_of(ui: &egui::Ui) -> Station {
    theme::station(ui.visuals().dark_mode)
}

fn section_label(ui: &mut egui::Ui, station: &Station, text: &str) {
    ui.label(
        RichText::new(text)
            .color(station.muted)
            .font(FontId::new(13.0, theme::condensed())),
    );
    ui.add_space(4.0);
}

/// The settings window's port field: empty picks a free port, otherwise 1 to 65535.
/// `None` when the text is not a port.
fn parse_port(text: &str) -> Option<Option<u16>> {
    let text = text.trim();
    if text.is_empty() {
        return Some(None);
    }
    text.parse::<u16>().ok().filter(|port| *port != 0).map(Some)
}

fn housing_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::NONE
        .fill(ui.visuals().panel_fill)
        .inner_margin(egui::Margin::same(12))
}

fn key_colors(station: &Station, strong: bool) -> (Color32, Color32) {
    if strong {
        (station.strong_fill, station.strong_ink)
    } else {
        (station.key, station.body)
    }
}

fn station_key<'a>(
    station: &Station,
    text: &'a str,
    strong: bool,
    min_height: f32,
    font_size: f32,
) -> egui::Button<'a> {
    let (fill, ink) = key_colors(station, strong);
    let stroke = if strong {
        Stroke::NONE
    } else {
        Stroke::new(station.line_w, station.line)
    };
    egui::Button::new(
        RichText::new(text)
            .color(ink)
            .font(FontId::new(font_size, theme::condensed())),
    )
    .fill(fill)
    .stroke(stroke)
    .corner_radius(egui::CornerRadius::same(3))
    .min_size(vec2(0.0, min_height))
}

fn station_button(
    ui: &mut egui::Ui,
    station: &Station,
    text: &str,
    strong: bool,
    min_height: f32,
    font_size: f32,
) -> egui::Response {
    ui.add(station_key(station, text, strong, min_height, font_size))
}

/// The settings key: a gear.
fn gear_button(ui: &mut egui::Ui, station: &Station) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
    let behind = if response.hovered() {
        station.key_hover
    } else {
        station.housing
    };
    if response.hovered() {
        ui.painter().circle_filled(rect.center(), 12.0, behind);
    }
    paint_gear(ui.painter(), rect.center(), station.body, behind);
    response
}

fn paint_gear(painter: &egui::Painter, center: egui::Pos2, color: Color32, behind: Color32) {
    let tooth = Stroke::new(3.0, color);
    for i in 0..8 {
        let angle = i as f32 * std::f32::consts::TAU / 8.0;
        let dir = vec2(angle.cos(), angle.sin());
        painter.line_segment([center + dir * 5.0, center + dir * 8.5], tooth);
    }
    painter.circle_filled(center, 6.0, color);
    painter.circle_filled(center, 2.5, behind);
}

fn icon_key(
    ui: &mut egui::Ui,
    station: &Station,
    hover: &str,
    source: egui::ImageSource<'static>,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::click());
    let fill = if response.hovered() {
        station.key_hover
    } else {
        station.key
    };
    paint_key(ui, rect, fill, station.line, station.line_w, 3.0);
    let icon = Rect::from_center_size(rect.center(), vec2(22.0, 22.0));
    egui::Image::new(source)
        .tint(station.body)
        .paint_at(ui, icon);
    response.on_hover_text(hover)
}

/// A circular arrow, same weight as the close mark beside it.
fn refresh_button(ui: &mut egui::Ui, color: Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
    let color = if response.hovered() {
        color.linear_multiply(1.35)
    } else {
        color
    };
    let center = rect.center();
    let radius = 6.5;
    let stroke = Stroke::new(1.7, color);
    let start = -0.5_f32;
    let end = std::f32::consts::TAU - 1.05;
    let steps = 20;
    let points = (0..=steps)
        .map(|i| {
            let t = start + (end - start) * (i as f32 / steps as f32);
            center + vec2(radius * t.cos(), radius * t.sin())
        })
        .collect();
    let painter = ui.painter();
    painter.add(egui::Shape::line(points, stroke));
    let forward = vec2(-end.sin(), end.cos());
    let side = vec2(-forward.y, forward.x);
    let tip = center + vec2(radius * end.cos(), radius * end.sin()) + forward * 2.2;
    let back = tip - forward * 5.0;
    painter.add(egui::Shape::convex_polygon(
        vec![tip, back + side * 3.1, back - side * 3.1],
        color,
        Stroke::NONE,
    ));
    response
}

/// The column's strong key, as wide as its label. Start opens the picker; Stop sits in the same
/// place once live.
fn column_key(ui: &mut egui::Ui, station: &Station, label: &str) -> egui::Response {
    let font = FontId::new(15.0, theme::condensed());
    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_string(), font.clone(), station.strong_ink)
        .size()
        .x;
    let width = (text_w + 36.0).min(ui.available_width());
    let (rect, response) = ui.allocate_exact_size(vec2(width, 32.0), Sense::click());
    let fill = if response.hovered() {
        station.strong_fill.linear_multiply(0.92)
    } else {
        station.strong_fill
    };
    ui.painter().rect_filled(rect, 3.0, fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        station.strong_ink,
    );
    response
}

fn paint_key(ui: &egui::Ui, rect: Rect, fill: Color32, line: Color32, width: f32, radius: f32) {
    ui.painter().rect_filled(rect, radius, fill);
    if width > 0.0 {
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(width, line),
            egui::StrokeKind::Inside,
        );
    }
}

/// The word after the ID on a nameplate. Every mark but `Saved` is someone broadcasting now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlateMark {
    Live,
    /// A saved contact who is not broadcasting: a ring instead of a filled dot.
    Saved,
    /// Not the contact saved under this name: the ID changed.
    ChangedId,
    /// Another broadcaster uses the same name.
    SameName,
}

/// A paper nameplate. Someone on air gets a larger name and a lamp for the dot. A name too long
/// for the column ends in "…" before the ID.
fn plate(
    ui: &mut egui::Ui,
    station: &Station,
    name: &str,
    id: &str,
    mark: PlateMark,
    selected: bool,
) -> egui::Response {
    let on_air = mark != PlateMark::Saved;
    let (word, word_color) = match mark {
        PlateMark::Live => ("Live", station.lamp),
        PlateMark::Saved => ("Saved", station.ink),
        PlateMark::ChangedId => ("⚠ Check ID", ui.visuals().error_fg_color),
        PlateMark::SameName => ("⚠ Check ID", ui.visuals().warn_fg_color),
    };
    let label_font = FontId::new(12.0, theme::condensed());
    let id_galley = ui
        .painter()
        .layout_no_wrap(id.to_string(), label_font.clone(), station.ink);
    let word_galley = ui
        .painter()
        .layout_no_wrap(word.to_string(), label_font, word_color);
    let right_w = id_galley.size().x + 6.0 + word_galley.size().x;
    let chrome = 12.0 + 8.0 + 12.0 + 12.0 + right_w + 12.0;
    let mut name_job = egui::text::LayoutJob::simple_singleline(
        truncate(name, if on_air { 28 } else { 22 }),
        FontId::proportional(if on_air { 14.0 } else { 13.0 }),
        station.ink,
    );
    name_job.wrap =
        egui::text::TextWrapping::truncate_at_width((ui.available_width() - chrome).max(0.0));
    let name_galley = ui.painter().layout_job(name_job);
    let width = (chrome + name_galley.size().x).min(ui.available_width());
    let height = if on_air { 30.0 } else { 28.0 };
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let stroke = if selected { 2.0 } else { station.line_w };
    let line = if selected { station.ink } else { station.line };
    paint_key(ui, rect, station.paper, line, stroke, 2.0);
    let painter = ui.painter();
    let dot = rect.left_center() + vec2(16.0, 0.0);
    if on_air {
        painter.circle_filled(dot, 4.0, station.lamp);
    } else {
        painter.circle_stroke(dot, 3.5, Stroke::new(1.5, station.ink));
    }
    let middle = |size: egui::Vec2, x: f32| egui::pos2(x, rect.center().y - size.y / 2.0);
    let word_x = rect.right() - 10.0 - word_galley.size().x;
    let id_x = word_x - 6.0 - id_galley.size().x;
    painter.galley(
        middle(name_galley.size(), dot.x + 14.0),
        name_galley,
        station.ink,
    );
    painter.galley(middle(id_galley.size(), id_x), id_galley, station.ink);
    painter.galley(middle(word_galley.size(), word_x), word_galley, word_color);
    response
}

fn preview_texture(
    ctx: &egui::Context,
    frame: PreviewFrame,
    previous: Option<TextureHandle>,
) -> TextureHandle {
    let image = ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
    match previous {
        Some(mut texture) => {
            texture.set(image, TextureOptions::LINEAR);
            texture
        }
        None => ctx.load_texture("own-preview", image, TextureOptions::LINEAR),
    }
}

fn preset_lines(preset: Preset) -> (&'static str, Option<&'static str>) {
    if preset == Preset::INTERNET {
        ("720p · 24 fps", Some("Internet / VPN"))
    } else {
        (preset.name, None)
    }
}

fn preset_short(preset: Preset) -> String {
    match preset_lines(preset) {
        (label, Some(note)) => format!("{label} · {note}"),
        (label, None) => label.to_string(),
    }
}

/// Title and the smaller line under a source card.
fn source_caption(source: &Source) -> (String, String) {
    match source.kind {
        SourceKind::TestPattern => ("Test pattern".to_string(), "built in".to_string()),
        SourceKind::Monitor => monitor_caption(&source.name),
        SourceKind::Window => window_caption(&source.name),
    }
}

fn monitor_caption(name: &str) -> (String, String) {
    let (head, rest) = name.split_once('(').unwrap_or((name, ""));
    let title = head.split(':').next().unwrap_or(head).trim().to_string();
    let subtitle = rest
        .replace(')', "")
        .replace('x', "×")
        .trim()
        .trim_matches(',')
        .trim()
        .to_string();
    (title, subtitle)
}

fn window_caption(name: &str) -> (String, String) {
    match name.rsplit_once(" [") {
        Some((title, process)) => (title.to_string(), process.trim_end_matches(']').to_string()),
        None => (name.to_string(), String::new()),
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

    #[test]
    fn port_field_takes_empty_or_a_real_port() {
        assert_eq!(parse_port(""), Some(None));
        assert_eq!(parse_port(" 50123 "), Some(Some(50123)));
        assert_eq!(parse_port("0"), None);
        assert_eq!(parse_port("70000"), None);
        assert_eq!(parse_port("port"), None);
    }

    #[test]
    fn source_captions_split_the_list_names() {
        let (title, detail) = monitor_caption("Monitor 1: DELL (1920x1080), primary");
        assert_eq!(title, "Monitor 1");
        assert_eq!(detail, "1920×1080, primary");
        let (title, detail) = window_caption("League of Legends [League of Legends.exe]");
        assert_eq!(title, "League of Legends");
        assert_eq!(detail, "League of Legends.exe");
        assert_eq!(preset_lines(Preset::INTERNET).1, Some("Internet / VPN"));
        assert_eq!(preset_short(Preset::P1080), "1080p · 30 fps");
        assert_eq!(
            preset_short(Preset::INTERNET),
            "720p · 24 fps · Internet / VPN"
        );
    }
}
