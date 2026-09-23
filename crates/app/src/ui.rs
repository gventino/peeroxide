use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use p2pss_capture::{Source, SourceKind, list_sources};
use p2pss_codec::Preset;

use crate::Args;
use crate::decoder::{DecoderPipeline, DecoderStats, VideoSlot};
use crate::encoder::{EncoderControl, EncoderEnd, EncoderPipeline};
use crate::video::VideoView;

/// Local capture → encode → decode → display, without networking.
struct Loopback {
    encoder: EncoderPipeline,
    decoder_stats: Arc<Mutex<DecoderStats>>,
    ended: Arc<Mutex<Option<EncoderEnd>>>,
}

pub struct App {
    sources: Vec<Source>,
    selected: usize,
    preset: Preset,
    loopback: Option<Loopback>,
    slot: Arc<VideoSlot>,
    video: VideoView,
    status: Option<String>,
    autostart: bool,
}

impl App {
    pub fn new(args: Args) -> Self {
        let mut app = Self {
            sources: Vec::new(),
            selected: 0,
            preset: Preset::default(),
            loopback: None,
            slot: Arc::new(VideoSlot::default()),
            video: VideoView::default(),
            status: None,
            autostart: false,
        };
        app.refresh_sources();
        if let Some(query) = &args.broadcast {
            match find_source(&app.sources, query) {
                Some(i) => {
                    app.selected = i;
                    app.autostart = true;
                }
                None => app.status = Some(format!("No source matches \"{query}\"")),
            }
        }
        app
    }

    fn refresh_sources(&mut self) {
        let previous = self.sources.get(self.selected).cloned();
        let mut sources = match list_sources() {
            Ok(s) => s,
            Err(e) => {
                self.status = Some(format!("Could not list sources: {e}"));
                Vec::new()
            }
        };
        sources.push(Source::test_pattern());
        self.selected = previous
            .and_then(|p| sources.iter().position(|s| *s == p))
            .unwrap_or(0);
        self.sources = sources;
    }

    fn start(&mut self, ctx: &egui::Context) {
        let Some(source) = self.sources.get(self.selected).cloned() else {
            return;
        };
        let control = EncoderControl::new(true);
        let repaint = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        let need_keyframe = {
            let control = control.clone();
            Arc::new(move || control.request_keyframe())
        };
        let mut decoder = DecoderPipeline::start(self.slot.clone(), repaint, need_keyframe);
        let decoder_stats = decoder.stats.clone();
        let ended = Arc::new(Mutex::new(None));
        let encoder = EncoderPipeline::start(
            control,
            source,
            self.preset,
            move |packet| decoder.push(packet),
            {
                let ended = ended.clone();
                let ctx = ctx.clone();
                move |end| {
                    *ended.lock().unwrap() = Some(end);
                    ctx.request_repaint();
                }
            },
        );
        self.status = None;
        self.loopback = Some(Loopback {
            encoder,
            decoder_stats,
            ended,
        });
    }

    fn stop(&mut self) {
        self.loopback = None;
        self.slot.take();
        self.video.clear();
    }

    fn overlay(&self) -> String {
        let Some(lb) = &self.loopback else {
            return String::new();
        };
        let mut enc = lb.encoder.stats.lock().unwrap();
        let e = enc.meter.rates();
        let canvas = enc
            .canvas
            .map(|(w, h)| format!("{w}x{h}"))
            .unwrap_or_else(|| "-".into());
        drop(enc);
        let mut dec = lb.decoder_stats.lock().unwrap();
        let d = dec.meter.rates();
        let latency = dec
            .latency_ms
            .map(|l| format!("{l:.0} ms"))
            .unwrap_or_else(|| "-".into());
        format!(
            "{canvas}  encode {:.1} fps {:.1} ms  {:.0} kbps\n\
             decode {:.1} fps {:.1} ms  latency {latency}  dropped {}",
            e.fps, e.avg_ms, e.kbps, d.fps, d.avg_ms, dec.dropped
        )
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if std::mem::take(&mut self.autostart) {
            self.start(&ctx);
        }
        let ended = self
            .loopback
            .as_ref()
            .and_then(|lb| lb.ended.lock().unwrap().take());
        if let Some(end) = ended {
            self.stop();
            self.status = Some(match end {
                EncoderEnd::Stopped => "Stopped".into(),
                EncoderEnd::SourceClosed => "Source closed".into(),
                EncoderEnd::Failed(e) => format!("Capture failed: {e}"),
            });
        }

        egui::Panel::left("controls")
            .resizable(false)
            .exact_size(300.0)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.heading("Broadcast");
                ui.label("Local loopback preview (no network yet)");
                ui.add_space(8.0);

                let running = self.loopback.is_some();
                ui.add_enabled_ui(!running, |ui| {
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
                        .width(280.0)
                        .selected_text(truncate(&current, 40))
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
                    egui::ComboBox::from_id_salt("preset")
                        .width(280.0)
                        .selected_text(self.preset.name)
                        .show_ui(ui, |ui| {
                            for p in Preset::ALL {
                                ui.selectable_value(&mut self.preset, p, p.name);
                            }
                        });
                });
                ui.add_space(8.0);
                if running {
                    if ui.button("■ Stop").clicked() {
                        self.stop();
                    }
                } else if ui.button("▶ Start preview").clicked() {
                    self.start(&ctx);
                }
                if let Some(s) = &self.status {
                    ui.add_space(8.0);
                    ui.colored_label(ui.visuals().warn_fg_color, s);
                }
            });

        egui::CentralPanel::default().show(ui, |ui| {
            if self.loopback.is_some() {
                let overlay = self.overlay();
                self.video.ui(ui, &self.slot, &overlay);
                ctx.request_repaint_after(Duration::from_millis(500));
            } else {
                VideoView::placeholder(ui, "Pick a source and start the preview");
            }
        });
    }
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
