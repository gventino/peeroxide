// Release builds are GUI apps on Windows (no console window); logs still go to the log file.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio_decoder;
mod audio_encoder;
mod contacts;
mod controller;
mod decoder;
mod encoder;
mod fullscreen;
mod launcher;
mod settings;
mod stats;
mod ui;
mod video;
mod viewer_state;

use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use clap::Parser;
use clap::builder::FalseyValueParser;
use peeroxide_net::Identity;
use peeroxide_update::RELEASES_API;

#[derive(Parser, Debug, Clone)]
#[command(version, about = "Peer-to-peer LAN screen sharing")]
pub struct Args {
    /// Start broadcasting right away: "test" (test pattern), "monitor" (first monitor),
    /// or part of a window title.
    #[arg(long, value_name = "SOURCE")]
    pub broadcast: Option<String>,

    /// With --broadcast: turn on "Share audio" (remembered, like ticking the checkbox).
    #[arg(long, requires = "broadcast")]
    pub share_audio: bool,

    /// Watch the first discovered broadcaster whose name contains this text.
    #[arg(long, value_name = "NAME", conflicts_with = "connect")]
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

    /// Don't check for updates at start. In the environment variable, 0, false, off and no
    /// mean "check"; anything else turns the check off.
    #[arg(long, env = "PEEROXIDE_NO_UPDATE", value_parser = FalseyValueParser::new())]
    pub no_update: bool,

    /// Where to look for releases, e.g. a local test server (see docs/releasing.md).
    #[arg(long, hide = true, env = "PEEROXIDE_UPDATE_URL", default_value = RELEASES_API)]
    pub update_url: String,

    /// Set by the updater when it restarts the app: shows "Updated to VERSION" and skips the
    /// check once.
    #[arg(long, hide = true, value_name = "VERSION")]
    pub just_updated: Option<String>,
}

fn parse_profile(s: &str) -> anyhow::Result<String> {
    let ok = !s.is_empty()
        && s.len() <= 32
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ensure!(ok, "use 1-32 letters, digits, '-' or '_'");
    Ok(s.to_string())
}

const APP_NAME: &str = "Peeroxide";
/// The app's name up to 0.2; its data folder is moved to the new location on first run.
const OLD_APP_NAME: &str = "P2P Screen Share";

fn app_data_root(name: &str) -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", name).map(|d| d.data_dir().to_path_buf())
}

pub fn data_dir(profile: Option<&str>) -> PathBuf {
    let base = app_data_root(APP_NAME).unwrap_or_else(|| std::env::temp_dir().join("peeroxide"));
    match profile {
        Some(p) => base.join("profiles").join(p),
        None => base,
    }
}

/// Moves the pre-rename data folder (identity, settings, contacts, logs) to `new`, once.
/// Returns whether anything was migrated.
fn migrate_data(old: &Path, new: &Path) -> anyhow::Result<bool> {
    if new.exists() || !old.exists() {
        return Ok(false);
    }
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    if std::fs::rename(old, new).is_err() {
        // An old version still running keeps its log file open; copy everything else instead.
        copy_except_logs(old, new)?;
    }
    // On Windows the data lives in "<app>\data": drop the old, now empty, "<app>" folder.
    if old.file_name() == Some("data".as_ref())
        && let Some(parent) = old.parent()
    {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(true)
}

fn copy_except_logs(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    let entries = std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", src.display()))?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            if entry.file_name() != "logs" {
                copy_except_logs(&entry.path(), &target)?;
            }
        } else {
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copying {}", entry.path().display()))?;
        }
    }
    Ok(())
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
        .unwrap_or_else(|_| "info,peeroxide=debug,wgpu_hal=warn,egui_wgpu=warn".into());
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

/// rayon's pool only runs short per-frame jobs: windows-capture's copy of each padded frame on
/// Windows, and the pixel conversion on macOS/Linux. Its default size (one thread per core)
/// spins far more CPU than it saves on jobs that short. Sharing a window used 70-100% of a core
/// with the default pool, and 30-37% with one thread, at the same frame rate. `RAYON_NUM_THREADS`
/// still overrides the choice.
fn size_rayon_pool() {
    let threads = if std::env::var_os("RAYON_NUM_THREADS").is_some() {
        0 // let rayon read it
    } else if cfg!(windows) {
        1
    } else {
        2
    };
    let built = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("rayon-{i}"))
        .build_global();
    if let Err(e) = built {
        tracing::warn!("could not size the rayon pool: {e}");
    }
}

