//! A monitor's sound minus the muted apps (Windows). Follows the apps that come and go and the
//! broadcaster's choices: one whole-system capture while nothing that plays is muted, otherwise
//! one capture per other app, mixed (see `apps.rs` for why).

use std::collections::HashMap;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;

use crate::apps::{self, Plan};
use crate::mixer::Mixer;
use crate::{AudioCapture, AudioSource, MutedApps, Next, Sink, start_capture};

/// How often the apps playing sound are listed again (and at once after a choice changes).
const RESCAN: Duration = Duration::from_secs(1);
/// How often the captures are collected and the mix released.
const TICK: Duration = Duration::from_millis(5);

pub(crate) fn start(
    exclude_pid: u32,
    muted: MutedApps,
    sink: Sink,
) -> anyhow::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("audio-filter".into())
        .spawn(move || {
            let _ = wasapi::initialize_mta();
            if let Err(e) = run(exclude_pid, &muted, &sink) {
                tracing::warn!("audio capture failed: {e:#}");
                sink.fail(e);
            }
        })
        .context("could not start the audio capture thread")
}

enum Mode {
    /// One capture of everything except Peeroxide.
    Everything(AudioCapture),
    /// One capture per process (with its descendants), mixed.
    Apps {
        captures: HashMap<u32, AudioCapture>,
        mixer: Mixer,
    },
}

fn run(exclude_pid: u32, muted: &MutedApps, sink: &Sink) -> anyhow::Result<()> {
    let mut mode: Option<Mode> = None;
    let mut plan: Option<Plan> = None;
    let mut scanned: Option<(Instant, u64)> = None;

    while !sink.stopped() {
        let generation = muted.generation();
        if scanned.is_none_or(|(at, g)| at.elapsed() >= RESCAN || g != generation) {
            scanned = Some((Instant::now(), generation));
            match apps::os::list(exclude_pid) {
                Ok((list, processes)) => {
                    let next = apps::plan(&list, muted, &processes);
                    if plan.as_ref() != Some(&next) {
                        let muted_names: Vec<&str> = list
                            .iter()
                            .filter(|a| muted.is_muted(&a.key))
                            .map(|a| a.name.as_str())
                            .collect();
                        tracing::info!(?next, muted = ?muted_names, "shared sound changed");
                        mode = Some(apply(mode.take(), &next, exclude_pid)?);
                        plan = Some(next);
                    }
                }
                // Keep the current captures; the next scan tries again.
                Err(e) => tracing::debug!("could not list the apps playing sound: {e:#}"),
            }
        }

        match &mut mode {
            Some(Mode::Everything(capture)) => loop {
                match capture.next(Duration::ZERO) {
                    Next::Chunk(chunk) => sink.chunk(chunk),
                    Next::Timeout => break,
                    Next::Failed(e) => return Err(e),
                }
            },
            Some(Mode::Apps { captures, mixer }) => {
                let mut ended = Vec::new();
                for (&pid, capture) in captures.iter() {
                    loop {
                        match capture.next(Duration::ZERO) {
                            Next::Chunk(chunk) => mixer.push(pid, &chunk),
                            Next::Timeout => break,
                            Next::Failed(e) => {
                                // Usually the app quit; the next scan notices.
                                tracing::debug!(pid, "app audio capture ended: {e:#}");
                                ended.push(pid);
                                break;
                            }
                        }
                    }
                }
                for pid in ended {
                    captures.remove(&pid);
                    mixer.remove(pid);
                }
                while let Some(chunk) = mixer.pop(Instant::now()) {
                    sink.chunk(chunk);
                }
            }
            None => {}
        }
        std::thread::sleep(TICK);
    }
    Ok(())
}

/// Moves from the current captures to what `plan` asks for, keeping what can be kept.
fn apply(current: Option<Mode>, plan: &Plan, exclude_pid: u32) -> anyhow::Result<Mode> {
    match plan {
        Plan::Everything => match current {
            Some(Mode::Everything(capture)) => Ok(Mode::Everything(capture)),
            _ => Ok(Mode::Everything(start_capture(&AudioSource::System {
                exclude_pid,
            })?)),
        },
        Plan::Only(pids) => {
            let (mut captures, mut mixer) = match current {
                Some(Mode::Apps { captures, mixer }) => (captures, mixer),
                _ => (HashMap::new(), Mixer::new(Instant::now())),
            };
            captures.retain(|pid, _| pids.contains(pid));
            for &pid in pids {
                if captures.contains_key(&pid) {
                    continue;
                }
                mixer.remove(pid);
                match start_capture(&AudioSource::Application { pid }) {
                    Ok(capture) => {
                        captures.insert(pid, capture);
                    }
                    // E.g. the app just quit; the next scan tries again if it's still there.
                    Err(e) => tracing::debug!(pid, "could not capture an app's sound: {e:#}"),
                }
            }
            Ok(Mode::Apps { captures, mixer })
        }
    }
}
