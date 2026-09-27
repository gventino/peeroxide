//! Hardware H.265 encoding through Media Foundation: the encoder MFTs that the NVIDIA, AMD and
//! Intel drivers register. There is no software fallback here: without one of these, the
//! broadcast uses H.264 instead (see [`crate::new_encoder`]).
#![allow(unsafe_code)]

use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::sync::OnceLock;
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use anyhow::{Context, bail, ensure};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, Interface, PWSTR};
use yuv::{BufferStoreMut, YuvBiPlanarImageMut, YuvConversionMode, YuvRange, YuvStandardMatrix};

use crate::{Codec, EncodedFrame, Preset, VideoEncoder};

/// How long a frame may take before the driver is considered stuck.
const TIMEOUT: Duration = Duration::from_millis(500);
/// Media Foundation counts time in 100 ns units.
const UNITS_PER_SECOND: i64 = 10_000_000;

/// COM (multithreaded) and Media Foundation, started for as long as this lives.
struct MfRuntime {
    /// COM is only uninitialized by the thread that initialized it.
    com_on: Option<ThreadId>,
}

impl MfRuntime {
    fn start() -> anyhow::Result<Self> {
        // SAFETY: plain initialization calls. `S_FALSE` (already initialized) still needs a
        // matching uninitialize; `RPC_E_CHANGED_MODE` (single-threaded here) doesn't, and Media
        // Foundation works in either.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let com_on = hr.is_ok().then(|| std::thread::current().id());
        // SAFETY: balanced by `MFShutdown` in `drop`.
        let started = unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) };
        let runtime = Self { com_on };
        started.context("could not start Media Foundation")?;
        Ok(runtime)
    }
}

impl Drop for MfRuntime {
    fn drop(&mut self) {
        // SAFETY: balances the successful `MFStartup`/`CoInitializeEx` in `start`.
        unsafe {
            let _ = MFShutdown();
            if self.com_on == Some(std::thread::current().id()) {
                CoUninitialize();
            }
        }
    }
}

/// The hardware H.265 encoders, best first.
fn enumerate() -> anyhow::Result<Vec<IMFActivate>> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_HEVC,
    };
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: the out pointers are valid; the returned array is released below.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut list,
            &mut count,
        )
        .context("could not list the H.265 encoders")?;
        if list.is_null() {
            return Ok(Vec::new());
        }
        let items = std::slice::from_raw_parts_mut(list, count as usize)
            .iter_mut()
            .filter_map(Option::take)
            .collect();
        CoTaskMemFree(Some(list as *const _));
        Ok(items)
    }
}

fn friendly_name(activate: &IMFActivate) -> String {
    let mut text = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: the string is allocated by the callee and freed here after copying.
    unsafe {
        if activate
            .GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut text, &mut len)
            .is_err()
        {
            return "hardware encoder".into();
        }
        let name = text.to_string().unwrap_or_default();
        CoTaskMemFree(Some(text.0 as *const _));
        name
    }
}

/// The name of the GPU's H.265 encoder, if there is one. Only lists encoders (a few
/// milliseconds, cached); whether it really works shows when encoding starts.
pub fn hardware_h265_encoder() -> Option<&'static str> {
    static FOUND: OnceLock<Option<String>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let _runtime = MfRuntime::start().ok()?;
            let first = enumerate().ok()?.into_iter().next()?;
            Some(friendly_name(&first))
        })
        .as_deref()
}

pub struct H265Encoder {
    activate: IMFActivate,
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec_api: Option<ICodecAPI>,
    name: String,
    preset: Preset,
    /// Frame size the MFT was set up for, on the first frame.
    size: Option<(u32, u32)>,
    provides_samples: bool,
    output_size: u32,
    /// Input requests the MFT made that haven't been answered yet.
    need_input: u32,
    /// Encoded frames not returned yet, oldest first. Never dropped: later frames refer to them.
    pending: VecDeque<EncodedFrame>,
    started: Instant,
    want_keyframe: bool,
    /// Last: Media Foundation shuts down after everything above is released.
    _runtime: MfRuntime,
}

