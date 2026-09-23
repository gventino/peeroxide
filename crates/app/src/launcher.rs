//! First screen (UC-09): checks for an update before the rest of the app starts, installs it and
//! restarts, or hands over to the main UI. Nothing on the network runs until then, so a restart
//! never interrupts a session.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, ProgressBar, RichText};
use peeroxide_net::Identity;
use peeroxide_update::{Config, Outcome, RELEASES_API, Step, UpdateError, Version};

use crate::Args;
use crate::settings::Settings;
use crate::ui::{App, UpdateNote};

/// Everything the main UI needs, held until the update phase is over.
pub struct Start {
    pub args: Args,
    pub identity: Identity,
    pub display_name: String,
    pub settings: Settings,
    pub dir: PathBuf,
}

/// A slow check can be skipped after this long.
const SKIP_AFTER: Duration = Duration::from_secs(2);
const JUST_UPDATED: &str = "--just-updated";

enum Msg {
    Step(Step),
    Done(Outcome),
}

struct Updating {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    step: Step,
    started: Instant,
    exe: PathBuf,
    start: Start,
}

enum Phase {
    Updating(Box<Updating>),
    Restarting,
    Main(Box<App>),
    Failed(String),
}

pub struct Launcher {
    phase: Phase,
    ctx: egui::Context,
}

impl Launcher {
    pub fn new(start: Start, ctx: egui::Context) -> anyhow::Result<Self> {
        let Some(cfg) = update_config(&start.args) else {
            let note = start.args.just_updated.as_ref().map(|v| UpdateNote {
                text: format!("Updated to {v}"),
                link: None,
                warn: false,
            });
            let mut app = new_app(start, &ctx)?;
            if let Some(note) = note {
                app.set_update_note(note);
            }
            return Ok(Self {
                phase: Phase::Main(Box::new(app)),
                ctx,
            });
        };
        tracing::info!(current = %cfg.current, url = %cfg.api_url, "checking for updates");
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let exe = cfg.exe.clone();
        std::thread::Builder::new().name("updater".into()).spawn({
            let ctx = ctx.clone();
            let cancel = cancel.clone();
            move || {
                let outcome = peeroxide_update::run(&cfg, &cancel, |step| {
                    let _ = tx.send(Msg::Step(step));
                    ctx.request_repaint();
                });
                let _ = tx.send(Msg::Done(outcome));
                ctx.request_repaint();
            }
        })?;
        Ok(Self {
            phase: Phase::Updating(Box::new(Updating {
                rx,
                cancel,
                step: Step::Checking,
                started: Instant::now(),
                exe,
                start,
            })),
            ctx,
        })
    }

    /// Applies what the updater reported.
    fn poll(&mut self) {
        let Phase::Updating(u) = &mut self.phase else {
            return;
        };
        let mut done = None;
        while let Ok(msg) = u.rx.try_recv() {
            match msg {
                Msg::Step(step) => u.step = step,
                Msg::Done(outcome) => done = Some(outcome),
            }
        }
        if let Some(outcome) = done {
            self.finish(outcome);
        }
    }

    fn finish(&mut self, outcome: Outcome) {
        let Phase::Updating(u) = std::mem::replace(&mut self.phase, Phase::Restarting) else {
            return;
        };
        let Updating { exe, start, .. } = *u;
        let note = match outcome {
            Outcome::Installed(release) => {
                let args = relaunch_args(std::env::args_os().skip(1), &release.version);
                match peeroxide_update::relaunch(&exe, &args) {
                    Ok(()) => {
                        tracing::info!(version = %release.version, "restarting into the update");
                        self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        return;
                    }
                    Err(e) => {
                        tracing::warn!("could not restart after updating: {e}");
                        Some(UpdateNote {
                            text: format!(
                                "Updated to {}: restart Peeroxide to use it",
                                release.version
                            ),
                            link: None,
                            warn: true,
                        })
                    }
                }
            }
            Outcome::UpToDate => {
                tracing::info!("up to date");
                None
            }
            Outcome::Skipped | Outcome::Busy => None,
            Outcome::CheckFailed(e) => {
                tracing::warn!("update check failed: {e}");
                Some(UpdateNote {
                    text: "Couldn't check for updates".into(),
                    link: None,
                    warn: false,
                })
            }
            Outcome::NotInstalled { release, error } => Some(not_installed_note(&release, &error)),
        };
        self.open_main(start, note);
    }

