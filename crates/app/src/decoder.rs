//! Viewer side: H.265 or H.264 packets → images in a [`VideoSlot`] for the GUI.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::ColorImage;
use peeroxide_codec::{Codec, DecodedFrame, VideoDecoder, new_decoder};

use crate::stats::Meter;
use peeroxide_net::{VideoCodec, VideoFrame};

const QUEUE: usize = 8;

/// Newest decoded frame, ready to upload, taken by the GUI on its next repaint.
#[derive(Default)]
pub struct VideoSlot(Mutex<Option<ColorImage>>);

impl VideoSlot {
    pub fn put(&self, image: ColorImage) {
        *self.0.lock().unwrap() = Some(image);
    }

    pub fn take(&self) -> Option<ColorImage> {
        self.0.lock().unwrap().take()
    }
}

/// Built on the decoder thread: at 1080p this copy takes about 2 ms, which the UI thread would
/// otherwise spend on every frame.
fn to_image(frame: &DecodedFrame) -> ColorImage {
    ColorImage::from_rgba_premultiplied([frame.width as usize, frame.height as usize], &frame.rgba)
}

#[derive(Default)]
pub struct DecoderStats {
    pub meter: Meter,
    pub dropped: u64,
    pub keyframe_requests: u64,
    /// Capture-to-decoded latency, only meaningful when both ends share a clock (same machine).
    pub latency_ms: Option<f32>,
    /// The codec of the frames being decoded.
    pub codec: Option<Codec>,
}

type Callback = Arc<dyn Fn() + Send + Sync>;

/// `local time − capture time` of the frames being shown, smoothed, in microseconds; audio
/// follows it to stay in sync. Includes the unknown difference between the two clocks.
pub struct VideoOffset(AtomicI64);

impl VideoOffset {
    const NONE: i64 = i64::MIN;

    pub fn get(&self) -> Option<i64> {
        Some(self.0.load(Ordering::Relaxed)).filter(|v| *v != Self::NONE)
    }

    fn record(&self, capture_time_us: u64) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as i64;
        let sample = now - capture_time_us as i64;
        let smoothed = match self.get() {
            Some(old) => old + (sample - old) / 8,
            None => sample,
        };
        self.0.store(smoothed, Ordering::Relaxed);
    }
}

impl Default for VideoOffset {
    fn default() -> Self {
        Self(AtomicI64::new(Self::NONE))
    }
}

pub struct DecoderPipeline {
    tx: Option<SyncSender<VideoFrame>>,
    waiting_for_keyframe: bool,
    resync: Arc<AtomicBool>,
    need_keyframe: Callback,
    pub stats: Arc<Mutex<DecoderStats>>,
    pub video_offset: Arc<VideoOffset>,
    thread: Option<JoinHandle<()>>,
}

impl DecoderPipeline {
    pub fn start(output: Arc<VideoSlot>, on_frame: Callback, need_keyframe: Callback) -> Self {
        let (tx, rx) = mpsc::sync_channel::<VideoFrame>(QUEUE);
        let stats = Arc::new(Mutex::new(DecoderStats::default()));
        let resync = Arc::new(AtomicBool::new(false));
        let video_offset = Arc::new(VideoOffset::default());
        let thread = std::thread::Builder::new()
            .name("decoder".into())
            .spawn({
                let stats = stats.clone();
                let resync = resync.clone();
                let video_offset = video_offset.clone();
                let need_keyframe = need_keyframe.clone();
                move || {
                    // Made for the codec of the first frame, and again whenever it changes.
                    let mut decoder: Option<(Codec, Box<dyn VideoDecoder>)> = None;
                    for packet in rx {
                        let codec = match packet.codec {
                            VideoCodec::H264 => Codec::H264,
                            VideoCodec::H265 => Codec::H265,
                        };
                        let decoder = match &mut decoder {
                            Some((c, d)) if *c == codec => d,
                            slot => match new_decoder(codec) {
                                Ok(d) => {
                                    tracing::info!(codec = codec.name(), "decoding");
                                    stats.lock().unwrap().codec = Some(codec);
                                    &mut slot.insert((codec, d)).1
                                }
                                Err(e) => {
                                    tracing::error!("decoder init failed: {e:#}");
                                    return;
                                }
                            },
                        };
                        let started = Instant::now();
                        match decoder.decode(&packet.data) {
                            Ok(Some(frame)) => {
                                let image = to_image(&frame);
                                let mut s = stats.lock().unwrap();
                                s.meter.record(packet.data.len(), started.elapsed());
                                s.latency_ms = latency_ms(packet.capture_time_us);
                                drop(s);
                                video_offset.record(packet.capture_time_us);
                                output.put(image);
                                on_frame();
                            }
                            Ok(None) => {}
                            Err(e) => {
                                tracing::warn!(seq = packet.seq, "decode error: {e:#}");
                                resync.store(true, Ordering::Relaxed);
                                stats.lock().unwrap().keyframe_requests += 1;
                                need_keyframe();
                            }
                        }
                    }
                }
            })
            .expect("spawn decoder thread");
        Self {
            tx: Some(tx),
            waiting_for_keyframe: true,
            resync,
            need_keyframe,
            stats,
            video_offset,
            thread: Some(thread),
        }
    }

