//! Self-update from GitHub Releases: find a newer signed release, download and verify it, and
//! replace the running executable with it.
//!
//! Nothing is installed unless its minisign signature checks out against the release public key
//! built into the app, and the signature's trusted comment names exactly the downloaded file, so
//! an older signed package can't be passed off as a newer one (abuse case AC-12). Every failure
//! leaves the running version untouched (NFR-14).

mod download;
mod github;
mod install;
mod select;
#[cfg(test)]
mod tests;
mod verify;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use install::{EXE_NAME, UpdateLock, extract_exe, relaunch};
pub use select::{Asset, Release, parse_releases, select_update};
pub use semver::Version;
pub use verify::{built_in_public_key, verify_file};

/// The project's releases, newest first.
pub const RELEASES_API: &str = "https://api.github.com/repos/gventino/peeroxide/releases";

/// Release packages are named `peeroxide-<version>-<platform>.zip`. `None` where no packages
/// are published yet.
pub const PLATFORM: Option<&str> = if cfg!(all(windows, target_arch = "x86_64")) {
    Some("windows-x64")
} else {
    None
};

/// How long the startup check may take before the app opens anyway.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const SIGNATURE_LIMIT: u64 = 4 * 1024;
const PACKAGE_LIMIT: u64 = 200 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("could not reach the update server: {0}")]
    Network(String),
    #[error("the update server is busy (rate limit); trying again next time")]
    RateLimited,
    #[error("unexpected answer from the update server: {0}")]
    BadResponse(String),
    #[error("refusing insecure address {0}")]
    InsecureUrl(String),
    #[error("download larger than expected")]
    TooLarge,
    #[error("the update's signature is invalid: {0}")]
    Verification(String),
    #[error("the update package is broken: {0}")]
    Package(String),
    #[error("can't write next to the app ({0})")]
    NotWritable(String),
    #[error("could not replace the app: {0}")]
    Install(String),
    #[error("cancelled")]
    Cancelled,
}

pub struct Config {
    /// GitHub's "list releases" endpoint (or a local test server).
    pub api_url: String,
    pub current: Version,
    pub platform: String,
    /// Minisign public key (the contents of a `.pub` file or the bare base64 key).
    pub public_key: String,
    /// The running executable, replaced on success.
    pub exe: PathBuf,
}

/// Progress reported while [`run`] works.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Checking,
    Downloading {
        version: Version,
        done: u64,
        total: u64,
    },
    Verifying,
    Installing,
}

#[derive(Debug)]
pub enum Outcome {
    UpToDate,
    /// The user skipped, or the check was cancelled.
    Skipped,
    /// Another instance is updating right now.
    Busy,
    /// Couldn't find out whether there is an update.
    CheckFailed(UpdateError),
    /// The executable was replaced; restart to run the new version.
    Installed(Release),
    /// A newer version exists but couldn't be installed here (e.g. a read-only folder).
    NotInstalled {
        release: Release,
        error: UpdateError,
    },
}

fn temp_path(exe: &Path, suffix: &str) -> PathBuf {
    exe.with_file_name(format!("peeroxide.update.{suffix}"))
}

/// Checks for a newer release and installs it over the running executable. Blocking; call it on
/// a background thread. Setting `cancel` stops it as soon as possible with [`Outcome::Skipped`].
pub fn run(cfg: &Config, cancel: &AtomicBool, on_step: impl FnMut(Step)) -> Outcome {
    run_with(cfg, cancel, on_step, install::replace_running_exe)
}

/// [`run`], with the final step (putting the verified executable in place) supplied by the
/// caller.
pub(crate) fn run_with(
    cfg: &Config,
    cancel: &AtomicBool,
    mut on_step: impl FnMut(Step),
    put_in_place: impl FnOnce(&Path) -> Result<(), UpdateError>,
) -> Outcome {
    let Some(dir) = cfg.exe.parent() else {
        return Outcome::CheckFailed(UpdateError::NotWritable("no app folder".into()));
    };
    let Some(_lock) = UpdateLock::acquire(dir) else {
        return Outcome::Busy;
    };
    on_step(Step::Checking);
    let user_agent = format!("Peeroxide/{}", cfg.current);
    let releases = match github::fetch(&cfg.api_url, &user_agent) {
        Ok(json) => json,
        Err(e) => return Outcome::CheckFailed(e),
    };
    if cancel.load(Ordering::Relaxed) {
        return Outcome::Skipped;
    }
    let release = match parse_releases(&releases) {
        Ok(list) => match select_update(&list, &cfg.current, &cfg.platform) {
            Some(r) => r,
            None => return Outcome::UpToDate,
        },
        Err(e) => return Outcome::CheckFailed(e),
    };
    tracing::info!(version = %release.version, tag = %release.tag, "update available");

    let zip = temp_path(&cfg.exe, "zip");
    let exe = temp_path(&cfg.exe, "exe");
    let result = download_and_extract(cfg, &release, &zip, &exe, cancel, &mut on_step)
        .and_then(|()| put_in_place(&exe));
    let _ = std::fs::remove_file(&zip);
    let _ = std::fs::remove_file(&exe);
    match result {
        Ok(()) => {
            tracing::info!(version = %release.version, "update installed");
            Outcome::Installed(release)
        }
        Err(UpdateError::Cancelled) => Outcome::Skipped,
        Err(error) => {
            tracing::warn!(version = %release.version, "update not installed: {error}");
            Outcome::NotInstalled { release, error }
        }
    }
}

fn download_and_extract(
    cfg: &Config,
    release: &Release,
    zip: &Path,
    exe: &Path,
    cancel: &AtomicBool,
    on_step: &mut impl FnMut(Step),
) -> Result<(), UpdateError> {
    let user_agent = format!("Peeroxide/{}", cfg.current);
    let signature = download::to_string(&release.signature.url, SIGNATURE_LIMIT, &user_agent)?;
    let total = release.package.size;
    on_step(Step::Downloading {
        version: release.version.clone(),
        done: 0,
        total,
    });
    download::to_file(
        &release.package.url,
        zip,
        PACKAGE_LIMIT,
        total,
        &user_agent,
        cancel,
        |done| {
            on_step(Step::Downloading {
                version: release.version.clone(),
                done,
                total,
            })
        },
    )?;
    on_step(Step::Verifying);
    verify_file(zip, &signature, &release.package.name, &cfg.public_key)?;
    on_step(Step::Installing);
    extract_exe(zip, exe)
}
