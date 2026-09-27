//! The apps playing sound on this computer, and which of them a shared monitor's audio leaves
//! out. Muting only matters for monitor shares: a window share captures one app's sound anyway.
//!
//! Windows can capture "everything except one process tree" or "only one process tree", never
//! "everything except several". Peeroxide itself always takes the one exclusion, so as soon as
//! an app is muted, every other app is captured on its own and the captures are mixed (see
//! [`plan`]). Windows' own sounds (notifications) have no process to capture, so they aren't
//! shared then.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// An app that has opened audio on this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioApp {
    /// Lowercase executable name, e.g. "discord.exe": what a mute choice is remembered by.
    pub key: String,
    /// For people, e.g. "Discord".
    pub name: String,
    /// Its processes that have an audio session.
    pub pids: Vec<u32>,
}

/// Voice and video chat apps, muted unless the broadcaster says otherwise: a friend in a call
/// who watches the stream would otherwise hear themselves.
const VOICE_CHAT: &[&str] = &[
    "discord.exe",
    "discordcanary.exe",
    "discordptb.exe",
    "element.exe",
    "guilded.exe",
    "mumble.exe",
    "ms-teams.exe",
    "signal.exe",
    "skype.exe",
    "slack.exe",
    "steamwebhelper.exe",
    "teams.exe",
    "teamspeak.exe",
    "telegram.exe",
    "ts3client_win32.exe",
    "ts3client_win64.exe",
    "viber.exe",
    "whatsapp.exe",
    "zoom.exe",
];

pub fn is_voice_chat(key: &str) -> bool {
    VOICE_CHAT.contains(&key.to_lowercase().as_str())
}

/// The broadcaster's mute choices, shared between the UI and a running capture, which follows
/// changes within a second. Apps without a choice are muted if they are voice chat apps.
#[derive(Clone, Debug, Default)]
pub struct MutedApps(Arc<Choices>);

#[derive(Debug, Default)]
struct Choices {
    muted: RwLock<BTreeMap<String, bool>>,
    generation: AtomicU64,
}

impl MutedApps {
    /// Starts from remembered choices (executable name → muted).
    pub fn new(choices: impl IntoIterator<Item = (String, bool)>) -> Self {
        let muted = choices
            .into_iter()
            .map(|(k, m)| (k.to_lowercase(), m))
            .collect();
        Self(Arc::new(Choices {
            muted: RwLock::new(muted),
            generation: AtomicU64::new(0),
        }))
    }

    pub fn is_muted(&self, key: &str) -> bool {
        let key = key.to_lowercase();
        let choice = self.0.muted.read().unwrap().get(&key).copied();
        choice.unwrap_or_else(|| is_voice_chat(&key))
    }

    pub fn set(&self, key: &str, muted: bool) {
        let old = self
            .0
            .muted
            .write()
            .unwrap()
            .insert(key.to_lowercase(), muted);
        if old != Some(muted) {
            self.0.generation.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The explicit choices, to remember them.
    pub fn choices(&self) -> BTreeMap<String, bool> {
        self.0.muted.read().unwrap().clone()
    }

    /// Changes whenever a choice does.
    pub fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Relaxed)
    }
}

/// A running process, from a snapshot of all of them.
#[derive(Clone, Debug)]
pub(crate) struct Process {
    pub parent: u32,
    /// Lowercase executable name.
    pub exe: String,
}

pub(crate) type Processes = HashMap<u32, Process>;

/// Whether `pid` is `root` or one of its descendants.
fn in_tree(pid: u32, root: u32, processes: &Processes) -> bool {
    let mut current = pid;
    // Bounded: parent ids can be stale (reused by a newer process) and form a cycle.
    for _ in 0..64 {
        if current == root {
            return true;
        }
        match processes.get(&current) {
            Some(p) if p.parent != current && p.parent != 0 => current = p.parent,
            _ => return false,
        }
    }
    false
}

/// Peeroxide's executable. Other instances (e.g. a second profile watching a stream) are left
/// out of the list like this one, so the mix never carries a stream back out.
const PEEROXIDE: &str = "peeroxide.exe";