    /// Queues a packet. Never blocks: on overflow or after a decode error, drops until the next keyframe.
    pub fn push(&mut self, packet: VideoFrame) {
        if self.resync.swap(false, Ordering::Relaxed) {
            self.waiting_for_keyframe = true;
        }
        if self.waiting_for_keyframe {
            if !packet.keyframe {
                self.stats.lock().unwrap().dropped += 1;
                return;
            }
            self.waiting_for_keyframe = false;
        }
        let Some(tx) = &self.tx else { return };
        if let Err(TrySendError::Full(_)) = tx.try_send(packet) {
            self.waiting_for_keyframe = true;
            let mut s = self.stats.lock().unwrap();
            s.dropped += 1;
            s.keyframe_requests += 1;
            drop(s);
            (self.need_keyframe)();
        }
    }
}

impl Drop for DecoderPipeline {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn latency_ms(capture_time_us: u64) -> Option<f32> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    let d = now.checked_sub(capture_time_us)?;
    (d < Duration::from_secs(5).as_micros() as u64).then_some(d as f32 / 1000.0)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use peeroxide_codec::{H264Encoder, Preset, VideoEncoder};

    use super::*;

    /// H.265 access units from the codec crate's fixture (see `peeroxide_codec`'s H.265 tests).
    fn h265_units() -> Vec<(bool, Vec<u8>)> {
        let file = include_bytes!("../../codec/tests/fixtures/pattern-320x180.aus");
        let mut rest = &file[..];
        let mut units = Vec::new();
        while !rest.is_empty() {
            let len = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
            units.push((rest[4] == 1, rest[5..5 + len].to_vec()));
            rest = &rest[5 + len..];
        }
        units
    }

    fn frame(seq: u64, codec: VideoCodec, keyframe: bool, data: Vec<u8>) -> VideoFrame {
        VideoFrame {
            seq,
            capture_time_us: 0,
            keyframe,
            codec,
            data: data.into(),
        }
    }

    /// A broadcaster that falls back from H.265 to H.264 mid-stream, and back: the viewer
    /// switches decoders and keeps showing pictures.
    #[test]
    fn follows_the_codec_of_each_frame() {
        let slot = Arc::new(VideoSlot::default());
        let shown = Arc::new(AtomicUsize::new(0));
        let mut pipeline = DecoderPipeline::start(
            slot.clone(),
            {
                let shown = shown.clone();
                Arc::new(move || {
                    shown.fetch_add(1, Ordering::Relaxed);
                })
            },
            Arc::new(|| {}),
        );
        let mut seq = 0;
        let push = |f: VideoFrame, pipeline: &mut DecoderPipeline| {
            pipeline.push(f);
            // Stay under the queue size so nothing is dropped as "lagging".
            std::thread::sleep(Duration::from_millis(5));
        };

        let units = h265_units();
        for (keyframe, data) in &units[..10] {
            push(
                frame(seq, VideoCodec::H265, *keyframe, data.clone()),
                &mut pipeline,
            );
            seq += 1;
        }
        let preset = Preset {
            max_width: 320,
            max_height: 180,
            ..Preset::INTERNET
        };
        let mut h264 = H264Encoder::new(&preset).unwrap();
        let picture = vec![90u8; 320 * 180 * 4];
        for _ in 0..10 {
            let e = h264.encode(&picture, 320, 180).unwrap().unwrap();
            push(
                frame(seq, VideoCodec::H264, e.keyframe, e.data),
                &mut pipeline,
            );
            seq += 1;
        }
        for (keyframe, data) in &units[20..] {
            push(
                frame(seq, VideoCodec::H265, *keyframe, data.clone()),
                &mut pipeline,
            );
            seq += 1;
        }
        let stats = pipeline.stats.clone();
        drop(pipeline);

        let s = stats.lock().unwrap();
        assert_eq!(shown.load(Ordering::Relaxed), 30, "every frame shown");
        assert_eq!((s.dropped, s.keyframe_requests), (0, 0));
        assert_eq!(s.codec, Some(Codec::H265));
        let image = slot.take().expect("a picture");
        assert_eq!(image.size, [320, 180]);
    }
}