    /// Skip: open the app now; the updater stops on its own and its result is ignored.
    fn skip(&mut self) {
        let Phase::Updating(u) = std::mem::replace(&mut self.phase, Phase::Restarting) else {
            return;
        };
        u.cancel.store(true, Ordering::Relaxed);
        tracing::info!("update skipped");
        self.open_main(u.start, None);
    }

    fn open_main(&mut self, start: Start, note: Option<UpdateNote>) {
        self.phase = match new_app(start, &self.ctx) {
            Ok(mut app) => {
                if let Some(note) = note {
                    app.set_update_note(note);
                }
                Phase::Main(Box::new(app))
            }
            Err(e) => {
                tracing::error!("could not start: {e}");
                Phase::Failed(e.to_string())
            }
        };
    }
}

/// What to tell the user when a newer version exists but wasn't installed. The details are in
/// the log.
fn not_installed_note(release: &peeroxide_update::Release, error: &UpdateError) -> UpdateNote {
    let version = &release.version;
    match error {
        // Something is wrong with the published update itself: don't send people to download it.
        UpdateError::Verification(_) => UpdateNote {
            text: format!(
                "The update to {version} was rejected because its signature is invalid. Keep                  using this version and tell whoever publishes Peeroxide."
            ),
            link: None,
            warn: true,
        },
        UpdateError::NotWritable(_) => UpdateNote {
            text: format!("Version {version} is available, but this folder can't be updated."),
            link: Some(release.page_url.clone()),
            warn: true,
        },
        _ => UpdateNote {
            text: format!("Version {version} is available but couldn't be installed."),
            link: Some(release.page_url.clone()),
            warn: true,
        },
    }
}

fn new_app(start: Start, ctx: &egui::Context) -> anyhow::Result<App> {
    App::new(
        start.args,
        start.identity,
        start.display_name,
        start.settings,
        start.dir,
        ctx.clone(),
    )
}

/// `None` when the app shouldn't update itself this time.
fn update_config(args: &Args) -> Option<Config> {
    // Development builds (`cargo run`) never replace themselves.
    if cfg!(debug_assertions) {
        return None;
    }
    let disabled = args.no_update
        || args.just_updated.is_some()
        || std::env::var_os("PEEROXIDE_NO_UPDATE").is_some_and(|v| v != "0");
    if disabled {
        return None;
    }
    let platform = peeroxide_update::PLATFORM?;
    let Some(public_key) = peeroxide_update::built_in_public_key() else {
        tracing::info!("no release key built in; updates are off");
        return None;
    };
    let exe = std::env::current_exe().ok()?;
    let api_url = std::env::var("PEEROXIDE_UPDATE_URL").unwrap_or_else(|_| RELEASES_API.into());
    Some(Config {
        api_url,
        current: Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver"),
        platform: platform.into(),
        public_key: public_key.into(),
        exe,
    })
}

/// The options the app was started with, plus `--just-updated <version>` (replacing an earlier
/// one), so the restarted app does what was asked and doesn't check again straight away.
fn relaunch_args(original: impl Iterator<Item = OsString>, version: &Version) -> Vec<OsString> {
    let mut args = Vec::new();
    let mut skip_value = false;
    for arg in original {
        if std::mem::take(&mut skip_value) {
            continue;
        }
        if arg == JUST_UPDATED {
            skip_value = true;
            continue;
        }
        if arg.to_string_lossy().starts_with("--just-updated=") {
            continue;
        }
        args.push(arg);
    }
    args.push(JUST_UPDATED.into());
    args.push(version.to_string().into());
    args
}