/// The processes owning audio `sessions`, grouped by executable. Leaves out PID 0 (Windows'
/// own sounds), processes that are gone, and Peeroxide (`own_pid`'s tree and other instances).
pub(crate) fn group(
    sessions: &[u32],
    processes: &Processes,
    own_pid: u32,
    name_of: impl Fn(u32, &str) -> String,
) -> Vec<AudioApp> {
    let mut apps: BTreeMap<String, AudioApp> = BTreeMap::new();
    let mut seen = HashSet::new();
    for &pid in sessions {
        if pid == 0 || !seen.insert(pid) || in_tree(pid, own_pid, processes) {
            continue;
        }
        let Some(process) = processes.get(&pid).filter(|p| p.exe != PEEROXIDE) else {
            continue;
        };
        apps.entry(process.exe.clone())
            .or_insert_with(|| AudioApp {
                key: process.exe.clone(),
                name: name_of(pid, &process.exe),
                pids: Vec::new(),
            })
            .pids
            .push(pid);
    }
    let mut apps: Vec<AudioApp> = apps.into_values().collect();
    for app in &mut apps {
        app.pids.sort_unstable();
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// How to capture a monitor's sound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    /// Nothing that plays is muted: everything except Peeroxide, in one capture.
    Everything,
    /// Capture these processes, each with its descendants, and mix them.
    Only(Vec<u32>),
}

/// Which processes to capture so the mix holds every app's sound except the muted ones'.
/// Each capture includes the process's descendants, so:
/// - a process with a muted descendant isn't captured (its own sound is lost, rather than
///   leaking the muted app's, e.g. a launcher that started Discord);
/// - a process inside another captured process's tree is already covered.
pub(crate) fn plan(apps: &[AudioApp], muted: &MutedApps, processes: &Processes) -> Plan {
    let (silenced, shared): (Vec<&AudioApp>, Vec<&AudioApp>) =
        apps.iter().partition(|a| muted.is_muted(&a.key));
    if silenced.is_empty() {
        return Plan::Everything;
    }
    let silenced: Vec<u32> = silenced
        .iter()
        .flat_map(|a| a.pids.iter().copied())
        .collect();
    let candidates: Vec<u32> = shared
        .iter()
        .flat_map(|a| a.pids.iter().copied())
        .filter(|&pid| !silenced.iter().any(|&m| in_tree(m, pid, processes)))
        .collect();
    let mut pids: Vec<u32> = candidates
        .iter()
        .copied()
        .filter(|&pid| {
            !candidates
                .iter()
                .any(|&other| other != pid && in_tree(pid, other, processes))
        })
        .collect();
    pids.sort_unstable();
    Plan::Only(pids)
}

/// "discord.exe" → "Discord": a name for an app known only by its executable.
pub fn name_from_exe(exe: &str) -> String {
    let stem = exe.strip_suffix(".exe").unwrap_or(exe);
    let mut chars = stem.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The apps that have opened audio on this computer (any output device), except Peeroxide.
/// Empty where audio can't be shared yet (not Windows).
pub fn audio_apps() -> anyhow::Result<Vec<AudioApp>> {
    #[cfg(windows)]
    {
        use anyhow::Context;
        std::thread::Builder::new()
            .name("audio-apps".into())
            .spawn(|| {
                let _ = wasapi::initialize_mta();
                os::list(std::process::id()).map(|(apps, _)| apps)
            })
            .context("could not start the audio app listing thread")?
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("listing audio apps panicked")))
    }
    #[cfg(not(windows))]
    Ok(Vec::new())
}

#[cfg(windows)]
pub(crate) mod os {
    #![allow(unsafe_code)]

    use anyhow::Context;
    use wasapi::{DeviceEnumerator, Direction, SessionState};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::{HSTRING, PWSTR};

    use super::{AudioApp, Process, Processes, group, name_from_exe};

