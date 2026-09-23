//! Viewer connection state machine (use-cases.md, "Viewer connection state").

use p2pss_net::{Fingerprint, SessionEnd};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerRef {
    pub fingerprint: Fingerprint,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerState {
    Idle,
    Connecting {
        peer: PeerRef,
    },
    Streaming {
        peer: PeerRef,
        broadcaster_name: String,
    },
    Disconnected {
        peer: PeerRef,
        reason: String,
    },
}

#[derive(Clone, Debug)]
pub enum ViewerInput {
    Select(PeerRef),
    Connected { broadcaster_name: String },
    Ended(SessionEnd),
    StopWatching,
    Acknowledge,
}

impl ViewerState {
    pub fn peer(&self) -> Option<&PeerRef> {
        match self {
            Self::Idle => None,
            Self::Connecting { peer }
            | Self::Streaming { peer, .. }
            | Self::Disconnected { peer, .. } => Some(peer),
        }
    }

    /// Whether a session should exist for this state (enforces one session at a time).
    pub fn wants_session(&self) -> bool {
        matches!(self, Self::Connecting { .. } | Self::Streaming { .. })
    }

    pub fn transition(&self, input: ViewerInput) -> Self {
        use ViewerInput as I;
        use ViewerState as S;
        match (self, input) {
            // Selecting while streaming is "switch": Streaming → Idle → Connecting in one step.
            (_, I::Select(peer)) => S::Connecting { peer },
            // The identity is verified by now, so the broadcaster's own name replaces whatever
            // label we had (e.g. just the ID for a pasted connect string).
            (S::Connecting { peer }, I::Connected { broadcaster_name }) => S::Streaming {
                peer: PeerRef {
                    fingerprint: peer.fingerprint,
                    name: broadcaster_name.clone(),
                },
                broadcaster_name,
            },
            (S::Connecting { .. } | S::Streaming { .. }, I::StopWatching) => S::Idle,
            (S::Connecting { .. }, I::Ended(_)) => S::Idle,
            (S::Streaming { peer, .. }, I::Ended(end)) if end != SessionEnd::Closed => {
                S::Disconnected {
                    peer: peer.clone(),
                    reason: describe(&end),
                }
            }
            (S::Disconnected { .. }, I::Acknowledge) => S::Idle,
            (state, _) => state.clone(),
        }
    }
}

pub fn describe(end: &SessionEnd) -> String {
    match end {
        SessionEnd::Closed => "Stopped watching".into(),
        SessionEnd::BroadcastStopped => "The broadcaster stopped sharing".into(),
        SessionEnd::SourceClosed => "The shared window was closed".into(),
        SessionEnd::Busy => "The broadcaster has reached its viewer limit".into(),
        SessionEnd::VersionMismatch => "The broadcaster runs an incompatible version".into(),
        SessionEnd::IdentityMismatch => {
            "Identity check failed: the peer's certificate does not match its announcement \
             (possible impersonation)"
                .into()
        }
        SessionEnd::Unreachable(e) => format!("Could not reach the broadcaster ({e})"),
        SessionEnd::ConnectionLost(e) => format!("Connection lost ({e})"),
        SessionEnd::ProtocolError(e) => format!("Protocol error ({e})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(n: u8) -> PeerRef {
        PeerRef {
            fingerprint: Fingerprint::of(&[n]),
            name: format!("peer-{n}"),
        }
    }

    fn connected() -> ViewerInput {
        ViewerInput::Connected {
            broadcaster_name: "b".into(),
        }
    }

    #[test]
    fn happy_path_matches_the_state_diagram() {
        let s = ViewerState::Idle.transition(ViewerInput::Select(peer(1)));
        assert_eq!(s, ViewerState::Connecting { peer: peer(1) });
        let s = s.transition(connected());
        assert!(matches!(
            &s,
            ViewerState::Streaming { peer: p, .. }
                if p.fingerprint == peer(1).fingerprint && p.name == "b"
        ));
        let s = s.transition(ViewerInput::Ended(SessionEnd::BroadcastStopped));
        assert!(
            matches!(&s, ViewerState::Disconnected { reason, .. } if reason.contains("stopped"))
        );
        assert_eq!(s.transition(ViewerInput::Acknowledge), ViewerState::Idle);
    }

    #[test]
    fn connect_failure_returns_to_idle() {
        let s = ViewerState::Idle
            .transition(ViewerInput::Select(peer(1)))
            .transition(ViewerInput::Ended(SessionEnd::Busy));
        assert_eq!(s, ViewerState::Idle);
    }

    #[test]
    fn switching_goes_straight_to_connecting_the_new_peer() {
        let s = ViewerState::Idle
            .transition(ViewerInput::Select(peer(1)))
            .transition(connected())
            .transition(ViewerInput::Select(peer(2)));
        assert_eq!(s, ViewerState::Connecting { peer: peer(2) });
        assert!(s.wants_session());
    }

    #[test]
    fn stop_watching_returns_to_idle() {
        for s in [
            ViewerState::Idle.transition(ViewerInput::Select(peer(1))),
            ViewerState::Idle
                .transition(ViewerInput::Select(peer(1)))
                .transition(connected()),
        ] {
            assert_eq!(s.transition(ViewerInput::StopWatching), ViewerState::Idle);
        }
    }

    #[test]
    fn own_close_and_stale_inputs_are_ignored() {
        let streaming = ViewerState::Idle
            .transition(ViewerInput::Select(peer(1)))
            .transition(connected());
        assert_eq!(
            streaming.transition(ViewerInput::Ended(SessionEnd::Closed)),
            streaming
        );
        assert_eq!(ViewerState::Idle.transition(connected()), ViewerState::Idle);
        assert_eq!(
            ViewerState::Idle.transition(ViewerInput::Acknowledge),
            ViewerState::Idle
        );
        assert!(!ViewerState::Idle.wants_session());
    }
}
