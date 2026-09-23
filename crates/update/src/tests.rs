//! The whole update flow against a local HTTP server standing in for GitHub.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::install::tests::zip_with;
use crate::verify::tests::{sign, test_keys};
use crate::*;

/// Path → (status, body). A missing route answers 404.
type Routes = HashMap<String, (u16, Vec<u8>)>;

/// A throwaway HTTP/1.1 server. With `stall`, it accepts connections and never answers.
fn serve(routes: Routes, stall: bool) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            if stall {
                std::thread::sleep(Duration::from_secs(30));
                continue;
            }
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            let path = path.split('?').next().unwrap_or(path);
            let (status, body) = routes.get(path).cloned().unwrap_or((404, Vec::new()));
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    addr
}

const PACKAGE: &str = "peeroxide-0.5.0-windows-x64.zip";

struct Fixture {
    cfg: Config,
    dir: tempfile::TempDir,
    new_exe: Vec<u8>,
}

impl Fixture {
    /// A fake app folder with version 0.4.0, and a server offering a signed 0.5.0. `tweak` can
    /// change the served files after signing.
    fn new(tweak: impl FnOnce(&mut Routes, &mut String)) -> Self {
        let (public_key, secret_key) = test_keys();
        let new_exe = b"the 0.5.0 executable".to_vec();
        let exe_path = format!("peeroxide-0.5.0-windows-x64/{EXE_NAME}");
        let package = zip_with(&[
            (&exe_path, &new_exe),
            ("peeroxide-0.5.0-windows-x64/QUICKSTART.txt", b"hi"),
        ]);
        let signature = sign(&secret_key, &package, PACKAGE);
        let mut routes = Routes::new();
        let mut size = package.len().to_string();
        routes.insert(format!("/dl/{PACKAGE}"), (200, package));
        routes.insert(
            format!("/dl/{PACKAGE}.minisig"),
            (200, signature.into_bytes()),
        );
        tweak(&mut routes, &mut size);
        let addr = serve(routes.clone(), false);
        let json = format!(
            r#"[{{"tag_name":"v0.5.0-pre-alpha","draft":false,"html_url":"http://{addr}/page","assets":[
                {{"name":"{PACKAGE}","size":{size},"browser_download_url":"http://{addr}/dl/{PACKAGE}"}},
                {{"name":"{PACKAGE}.minisig","size":300,"browser_download_url":"http://{addr}/dl/{PACKAGE}.minisig"}}]}}]"#
        );
        routes.insert("/releases".into(), (200, json.into_bytes()));
        let addr = serve(routes, false);
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            api_url: format!("http://{addr}/releases"),
            current: Version::new(0, 4, 0),
            platform: "windows-x64".into(),
            public_key,
            exe: dir.path().join(EXE_NAME),
        };
        Self { cfg, dir, new_exe }
    }

    /// Runs the update; "installing" copies the verified executable to `installed`.
    fn run(&self, cancel_on_download: bool) -> (Outcome, Vec<Step>, Option<Vec<u8>>) {
        let cancel = AtomicBool::new(false);
        let mut steps = Vec::new();
        let installed: Arc<Mutex<Option<Vec<u8>>>> = Arc::default();
        let outcome = run_with(
            &self.cfg,
            &cancel,
            |step| {
                if cancel_on_download && matches!(step, Step::Downloading { .. }) {
                    cancel.store(true, Ordering::Relaxed);
                }
                steps.push(step);
            },
            |exe| {
                *installed.lock().unwrap() = Some(std::fs::read(exe).unwrap());
                Ok(())
            },
        );
        let installed = installed.lock().unwrap().take();
        (outcome, steps, installed)
    }

    fn leftovers(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    }
}

#[test]
fn installs_a_newer_signed_release() {
    let f = Fixture::new(|_, _| {});
    let (outcome, steps, installed) = f.run(false);
    let Outcome::Installed(release) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(release.version, Version::new(0, 5, 0));
    assert_eq!(installed.as_deref(), Some(&f.new_exe[..]));
    assert_eq!(steps.first(), Some(&Step::Checking));
    assert!(
        steps
            .iter()
            .any(|s| matches!(s, Step::Downloading { done, total, .. } if done == total))
    );
    assert!(steps.ends_with(&[Step::Verifying, Step::Installing]));
    assert!(
        f.leftovers().is_empty(),
        "temporary files and the lock are cleaned up"
    );
}