    /// Every running process, from one snapshot.
    pub(crate) fn processes() -> anyhow::Result<Processes> {
        let mut out = Processes::new();
        // SAFETY: the snapshot handle is closed below; the entry is sized as the API requires.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
                .context("could not list the running processes")?;
            let mut entry = PROCESSENTRY32W {
                dwSize: size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut next = Process32FirstW(snapshot, &mut entry);
            while next.is_ok() {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
                out.insert(
                    entry.th32ProcessID,
                    Process {
                        parent: entry.th32ParentProcessID,
                        exe: String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase(),
                    },
                );
                next = Process32NextW(snapshot, &mut entry);
            }
            let _ = CloseHandle(snapshot);
        }
        Ok(out)
    }

    /// Processes owning an audio session on any active output device. Needs COM.
    fn session_pids() -> anyhow::Result<Vec<u32>> {
        let devices = DeviceEnumerator::new()
            .and_then(|e| e.get_device_collection(&Direction::Render))
            .context("could not list the audio output devices")?;
        let mut pids = Vec::new();
        for device in &devices {
            let Ok(device) = device else { continue };
            let Ok(sessions) = device
                .get_iaudiosessionmanager()
                .and_then(|m| m.get_audiosessionenumerator())
            else {
                continue;
            };
            for i in 0..sessions.get_count().unwrap_or(0) {
                let Ok(session) = sessions.get_session(i) else {
                    continue;
                };
                if session.get_state().ok() == Some(SessionState::Expired) {
                    continue;
                }
                if let Ok(pid) = session.get_process_id() {
                    pids.push(pid);
                }
            }
        }
        Ok(pids)
    }