// SAFETY: the MFT is created with COM in multithreaded mode, and all its interfaces are used
// through `&mut self`, so from one thread at a time.
unsafe impl Send for H265Encoder {}

impl H265Encoder {
    pub fn new(preset: &Preset) -> anyhow::Result<Self> {
        let runtime = MfRuntime::start()?;
        let mut last_error = None;
        for activate in enumerate()? {
            match Self::activate(&activate) {
                Ok((transform, events, codec_api)) => {
                    let name = friendly_name(&activate);
                    tracing::info!(encoder = %name, "hardware H.265 encoder ready");
                    return Ok(Self {
                        activate,
                        transform,
                        events,
                        codec_api,
                        name,
                        preset: *preset,
                        size: None,
                        provides_samples: true,
                        output_size: 0,
                        need_input: 0,
                        pending: VecDeque::new(),
                        started: Instant::now(),
                        want_keyframe: false,
                        _runtime: runtime,
                    });
                }
                Err(e) => {
                    tracing::debug!(encoder = %friendly_name(&activate), "unusable: {e:#}");
                    last_error = Some(e);
                }
            }
        }
        match last_error {
            Some(e) => Err(e.context("no working hardware H.265 encoder")),
            None => bail!("no hardware H.265 encoder found"),
        }
    }

    fn activate(
        activate: &IMFActivate,
    ) -> anyhow::Result<(IMFTransform, IMFMediaEventGenerator, Option<ICodecAPI>)> {
        // SAFETY: COM calls on valid interfaces.
        unsafe {
            let transform: IMFTransform = activate.ActivateObject().context("activating")?;
            let attributes = transform
                .GetAttributes()
                .context("reading its attributes")?;
            // Hardware MFTs are asynchronous and must be unlocked before use.
            ensure!(
                attributes.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 1,
                "not an asynchronous MFT"
            );
            attributes
                .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                .context("unlocking")?;
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            let events = transform.cast().context("no event generator")?;
            let codec_api = transform.cast().ok();
            Ok((transform, events, codec_api))
        }
    }

    /// Sets a codec property; a property the driver doesn't support is only logged.
    fn set(&self, api: &GUID, value: VARIANT, what: &str) {
        if !self.try_set(api, value) {
            tracing::debug!(encoder = %self.name, "{what} not supported");
        }
    }

    fn try_set(&self, api: &GUID, value: VARIANT) -> bool {
        let Some(codec_api) = &self.codec_api else {
            return false;
        };
        // SAFETY: valid pointers for the duration of the call.
        unsafe { codec_api.SetValue(api, &value) }.is_ok()
    }