impl eframe::App for Launcher {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.poll();
        let mut skip = false;
        match &mut self.phase {
            Phase::Main(app) => app.ui(ui, frame),
            Phase::Updating(u) => {
                let can_skip = match u.step {
                    Step::Checking => u.started.elapsed() >= SKIP_AFTER,
                    Step::Downloading { .. } => true,
                    Step::Verifying | Step::Installing => false,
                };
                if u.step == Step::Checking && !can_skip {
                    // Bring the Skip button in on time even if nothing else happens.
                    ui.ctx().request_repaint_after(SKIP_AFTER);
                }
                egui::CentralPanel::default().show(ui, |ui| {
                    centered(ui, |ui| {
                        status_ui(ui, &u.step);
                        if can_skip {
                            ui.add_space(12.0);
                            skip = ui
                                .button("Skip")
                                .on_hover_text("Open now and update next time")
                                .clicked();
                        }
                    });
                });
            }
            Phase::Restarting => {
                egui::CentralPanel::default().show(ui, |ui| {
                    centered(ui, |ui| {
                        ui.spinner();
                        ui.label("Restarting…");
                    });
                });
            }
            Phase::Failed(error) => {
                egui::CentralPanel::default().show(ui, |ui| {
                    centered(ui, |ui| {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            format!("Peeroxide could not start: {error}"),
                        );
                    });
                });
            }
        }
        if skip {
            self.skip();
        }
    }
}

fn centered(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.35);
        ui.label(RichText::new("Peeroxide").heading().strong());
        ui.add_space(16.0);
        add(ui);
    });
}

fn status_ui(ui: &mut egui::Ui, step: &Step) {
    match step {
        Step::Checking => {
            ui.spinner();
            ui.label("Checking for updates…");
        }
        Step::Downloading {
            version,
            done,
            total,
        } => {
            ui.label(format!("Updating to {version}…"));
            ui.add_space(6.0);
            let fraction = if *total == 0 {
                0.0
            } else {
                *done as f32 / *total as f32
            };
            ui.add(
                ProgressBar::new(fraction)
                    .desired_width(320.0)
                    .show_percentage(),
            );
            let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
            ui.weak(format!("{:.1} of {:.1} MB", mb(*done), mb(*total)));
        }
        Step::Verifying => {
            ui.spinner();
            ui.label("Checking the download…");
        }
        Step::Installing => {
            ui.spinner();
            ui.label("Installing…");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn relaunch_keeps_the_options_and_marks_the_update_once() {
        let v = Version::new(0, 5, 0);
        assert_eq!(
            relaunch_args(
                args(&["--profile", "a", "--broadcast", "test"]).into_iter(),
                &v
            ),
            args(&[
                "--profile",
                "a",
                "--broadcast",
                "test",
                "--just-updated",
                "0.5.0"
            ])
        );
        assert_eq!(
            relaunch_args(
                args(&[
                    "--just-updated",
                    "0.4.9",
                    "--name",
                    "Ana",
                    "--just-updated=0.4.8"
                ])
                .into_iter(),
                &v
            ),
            args(&["--name", "Ana", "--just-updated", "0.5.0"])
        );
        assert_eq!(
            relaunch_args(std::iter::empty(), &v),
            args(&["--just-updated", "0.5.0"])
        );
    }

    #[test]
    fn a_rejected_signature_never_links_to_the_download() {
        let release = peeroxide_update::Release {
            version: Version::new(0, 5, 0),
            tag: "v0.5.0".into(),
            page_url: "https://example.com/v0.5.0".into(),
            package: peeroxide_update::Asset {
                name: "p.zip".into(),
                size: 1,
                url: "https://example.com/p.zip".into(),
            },
            signature: peeroxide_update::Asset {
                name: "p.zip.minisig".into(),
                size: 1,
                url: "https://example.com/p.zip.minisig".into(),
            },
        };
        let bad = not_installed_note(&release, &UpdateError::Verification("x".into()));
        assert!(bad.link.is_none());
        assert!(bad.text.contains("rejected"));
        let read_only = not_installed_note(&release, &UpdateError::NotWritable("x".into()));
        assert_eq!(
            read_only.link.as_deref(),
            Some("https://example.com/v0.5.0")
        );
    }

    #[test]
    fn the_hidden_flag_parses() {
        use clap::Parser;
        let a =
            Args::try_parse_from(["peeroxide", "--no-update", "--just-updated", "0.5.0"]).unwrap();
        assert!(a.no_update);
        assert_eq!(a.just_updated.as_deref(), Some("0.5.0"));
        assert!(update_config(&a).is_none());
    }
}