    /// The executable's description ("TeamSpeak 3 Client"), or its name ("Ts3client_win64").
    fn friendly_name(pid: u32, exe: &str) -> String {
        description(pid)
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| name_from_exe(exe))
    }

    fn description(pid: u32) -> Option<String> {
        // SAFETY: the process handle is closed; buffers are sized as each call reports.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut path = vec![0u16; 1024];
            let mut len = path.len() as u32;
            let named = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(path.as_mut_ptr()),
                &mut len,
            );
            let _ = CloseHandle(process);
            named.ok()?;
            let path = HSTRING::from_wide(&path[..len as usize]);

            let size = GetFileVersionInfoSizeW(&path, None);
            if size == 0 {
                return None;
            }
            let mut info = vec![0u8; size as usize];
            GetFileVersionInfoW(&path, None, size, info.as_mut_ptr().cast()).ok()?;

            let mut ptr = std::ptr::null_mut();
            let mut bytes = 0u32;
            let translation = HSTRING::from("\\VarFileInfo\\Translation");
            if !VerQueryValueW(info.as_ptr().cast(), &translation, &mut ptr, &mut bytes).as_bool()
                || bytes < 4
            {
                return None;
            }
            let ids = std::slice::from_raw_parts(ptr as *const u16, 2);
            let key = HSTRING::from(format!(
                "\\StringFileInfo\\{:04x}{:04x}\\FileDescription",
                ids[0], ids[1]
            ));
            let mut chars = 0u32;
            if !VerQueryValueW(info.as_ptr().cast(), &key, &mut ptr, &mut chars).as_bool()
                || chars == 0
            {
                return None;
            }
            let text = std::slice::from_raw_parts(ptr as *const u16, chars as usize);
            let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
            Some(String::from_utf16_lossy(&text[..end]).trim().to_string())
        }
    }

    /// The audio apps and the process snapshot they came from. Needs COM.
    pub(crate) fn list(own_pid: u32) -> anyhow::Result<(Vec<AudioApp>, Processes)> {
        let sessions = session_pids()?;
        let processes = processes()?;
        let apps = group(&sessions, &processes, own_pid, friendly_name);
        Ok((apps, processes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn procs(list: &[(u32, u32, &str)]) -> Processes {
        list.iter()
            .map(|&(pid, parent, exe)| {
                (
                    pid,
                    Process {
                        parent,
                        exe: exe.into(),
                    },
                )
            })
            .collect()
    }

    fn name(_: u32, exe: &str) -> String {
        name_from_exe(exe)
    }

    /// explorer (1) started Peeroxide (10, playback in child 11), Discord (20, voice in
    /// child 21), Chrome (30, audio service 31), a launcher (40) that started a game (41), and
    /// a second Peeroxide (50).
    fn desktop() -> Processes {
        procs(&[
            (1, 0, "explorer.exe"),
            (10, 1, "peeroxide.exe"),
            (11, 10, "peeroxide.exe"),
            (20, 1, "discord.exe"),
            (21, 20, "discord.exe"),
            (30, 1, "chrome.exe"),
            (31, 30, "chrome.exe"),
            (40, 1, "launcher.exe"),
            (41, 40, "game.exe"),
            (50, 1, "peeroxide.exe"),
        ])
    }

    #[test]
    fn apps_are_grouped_by_executable_without_peeroxide_or_system_sounds() {
        let apps = group(&[0, 11, 21, 31, 31, 99, 41, 50], &desktop(), 10, name);
        let summary: Vec<(&str, &str, Vec<u32>)> = apps
            .iter()
            .map(|a| (a.key.as_str(), a.name.as_str(), a.pids.clone()))
            .collect();
        assert_eq!(
            summary,
            [
                ("chrome.exe", "Chrome", vec![31]),
                ("discord.exe", "Discord", vec![21]),
                ("game.exe", "Game", vec![41]),
            ]
        );
    }

    #[test]
    fn voice_chat_apps_are_recognized_in_any_case() {
        for key in [
            "discord.exe",
            "Discord.exe",
            "ts3client_win64.exe",
            "TeamSpeak.exe",
        ] {
            assert!(is_voice_chat(key), "{key}");
        }
        for key in ["chrome.exe", "game.exe", "discord"] {
            assert!(!is_voice_chat(key), "{key}");
        }
    }

    #[test]
    fn choices_override_the_voice_chat_default_and_bump_the_generation() {
        let muted = MutedApps::new([("Chrome.exe".to_string(), true)]);
        assert!(muted.is_muted("discord.exe"), "voice chat starts muted");
        assert!(muted.is_muted("chrome.exe"));
        assert!(!muted.is_muted("game.exe"));

        let g = muted.generation();
        muted.set("DISCORD.EXE", false);
        assert!(!muted.is_muted("discord.exe"));
        assert_eq!(muted.generation(), g + 1);
        muted.set("discord.exe", false);
        assert_eq!(muted.generation(), g + 1, "no change, no bump");
        assert_eq!(
            muted.choices().into_iter().collect::<Vec<_>>(),
            [("chrome.exe".into(), true), ("discord.exe".into(), false)]
        );
    }

    #[test]
    fn nothing_muted_captures_everything_in_one_go() {
        let apps = group(&[31, 41], &desktop(), 10, name);
        assert_eq!(
            plan(&apps, &MutedApps::default(), &desktop()),
            Plan::Everything
        );
    }

    #[test]
    fn muting_captures_every_other_app_on_its_own() {
        let apps = group(&[21, 31, 41], &desktop(), 10, name);
        // Discord is muted by default.
        assert_eq!(
            plan(&apps, &MutedApps::default(), &desktop()),
            Plan::Only(vec![31, 41])
        );
    }

    #[test]
    fn a_process_with_a_muted_descendant_is_not_captured() {
        // The launcher (40) plays sound too, and its game (41) is muted: capturing the
        // launcher's tree would include the game.
        let apps = group(&[31, 40, 41], &desktop(), 10, name);
        let muted = MutedApps::new([("game.exe".to_string(), true)]);
        assert_eq!(plan(&apps, &muted, &desktop()), Plan::Only(vec![31]));
    }

    #[test]
    fn a_process_inside_a_captured_tree_is_not_captured_twice() {
        // Chrome's main process (30) and its audio service (31) both have sessions.
        let apps = group(&[21, 30, 31], &desktop(), 10, name);
        assert_eq!(
            plan(&apps, &MutedApps::default(), &desktop()),
            Plan::Only(vec![30])
        );
    }

    #[test]
    fn stale_parent_ids_cannot_loop_forever() {
        let loopy = procs(&[(5, 6, "a.exe"), (6, 5, "b.exe")]);
        assert!(!in_tree(5, 7, &loopy));
        assert!(in_tree(5, 6, &loopy));
    }
}
