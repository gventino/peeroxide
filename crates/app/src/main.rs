mod controller;
mod decoder;
mod encoder;
mod settings;
mod stats;
mod ui;
mod video;
mod viewer_state;

use std::path::PathBuf;

use clap::Parser;
use p2pss_net::Identity;

#[derive(Parser, Debug, Clone)]
#[command(version, about = "Peer-to-peer LAN screen sharing")]
pub struct Args {
    /// Start broadcasting right away: "test" (test pattern), "monitor" (first monitor),
    /// or part of a window title.
    #[arg(long, value_name = "SOURCE")]
    pub broadcast: Option<String>,

    /// Watch the first discovered broadcaster whose name contains this text.
    #[arg(long, value_name = "NAME")]
    pub watch: Option<String>,

    /// Watch a broadcaster directly, bypassing discovery: "IP:PORT#FINGERPRINT".
    #[arg(long, value_name = "ADDR#FINGERPRINT")]
    pub connect: Option<String>,

    /// Use a separate identity and settings, e.g. to run several instances on one machine.
    #[arg(long, value_name = "NAME", value_parser = parse_profile)]
    pub profile: Option<String>,

    /// Name shown to other peers (defaults to this computer's name).
    #[arg(long)]
    pub name: Option<String>,
}

fn parse_profile(s: &str) -> Result<String, String> {
    let ok = !s.is_empty()
        && s.len() <= 32
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| s.to_string())
        .ok_or_else(|| "use 1-32 letters, digits, '-' or '_'".into())
}

pub fn data_dir(profile: Option<&str>) -> PathBuf {
    let base = directories::ProjectDirs::from("", "", "P2P Screen Share")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("p2p-screen-share"));
    match profile {
        Some(p) => base.join("profiles").join(p),
        None => base,
    }
}

/// Logs to stdout and to a daily-rotated file under `<data dir>/logs`, which doubles as the
/// local session record (who broadcast/watched what, and when).
fn init_logging(dir: &std::path::Path) -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::prelude::*;
    let (file, guard) = tracing_appender::non_blocking(tracing_appender::rolling::daily(
        dir.join("logs"),
        "session.log",
    ));
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,p2pss=debug,wgpu_hal=warn,egui_wgpu=warn".into());
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file),
        )
        .init();
    guard
}

fn main() -> eframe::Result {
    let args = Args::parse();
    let dir = data_dir(args.profile.as_deref());
    let _log_guard = init_logging(&dir);

    let identity = Identity::load_or_create(&dir).unwrap_or_else(|e| {
        tracing::warn!(
            "could not load identity from {}: {e}; using a temporary one",
            dir.display()
        );
        Identity::generate().expect("generate identity")
    });
    let settings = settings::Settings::load(&dir);
    let display_name = args
        .name
        .clone()
        .or_else(|| settings.display_name.clone())
        .unwrap_or_else(|| {
            let host = gethostname::gethostname().to_string_lossy().into_owned();
            match &args.profile {
                Some(p) => format!("{host} ({p})"),
                None => host,
            }
        });
    tracing::info!(name = %display_name, fingerprint = %identity.fingerprint(), dir = %dir.display(), "starting");

    let title = match &args.profile {
        Some(p) => format!("P2P Screen Share — {p}"),
        None => "P2P Screen Share".into(),
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(&title)
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            let app = ui::App::new(
                args,
                identity,
                display_name,
                settings,
                dir,
                cc.egui_ctx.clone(),
            )?;
            Ok(Box::new(app))
        }),
    )
}