fn main() -> eframe::Result {
    let args = Args::parse();
    let migrated = match (app_data_root(OLD_APP_NAME), app_data_root(APP_NAME)) {
        (Some(old), Some(new)) => migrate_data(&old, &new),
        _ => Ok(false),
    };
    let dir = data_dir(args.profile.as_deref());
    let _log_guard = init_logging(&dir);
    size_rayon_pool();
    match migrated {
        Ok(true) => tracing::info!("moved data from the {OLD_APP_NAME} folder"),
        Ok(false) => {}
        Err(e) => tracing::warn!("could not migrate data from the {OLD_APP_NAME} folder: {e:#}"),
    }

    let identity = Identity::load_or_create(&dir).unwrap_or_else(|e| {
        tracing::warn!(
            "could not load identity from {}: {e:#}; using a temporary one",
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
        Some(p) => format!("{APP_NAME} — {p}"),
        None => APP_NAME.into(),
    };
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title(&title)
        .with_inner_size([1280.0, 800.0]);
    match eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon.png")) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(e) => tracing::warn!("could not load the window icon: {e}"),
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            let start = launcher::Start {
                args,
                identity,
                display_name,
                settings,
                dir,
            };
            Ok(Box::new(launcher::Launcher::new(
                start,
                cc.egui_ctx.clone(),
            )?))
        }),
    )
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_options_are_well_formed() {
        Args::command().debug_assert();
    }

    #[test]
    fn contradictory_options_are_refused() {
        let parse = |list: &[&str]| Args::try_parse_from([&["peeroxide"], list].concat());
        assert!(parse(&["--watch", "ana", "--connect", "127.0.0.1:1#ab"]).is_err());
        assert!(parse(&["--share-audio"]).is_err());
        assert!(parse(&["--broadcast", "test", "--share-audio"]).is_ok());
        assert!(parse(&["--profile", "bad name"]).is_err());
    }

    #[test]
    fn update_options_default_to_checking_github() {
        let a = Args::try_parse_from(["peeroxide"]).unwrap();
        assert!(!a.no_update);
        assert_eq!(a.update_url, RELEASES_API);
        let a =
            Args::try_parse_from(["peeroxide", "--update-url", "http://127.0.0.1:1/r"]).unwrap();
        assert_eq!(a.update_url, "http://127.0.0.1:1/r");
    }

    fn old_layout(root: &Path) -> PathBuf {
        let old = root.join(OLD_APP_NAME).join("data");
        std::fs::create_dir_all(old.join("logs")).unwrap();
        std::fs::create_dir_all(old.join("profiles").join("a")).unwrap();
        std::fs::write(old.join("identity.key.der"), b"key").unwrap();
        std::fs::write(old.join("contacts.toml"), b"contacts").unwrap();
        std::fs::write(old.join("profiles").join("a").join("settings.toml"), b"a").unwrap();
        std::fs::write(old.join("logs").join("session.log"), b"log").unwrap();
        old
    }

    #[test]
    fn moves_old_data_once_and_removes_the_old_folder() {
        let root = tempfile::tempdir().unwrap();
        let old = old_layout(root.path());
        let new = root.path().join(APP_NAME).join("data");

        assert!(migrate_data(&old, &new).unwrap());
        assert_eq!(std::fs::read(new.join("identity.key.der")).unwrap(), b"key");
        assert_eq!(
            std::fs::read(new.join("profiles").join("a").join("settings.toml")).unwrap(),
            b"a"
        );
        assert!(!root.path().join(OLD_APP_NAME).exists());

        assert!(
            !migrate_data(&old, &new).unwrap(),
            "nothing left to migrate"
        );
    }

    #[test]
    fn never_overwrites_existing_new_data() {
        let root = tempfile::tempdir().unwrap();
        let old = old_layout(root.path());
        let new = root.path().join(APP_NAME).join("data");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("identity.key.der"), b"newer").unwrap();

        assert!(!migrate_data(&old, &new).unwrap());
        assert_eq!(
            std::fs::read(new.join("identity.key.der")).unwrap(),
            b"newer"
        );
        assert!(old.exists());
    }

    #[test]
    fn copy_fallback_keeps_everything_but_logs() {
        let root = tempfile::tempdir().unwrap();
        let old = old_layout(root.path());
        let new = root.path().join("copy");

        copy_except_logs(&old, &new).unwrap();
        assert_eq!(
            std::fs::read(new.join("contacts.toml")).unwrap(),
            b"contacts"
        );
        assert!(
            new.join("profiles")
                .join("a")
                .join("settings.toml")
                .exists()
        );
        assert!(!new.join("logs").exists());
    }
}
