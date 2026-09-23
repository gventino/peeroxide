//! End-to-end tests over real QUIC on localhost with a synthetic frame publisher.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use bytes::Bytes;
use quinn::{ConnectionError, Endpoint};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::protocol::{ClientMsg, close, write_msg};
use crate::*;

const WAIT: Duration = Duration::from_secs(10);

struct TestServer {
    server: Arc<BroadcastServer>,
    identity: Identity,
    keyframe_requests: Arc<AtomicUsize>,
    publisher: JoinHandle<()>,
}

impl TestServer {
    fn start(max_viewers: usize, frame_size: usize, interval: Duration, tag: u8) -> Self {
        let identity = Identity::generate().unwrap();
        let want_keyframe = Arc::new(AtomicBool::new(true));
        let keyframe_requests = Arc::new(AtomicUsize::new(0));
        let server = Arc::new(
            BroadcastServer::start(
                &identity,
                ServerOptions {
                    name: format!("server-{tag}"),
                    max_viewers,
                    bind: "127.0.0.1:0".parse().unwrap(),
                },
                {
                    let want = want_keyframe.clone();
                    let count = keyframe_requests.clone();
                    move || {
                        want.store(true, SeqCst);
                        count.fetch_add(1, SeqCst);
                    }
                },
            )
            .unwrap(),
        );
        let publisher = tokio::spawn({
            let server = server.clone();
            async move {
                for seq in 0.. {
                    server.publish(VideoFrame {
                        seq,
                        capture_time_us: 0,
                        keyframe: want_keyframe.swap(false, SeqCst),
                        data: Bytes::from(vec![tag; frame_size]),
                    });
                    tokio::time::sleep(interval).await;
                }
            }
        });
        Self {
            server,
            identity,
            keyframe_requests,
            publisher,
        }
    }

    fn simple(tag: u8) -> Self {
        Self::start(8, 1000, Duration::from_millis(10), tag)
    }

    fn addr(&self) -> SocketAddr {
        self.server.local_addr().unwrap()
    }

    async fn wait_viewers(&self, n: usize) {
        let mut rx = self.server.viewers();
        timeout(WAIT, rx.wait_for(|c| *c == n))
            .await
            .unwrap_or_else(|_| panic!("viewer count never reached {n}"))
            .unwrap();
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.publisher.abort();
    }
}

struct TestViewer {
    handle: SessionHandle,
    frames: mpsc::UnboundedReceiver<VideoFrame>,
    events: mpsc::UnboundedReceiver<SessionEvent>,
}

impl TestViewer {
    fn watch(client: &ViewerClient, addr: SocketAddr, fp: Fingerprint) -> Self {
        Self::watch_with(client, addr, fp, |_| {})
    }

    fn watch_with(
        client: &ViewerClient,
        addr: SocketAddr,
        fp: Fingerprint,
        on_frame: impl Fn(&VideoFrame) + Send + 'static,
    ) -> Self {
        let (ftx, frames) = mpsc::unbounded_channel();
        let (etx, events) = mpsc::unbounded_channel();
        let handle = client.watch(
            vec![addr],
            fp,
            "tester".into(),
            move |f| {
                on_frame(&f);
                let _ = ftx.send(f);
            },
            move |_, e| {
                let _ = etx.send(e);
            },
        );
        Self {
            handle,
            frames,
            events,
        }
    }

    async fn event(&mut self) -> SessionEvent {
        timeout(WAIT, self.events.recv())
            .await
            .expect("no session event")
            .expect("event channel closed")
    }

    async fn frame(&mut self) -> VideoFrame {
        timeout(WAIT, self.frames.recv())
            .await
            .expect("no frame")
            .expect("frame channel closed")
    }