#[test]
fn up_to_date_when_nothing_newer() {
    let mut f = Fixture::new(|_, _| {});
    f.cfg.current = Version::new(0, 5, 0);
    assert!(matches!(f.run(false).0, Outcome::UpToDate));
}

#[test]
fn a_tampered_package_is_never_installed() {
    let f = Fixture::new(|routes, _| {
        let package = &mut routes.get_mut(&format!("/dl/{PACKAGE}")).unwrap().1;
        let last = package.len() - 1;
        package[last] ^= 1;
    });
    let (outcome, _, installed) = f.run(false);
    assert!(
        matches!(
            outcome,
            Outcome::NotInstalled {
                error: UpdateError::Verification(_),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(installed.is_none());
    assert!(f.leftovers().is_empty());
}

#[test]
fn a_package_of_the_wrong_size_is_rejected() {
    let f = Fixture::new(|_, size| *size = "10".into());
    let (outcome, _, installed) = f.run(false);
    assert!(
        matches!(
            outcome,
            Outcome::NotInstalled {
                error: UpdateError::TooLarge,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(installed.is_none());
}

#[test]
fn a_missing_signature_file_is_rejected() {
    let f = Fixture::new(|routes, _| {
        routes.remove(&format!("/dl/{PACKAGE}.minisig"));
    });
    let (outcome, _, installed) = f.run(false);
    assert!(
        matches!(outcome, Outcome::NotInstalled { .. }),
        "{outcome:?}"
    );
    assert!(installed.is_none());
}

#[test]
fn skipping_during_the_download_installs_nothing() {
    let f = Fixture::new(|_, _| {});
    let (outcome, _, installed) = f.run(true);
    assert!(matches!(outcome, Outcome::Skipped), "{outcome:?}");
    assert!(installed.is_none());
    assert!(f.leftovers().is_empty());
}

#[test]
fn rate_limits_and_bad_answers_only_fail_the_check() {
    for (status, body) in [(403, "rate limited"), (500, "oops"), (200, "<html>")] {
        let addr = serve(
            Routes::from([("/releases".into(), (status, body.into()))]),
            false,
        );
        let mut f = Fixture::new(|_, _| {});
        f.cfg.api_url = format!("http://{addr}/releases");
        assert!(
            matches!(f.run(false).0, Outcome::CheckFailed(_)),
            "{status}"
        );
    }
}

#[test]
fn a_silent_server_times_out_within_the_check_limit() {
    let addr = serve(Routes::new(), true);
    let mut f = Fixture::new(|_, _| {});
    f.cfg.api_url = format!("http://{addr}/releases");
    let started = Instant::now();
    assert!(matches!(f.run(false).0, Outcome::CheckFailed(_)));
    assert!(started.elapsed() < CHECK_TIMEOUT + Duration::from_secs(2));
}

#[test]
fn another_instance_updating_means_busy() {
    let f = Fixture::new(|_, _| {});
    let _held = UpdateLock::acquire(f.dir.path()).unwrap();
    assert!(matches!(f.run(false).0, Outcome::Busy));
}

#[test]
fn plain_http_downloads_from_other_hosts_are_refused() {
    let mut f = Fixture::new(|_, _| {});
    let json = format!(
        r#"[{{"tag_name":"v0.5.0","assets":[
            {{"name":"{PACKAGE}","size":10,"browser_download_url":"http://example.com/{PACKAGE}"}},
            {{"name":"{PACKAGE}.minisig","size":10,"browser_download_url":"http://example.com/{PACKAGE}.minisig"}}]}}]"#
    );
    let addr = serve(
        Routes::from([("/releases".into(), (200, json.into_bytes()))]),
        false,
    );
    f.cfg.api_url = format!("http://{addr}/releases");
    let (outcome, _, _) = f.run(false);
    assert!(
        matches!(
            outcome,
            Outcome::NotInstalled {
                error: UpdateError::InsecureUrl(_),
                ..
            }
        ),
        "{outcome:?}"
    );
}