    /// Output HEVC, input NV12, both `width`x`height` at the preset's rate and bitrate.
    fn configure(&mut self, width: u32, height: u32) -> anyhow::Result<()> {
        let p = self.preset;
        // Rate control has to be chosen before the output type on some drivers. Low-delay VBR
        // stays under the budget without padding: on the AMD encoder, CBR filled every frame to
        // the full bitrate (scrolling text: 2.0 instead of 1.2 Mbps, for the same quality).
        let vbr = [
            eAVEncCommonRateControlMode_LowDelayVBR,
            eAVEncCommonRateControlMode_PeakConstrainedVBR,
        ];
        if !vbr.iter().any(|mode| {
            self.try_set(
                &CODECAPI_AVEncCommonRateControlMode,
                VARIANT::from(mode.0 as u32),
            )
        }) {
            tracing::debug!(encoder = %self.name, "no VBR mode: using the driver's default");
        }
        self.set(
            &CODECAPI_AVEncCommonMeanBitRate,
            VARIANT::from(p.bitrate_bps),
            "the bitrate",
        );
        self.set(
            &CODECAPI_AVEncCommonMaxBitRate,
            VARIANT::from(p.bitrate_bps),
            "the peak bitrate",
        );
        self.set(
            &CODECAPI_AVLowLatencyMode,
            VARIANT::from(true),
            "low latency",
        );
        self.set(
            &CODECAPI_AVEncMPVDefaultBPictureCount,
            VARIANT::from(0u32),
            "no B-frames",
        );
        self.set(
            &CODECAPI_AVEncMPVGOPSize,
            VARIANT::from(p.fps * 10),
            "the GOP size",
        );

        let size = (u64::from(width) << 32) | u64::from(height);
        let rate = (u64::from(p.fps) << 32) | 1;
        // SAFETY: COM calls on valid interfaces with valid pointers.
        unsafe {
            let output = MFCreateMediaType()?;
            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_HEVC)?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, p.bitrate_bps)?;
            output.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
            output.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
            output.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
            output.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH265VProfile_Main_420_8.0 as u32)?;
            self.transform
                .SetOutputType(0, &output, 0)
                .context("the encoder refused the output format")?;

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
            input.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
            input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            self.transform
                .SetInputType(0, &input, 0)
                .context("the encoder refused NV12 input from memory")?;

            let info = self.transform.GetOutputStreamInfo(0)?;
            self.provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            self.output_size = info.cbSize.max(width * height * 3 / 2);

            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        self.size = Some((width, height));
        Ok(())
    }

    /// The BGRA picture as an NV12 sample (BT.601 limited range, like the decoders assume).
    fn input_sample(
        &self,
        bgra: &[u8],
        width: u32,
        height: u32,
        timestamp: Duration,
    ) -> anyhow::Result<IMFSample> {
        let (w, h) = (width as usize, height as usize);
        let len = w * h * 3 / 2;
        // SAFETY: the locked memory is `len` bytes long and only used while locked.
        unsafe {
            let buffer = MFCreateMemoryBuffer(len as u32)?;
            let mut data = std::ptr::null_mut();
            let mut max = 0u32;
            buffer.Lock(&mut data, Some(&mut max), None)?;
            ensure!(max as usize >= len, "input buffer too small");
            let memory = std::slice::from_raw_parts_mut(data, len);
            let (y, uv) = memory.split_at_mut(w * h);
            let mut image = YuvBiPlanarImageMut {
                y_plane: BufferStoreMut::Borrowed(y),
                y_stride: width,
                uv_plane: BufferStoreMut::Borrowed(uv),
                uv_stride: width,
                width,
                height,
            };
            let converted = yuv::bgra_to_yuv_nv12(
                &mut image,
                &bgra[..w * h * 4],
                width * 4,
                YuvRange::Limited,
                YuvStandardMatrix::Bt601,
                YuvConversionMode::Balanced,
            );
            buffer.Unlock()?;
            converted.context("converting to NV12")?;
            buffer.SetCurrentLength(len as u32)?;

            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            let units = |d: Duration| (d.as_nanos() / 100) as i64;
            sample.SetSampleTime(units(timestamp))?;
            sample.SetSampleDuration(UNITS_PER_SECOND / i64::from(self.preset.fps.max(1)))?;
            Ok(sample)
        }
    }

    /// Waits for the MFT's next event, polling so a stuck driver becomes an error.
    fn next_event(&self, deadline: Instant) -> anyhow::Result<u32> {
        loop {
            // SAFETY: COM call on a valid interface.
            match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => {
                    // SAFETY: COM calls on a valid interface.
                    let (kind, status) = unsafe { (event.GetType()?, event.GetStatus()?) };
                    if kind == MEError.0 as u32 || status.is_err() {
                        bail!(
                            "the H.265 encoder reported an error: {}",
                            windows::core::Error::from(status)
                        );
                    }
                    return Ok(kind);
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    ensure!(
                        Instant::now() < deadline,
                        "the H.265 encoder stopped responding"
                    );
                    std::thread::sleep(Duration::from_micros(250));
                }
                Err(e) => return Err(e).context("waiting for the H.265 encoder"),
            }
        }
    }

    fn take_output(&mut self) -> anyhow::Result<Option<EncodedFrame>> {
        loop {
            // SAFETY: COM calls on valid interfaces; the output buffer's references are taken
            // back out of the `ManuallyDrop`s right after the call so they're released.
            let (result, sample) = unsafe {
                let offered = if self.provides_samples {
                    None
                } else {
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&MFCreateMemoryBuffer(self.output_size)?)?;
                    Some(sample)
                };
                let mut buffer = MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(offered),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                };
                let mut status = 0u32;
                let result =
                    self.transform
                        .ProcessOutput(0, std::slice::from_mut(&mut buffer), &mut status);
                drop(ManuallyDrop::into_inner(buffer.pEvents));
                (result, ManuallyDrop::into_inner(buffer.pSample))
            };
            match result {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // The encoder changed its output format (some do on the first frame).
                    // SAFETY: COM calls on a valid interface.
                    unsafe {
                        let changed = self.transform.GetOutputAvailableType(0, 0)?;
                        self.transform.SetOutputType(0, &changed, 0)?;
                    }
                    continue;
                }
                Err(e) => return Err(e).context("H.265 encoding"),
            }
            let sample = sample.context("the H.265 encoder returned no sample")?;
            // SAFETY: COM calls on a valid sample; the buffer is only read while locked.
            let (mut data, keyframe) = unsafe {
                let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) == 1;
                let buffer = sample.ConvertToContiguousBuffer()?;
                let mut ptr = std::ptr::null_mut();
                let mut len = 0u32;
                buffer.Lock(&mut ptr, None, Some(&mut len))?;
                let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
                buffer.Unlock()?;
                (data, keyframe)
            };
            if keyframe && !has_parameter_sets(&data) {
                // A viewer can only start at a keyframe that carries VPS/SPS/PPS.
                let mut with = self.sequence_header()?;
                with.append(&mut data);
                data = with;
            }
            if data.is_empty() {
                return Ok(None);
            }
            if keyframe {
                self.want_keyframe = false;
            }
            return Ok(Some(EncodedFrame { data, keyframe }));
        }
    }

    /// VPS, SPS and PPS as Annex-B, from the output type.
    fn sequence_header(&self) -> anyhow::Result<Vec<u8>> {
        // SAFETY: COM calls on valid interfaces; the blob is copied into a sized buffer.
        unsafe {
            let output = self.transform.GetOutputCurrentType(0)?;
            let len = output
                .GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER)
                .context("the keyframe has no parameter sets and the encoder offers none")?;
            let mut blob = vec![0u8; len as usize];
            output.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None)?;
            Ok(blob)
        }
    }

    /// Like [`VideoEncoder::encode`] with an explicit timestamp.
    pub(crate) fn encode_at(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
        timestamp: Duration,
    ) -> anyhow::Result<Option<EncodedFrame>> {
        let (w, h) = (width as usize, height as usize);
        ensure!(
            w % 2 == 0 && h % 2 == 0 && w > 0 && h > 0 && bgra.len() >= w * h * 4,
            "invalid frame: {width}x{height} with {} bytes",
            bgra.len()
        );
        match self.size {
            None => self.configure(width, height)?,
            Some(size) => ensure!(size == (width, height), "the frame size changed"),
        }
        let sample = self.input_sample(bgra, width, height, timestamp)?;
        let deadline = Instant::now() + TIMEOUT;

        // A frame goes in when the MFT asks for one; anything it finishes meanwhile is kept.
        while self.need_input == 0 {
            self.handle_event(deadline)?;
        }
        if self.want_keyframe {
            self.set(
                &CODECAPI_AVEncVideoForceKeyFrame,
                VARIANT::from(1u32),
                "forcing a keyframe",
            );
        }
        // SAFETY: COM call on a valid interface and sample.
        unsafe { self.transform.ProcessInput(0, &sample, 0) }.context("H.265 encoding")?;
        self.need_input -= 1;

        // Normally this frame comes out next. An encoder that first wants more input returns
        // it on a later call instead.
        while self.pending.is_empty() && self.need_input == 0 {
            self.handle_event(deadline)?;
        }
        Ok(self.pending.pop_front())
    }

    fn handle_event(&mut self, deadline: Instant) -> anyhow::Result<()> {
        match self.next_event(deadline)? {
            k if k == METransformNeedInput.0 as u32 => self.need_input += 1,
            k if k == METransformHaveOutput.0 as u32 => {
                if let Some(frame) = self.take_output()? {
                    self.pending.push_back(frame);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl VideoEncoder for H265Encoder {
    fn encode(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
    ) -> anyhow::Result<Option<EncodedFrame>> {
        let now = self.started.elapsed();
        self.encode_at(bgra, width, height, now)
    }

    fn request_keyframe(&mut self) {
        self.want_keyframe = true;
    }

    fn codec(&self) -> Codec {
        Codec::H265
    }

    fn describe(&self) -> String {
        format!("H.265 · {}", self.name)
    }
}

impl Drop for H265Encoder {
    fn drop(&mut self) {
        // SAFETY: releases the driver's resources; the interfaces are dropped afterwards.
        unsafe {
            let _ = self.activate.ShutdownObject();
        }
    }
}

/// Whether an Annex-B access unit contains a VPS (NAL type 32), which comes with SPS and PPS.
fn has_parameter_sets(annexb: &[u8]) -> bool {
    annexb
        .windows(4)
        .any(|w| w[0] == 0 && w[1] == 0 && w[2] == 1 && (w[3] >> 1) & 0x3f == 32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{H, PAGE_H, PAGE_W, W, picture, psnr, scrolled, text_page};
    use crate::{H265Decoder, VideoDecoder};

    #[test]
    fn finds_parameter_sets_after_either_start_code() {
        assert!(has_parameter_sets(&[0, 0, 0, 1, 0x40, 0x01, 0x0c]));
        assert!(has_parameter_sets(&[0, 0, 1, 0x40, 0x01]));
        // An IDR slice (type 19) alone has none.
        assert!(!has_parameter_sets(&[0, 0, 0, 1, 0x26, 0x01, 0xaf]));
    }

    fn encoder(preset: &Preset) -> H265Encoder {
        H265Encoder::new(preset).expect("needs a hardware H.265 encoder")
    }

    fn small() -> Preset {
        Preset {
            id: "test",
            name: "test",
            max_width: W,
            max_height: H,
            fps: 30,
            bitrate_bps: 2_000_000,
            audio_bitrate_bps: 64_000,
        }
    }

    /// Encodes `picture(0..frames)` at 30 fps, asking for a keyframe at `keyframe_at`.
    fn encode_pattern(frames: u32, keyframe_at: Option<u32>) -> Vec<Option<EncodedFrame>> {
        let mut enc = encoder(&small());
        (0..frames)
            .map(|n| {
                if keyframe_at == Some(n) {
                    enc.request_keyframe();
                }
                let at = Duration::from_millis(u64::from(n) * 1000 / 30);
                enc.encode_at(&picture(n), W, H, at).unwrap()
            })
            .collect()
    }

    #[test]
    #[ignore = "needs a hardware H.265 encoder; run with --ignored"]
    fn every_frame_comes_out_at_once_and_keyframes_follow_requests() {
        let out = encode_pattern(30, Some(20));
        let kinds: Vec<Option<bool>> = out.iter().map(|f| f.as_ref().map(|f| f.keyframe)).collect();
        assert!(kinds.iter().all(Option::is_some), "{kinds:?}");
        assert_eq!(kinds[0], Some(true));
        assert_eq!(kinds[20], Some(true), "{kinds:?}");
        assert!(
            kinds[1..20]
                .iter()
                .chain(&kinds[21..])
                .all(|k| *k == Some(false)),
            "{kinds:?}"
        );
    }

    /// Writes the H.265 fixture the decoder tests use on every platform (see
    /// `crate::h265::tests`): `picture(0..30)` with a requested keyframe at 20.
    #[test]
    #[ignore = "writes tests/fixtures/pattern-320x180.aus; run with --ignored when it must change"]
    fn write_h265_fixture() {
        let mut file = Vec::new();
        for frame in encode_pattern(30, Some(20)) {
            let frame = frame.expect("frame");
            file.extend_from_slice(&(frame.data.len() as u32).to_le_bytes());
            file.push(u8::from(frame.keyframe));
            file.extend_from_slice(&frame.data);
        }
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/pattern-320x180.aus"
        );
        std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
        std::fs::write(path, file).unwrap();
    }

    #[test]
    #[ignore = "needs a hardware H.265 encoder; run with --ignored"]
    fn roundtrip_through_libde265_preserves_size_and_quality() {
        let mut dec = H265Decoder::new().unwrap();
        for (n, frame) in encode_pattern(15, None).into_iter().enumerate() {
            let frame = frame.expect("frame");
            let out = dec
                .decode(&frame.data)
                .unwrap()
                .expect("a picture per access unit");
            assert_eq!((out.width, out.height), (W, H));
            let q = psnr(&picture(n as u32), &out.rgba);
            assert!(q > 30.0, "frame {n}: psnr {q:.1} dB");
        }
    }

    #[test]
    #[ignore = "needs a hardware H.265 encoder; run with --ignored"]
    fn decoder_can_join_at_a_requested_keyframe() {
        let frames = encode_pattern(12, Some(8));
        let mut dec = H265Decoder::new().unwrap();
        for frame in frames[8..].iter().flatten() {
            assert!(dec.decode(&frame.data).unwrap().is_some());
        }
    }

    /// Ten seconds of scrolling text at the Internet preset: the budget holds, every frame is
    /// still delivered, and the text stays sharp.
    #[test]
    #[ignore = "needs a hardware H.265 encoder; run with --ignored"]
    fn internet_budget_holds_on_scrolling_text() {
        let preset = Preset::INTERNET;
        let page = text_page();
        let mut enc = encoder(&preset);
        let frames = preset.fps * 10;
        let mut bytes = 0;
        let mut dec = H265Decoder::new().unwrap();
        let mut q = 0.0;
        for n in 0..frames {
            let at = Duration::from_secs_f64(f64::from(n) / f64::from(preset.fps));
            let out = enc
                .encode_at(scrolled(&page, n), PAGE_W, PAGE_H, at)
                .unwrap();
            let out = out.expect("no frame may be dropped");
            bytes += out.data.len();
            let pic = dec.decode(&out.data).unwrap().unwrap();
            q += psnr(scrolled(&page, n), &pic.rgba);
        }
        let q = q / f64::from(frames);
        let bps = bytes as f64 * 8.0 / 10.0;
        println!("scrolling text at the Internet preset: {bps:.0} bps, psnr {q:.1} dB");
        let budget = f64::from(preset.bitrate_bps);
        assert!(bps < budget * 1.5, "{bps:.0} bps vs budget {budget:.0}");
        assert!(q > 35.0, "psnr {q:.1} dB");
    }

    /// 1080p60 must encode well within the 16.7 ms a frame has.
    #[test]
    #[ignore = "needs a hardware H.265 encoder; run with --release --ignored --nocapture"]
    fn encodes_1080p60_in_time() {
        let preset = Preset::P1080_60;
        let (w, h) = (1920, 1080);
        let mut enc = encoder(&preset);
        let frames: Vec<Vec<u8>> = (0..4u8)
            .map(|k| {
                (0..w * h * 4)
                    .map(|i| (i as u8).wrapping_mul(k + 1))
                    .collect()
            })
            .collect();
        let started = Instant::now();
        let n = 120u32;
        for i in 0..n {
            let at = Duration::from_secs_f64(f64::from(i) / 60.0);
            enc.encode_at(&frames[i as usize % 4], w, h, at).unwrap();
        }
        let ms = started.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
        println!("1080p H.265 encode ({}): {ms:.2} ms per frame", enc.name);
        assert!(ms < 16.0, "{ms:.2} ms per frame");
    }
}