    async fn ended(&mut self) -> SessionEnd {
        loop {
            if let SessionEvent::Ended(end) = self.event().await {
                return end;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_viewers_get_ordered_frames_starting_with_a_keyframe() {
    let ts = TestServer::simple(1);
    let client = ViewerClient::new().unwrap();
    let mut viewers = [
        TestViewer::watch(&client, ts.addr(), ts.identity.fingerprint()),
        TestViewer::watch(&client, ts.addr(), ts.identity.fingerprint()),
    ];
    for v in &mut viewers {
        assert!(matches!(
            v.event().await,
            SessionEvent::Connected { broadcaster_name, remote }
                if broadcaster_name == "server-1" && remote == ts.addr()
        ));
        let first = v.frame().await;
        assert!(first.keyframe, "first frame must be a keyframe");
        let mut last = first.seq;
        for _ in 0..10 {
            let f = v.frame().await;
            assert!(f.seq > last, "frames out of order");
            last = f.seq;
        }
    }
    ts.wait_viewers(2).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_fingerprint_is_rejected() {
    let ts = TestServer::simple(1);
    let client = ViewerClient::new().unwrap();
    let impostor = Identity::generate().unwrap().fingerprint();
    let mut v = TestViewer::watch(&client, ts.addr(), impostor);
    assert_eq!(v.ended().await, SessionEnd::IdentityMismatch);
    assert_eq!(*ts.server.viewers().borrow(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_reports_the_reason_to_viewers() {
    for (reason, expected) in [
        (StopReason::Stopped, SessionEnd::BroadcastStopped),
        (StopReason::SourceClosed, SessionEnd::SourceClosed),
    ] {
        let ts = TestServer::simple(1);
        let client = ViewerClient::new().unwrap();
        let mut v = TestViewer::watch(&client, ts.addr(), ts.identity.fingerprint());
        v.frame().await;
        ts.server.stop(reason).await;
        assert_eq!(v.ended().await, expected);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn viewer_limit_rejects_extra_viewers_as_busy() {
    let ts = TestServer::start(1, 1000, Duration::from_millis(10), 1);
    let client = ViewerClient::new().unwrap();
    let mut first = TestViewer::watch(&client, ts.addr(), ts.identity.fingerprint());
    first.frame().await;
    let mut second = TestViewer::watch(&client, ts.addr(), ts.identity.fingerprint());
    assert_eq!(second.ended().await, SessionEnd::Busy);
    first.frame().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_protocol_version_is_refused() {
    let ts = TestServer::simple(1);
    let endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();
    let (config, _) = tls::client_config(ts.identity.fingerprint()).unwrap();
    let conn = endpoint
        .connect_with(config, ts.addr(), "peeroxide.local")
        .unwrap()
        .await
        .unwrap();
    let (mut send, _recv) = conn.open_bi().await.unwrap();
    write_msg(
        &mut send,
        &ClientMsg::Hello {
            version: 999,
            viewer_name: "future".into(),
        },
    )
    .await
    .unwrap();
    match timeout(WAIT, conn.closed()).await.unwrap() {
        ConnectionError::ApplicationClosed(c) => {
            assert_eq!(
                c.error_code.into_inner(),
                u64::from(close::VERSION_MISMATCH)
            )
        }
        other => panic!("unexpected close: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lagging_viewer_skips_ahead_to_a_keyframe() {
    let ts = TestServer::start(8, 64 * 1024, Duration::from_millis(1), 1);
    let client = ViewerClient::new().unwrap();
    let stalled = Arc::new(AtomicBool::new(false));
    let mut v = TestViewer::watch_with(&client, ts.addr(), ts.identity.fingerprint(), {
        let stalled = stalled.clone();
        move |_| {
            // Simulates a viewer that stops reading for a while.
            if !stalled.swap(true, SeqCst) {
                std::thread::sleep(Duration::from_millis(1500));
            }
        }
    });

    let mut prev = v.frame().await;
    loop {
        let f = v.frame().await;
        if f.seq != prev.seq + 1 {
            assert!(
                f.keyframe,
                "after skipping {} frames the next one must be a keyframe",
                f.seq - prev.seq - 1
            );
            break;
        }
        prev = f;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn keyframe_requests_reach_the_broadcaster() {
    let ts = TestServer::simple(1);
    let client = ViewerClient::new().unwrap();
    let mut v = TestViewer::watch(&client, ts.addr(), ts.identity.fingerprint());
    v.frame().await;
    let before = ts.keyframe_requests.load(SeqCst);
    assert!(before >= 1, "joining should request a keyframe");
    v.handle.request_keyframe();
    timeout(WAIT, async {
        while ts.keyframe_requests.load(SeqCst) == before {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("keyframe request never arrived");
}

#[tokio::test(flavor = "multi_thread")]
async fn viewer_count_follows_connects_and_disconnects() {
    let ts = TestServer::simple(1);
    let client = ViewerClient::new().unwrap();
    let mut a = TestViewer::watch(&client, ts.addr(), ts.identity.fingerprint());
    let mut b = TestViewer::watch(&client, ts.addr(), ts.identity.fingerprint());
    a.frame().await;
    b.frame().await;
    ts.wait_viewers(2).await;

    let TestViewer {
        handle, mut events, ..
    } = a;
    drop(handle);
    ts.wait_viewers(1).await;
    let ended = timeout(WAIT, async {
        loop {
            if let Some(SessionEvent::Ended(end)) = events.recv().await {
                return end;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(ended, SessionEnd::Closed);

    drop(b);
    ts.wait_viewers(0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_broadcasters_moves_the_single_session() {
    let a = TestServer::simple(0xA);
    let b = TestServer::simple(0xB);
    let client = ViewerClient::new().unwrap();

    let mut v = TestViewer::watch(&client, a.addr(), a.identity.fingerprint());
    assert_eq!(v.frame().await.data[0], 0xA);
    a.wait_viewers(1).await;

    drop(v);
    let mut v = TestViewer::watch(&client, b.addr(), b.identity.fingerprint());
    assert_eq!(v.frame().await.data[0], 0xB);
    a.wait_viewers(0).await;
    b.wait_viewers(1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connected_reports_the_address_that_answered() {
    let ts = TestServer::simple(1);
    let client = ViewerClient::new().unwrap();
    let dead = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let (etx, mut events) = mpsc::unbounded_channel();
    let _handle = client.watch(
        vec![dead, ts.addr()],
        ts.identity.fingerprint(),
        "tester".into(),
        |_| {},
        move |_, e| {
            let _ = etx.send(e);
        },
    );
    let event = timeout(WAIT, events.recv()).await.unwrap().unwrap();
    assert!(
        matches!(event, SessionEvent::Connected { remote, .. } if remote == ts.addr()),
        "{event:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_broadcaster_is_reported() {
    let client = ViewerClient::new().unwrap();
    let dead: SocketAddr = {
        let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        s.local_addr().unwrap()
    };
    let mut v = TestViewer::watch(&client, dead, Identity::generate().unwrap().fingerprint());
    assert!(matches!(v.ended().await, SessionEnd::Unreachable(_)));
}
