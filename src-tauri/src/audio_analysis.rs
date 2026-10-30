//! A read-only tap of this process's rendered audio, independent of libmpv's
//! persistent playback instance. Windows process loopback never captures a mic
//! or other applications. PCM is bounded in memory and never saved to disk.
use serde::Serialize;
use std::{
    f32::consts::PI,
    sync::{mpsc, Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};
use windows::{
    core::{implement, Interface, Ref, Result as WinResult, HRESULT},
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Media::Audio::{
            ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
            IActivateAudioInterfaceCompletionHandler,
            IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
            AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
            AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
            AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
            AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
            PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX,
        },
        System::{
            Com::{
                CoInitializeEx, CoUninitialize,
                StructuredStorage::{
                    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
                },
                BLOB, COINIT_MULTITHREADED,
            },
            Threading::{CreateEventW, WaitForSingleObject},
            Variant::VT_BLOB,
        },
    },
};

const SAMPLE_RATE: u32 = 48_000;
// 170.7 ms of PCM at 48 kHz; 5.86 Hz FFT-bin spacing. Storage remains
// fixed (64 KiB for the stereo ring); stereo analyses run at most 25 times/sec.
const FFT_SIZE: usize = 16384;
const BANDS: usize = 512;
const LEASE: Duration = Duration::from_millis(750);

/// 512 logarithmic band centres in Hz; left/right are Hann-windowed FFT
/// magnitudes mapped from -80..0 dBFS to 0..1. rmsLeft/rmsRight are linear RMS.
/// Channel data describe the captured signal; a renderer may combine them.
/// A new sequence means new captured PCM. Inactive/unavailable frames are zero.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisSnapshot {
    pub available: bool,
    pub active: bool,
    pub source: &'static str,
    pub status: String,
    pub sequence: u64,
    pub sample_rate: u32,
    pub frequencies: Vec<f32>,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub rms_left: f32,
    pub rms_right: f32,
}
impl Default for AnalysisSnapshot {
    fn default() -> Self {
        Self {
            available: false,
            active: false,
            source: "Windows process loopback",
            status: "inactive".into(),
            sequence: 0,
            sample_rate: SAMPLE_RATE,
            frequencies: (0..BANDS).map(|i| band_edge(i as f32 + 0.5)).collect(),
            left: vec![0.0; BANDS],
            right: vec![0.0; BANDS],
            rms_left: 0.0,
            rms_right: 0.0,
        }
    }
}
impl AnalysisSnapshot {
    fn clear(&mut self) {
        self.active = false;
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.rms_left = 0.0;
        self.rms_right = 0.0;
    }
}

#[derive(Default)]
struct Request {
    deadline: Option<Instant>,
    closing: bool,
}
pub struct AudioAnalysis {
    request: Arc<(Mutex<Request>, Condvar)>,
    snapshot: Arc<Mutex<AnalysisSnapshot>>,
    done: Mutex<Option<mpsc::Receiver<()>>>,
}
impl AudioAnalysis {
    pub fn start() -> Self {
        let request = Arc::new((Mutex::new(Request::default()), Condvar::new()));
        let snapshot = Arc::new(Mutex::new(AnalysisSnapshot::default()));
        let pending = request.clone();
        let frame = snapshot.clone();
        let (done_tx, done_rx) = mpsc::channel();
        if thread::Builder::new()
            .name("musicplayer-spectrum".into())
            .spawn(move || {
                worker(pending, frame);
                let _ = done_tx.send(());
            })
            .is_err()
        {
            snapshot.lock().unwrap().status = "Audio analysis worker unavailable".into();
        }
        Self {
            request,
            snapshot,
            done: Mutex::new(Some(done_rx)),
        }
    }

    /// Polling renews a short lease. The caller must also check real playback
    /// state, so paused/hidden views release capture even after a UI crash.
    pub fn poll(&self, enabled: bool) -> AnalysisSnapshot {
        let (lock, wake) = &*self.request;
        lock.lock().unwrap().deadline = enabled.then(|| Instant::now() + LEASE);
        if !enabled {
            let mut frame = self.snapshot.lock().unwrap();
            frame.clear();
            if frame.available {
                frame.status = "inactive".into();
            }
        }
        wake.notify_one();
        self.snapshot.lock().unwrap().clone()
    }
    pub fn shutdown(&self) {
        let (lock, wake) = &*self.request;
        lock.lock().unwrap().closing = true;
        wake.notify_one();
        if let Some(done) = self.done.lock().unwrap().take() {
            // Activation has its own five-second deadline; normally capture
            // stops within the 15 ms audio wait and releases COM on its worker.
            let _ = done.recv_timeout(Duration::from_secs(6));
        }
    }
}
impl Drop for AudioAnalysis {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    signal: mpsc::Sender<()>,
    // Keep the blob alive even if activation exceeds our wait deadline.
    _activation: Box<AUDIOCLIENT_ACTIVATION_PARAMS>,
}
impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        _operation: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> WinResult<()> {
        let _ = self.signal.send(());
        Ok(())
    }
}

struct Capture {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
}
impl Capture {
    fn open() -> WinResult<Self> {
        let mut activation = Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: std::process::id(),
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        });
        // This PROPVARIANT borrows stack storage; do not PropVariantClear it.
        let params = std::mem::ManuallyDrop::new(PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB,
                    Anonymous: PROPVARIANT_0_0_0 {
                        blob: BLOB {
                            cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                            pBlobData: (&mut *activation as *mut AUDIOCLIENT_ACTIVATION_PARAMS)
                                .cast(),
                        },
                    },
                    ..Default::default()
                }),
            },
        });
        let (sender, receiver) = mpsc::channel();
        let handler: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
            signal: sender,
            _activation: activation,
        }
        .into();
        let operation = unsafe {
            ActivateAudioInterfaceAsync(
                VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
                &IAudioClient::IID,
                Some(&*params),
                &handler,
            )?
        };
        receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| windows::core::Error::from_hresult(HRESULT(0x800705B4u32 as i32)))?;
        let mut result = HRESULT(0);
        let mut interface = None;
        unsafe { operation.GetActivateResult(&mut result, &mut interface)? };
        result.ok()?;
        let client: IAudioClient = interface
            .ok_or_else(|| windows::core::Error::from_hresult(HRESULT(0x80004005u32 as i32)))?
            .cast()?;
        let format = WAVEFORMATEX {
            wFormatTag: 1, // Signed 16-bit PCM, converted by WASAPI.
            nChannels: 2,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
                0,
                0,
                &format,
                None,
            )?;
        }
        let capture = unsafe { client.GetService::<IAudioCaptureClient>()? };
        let event = unsafe { CreateEventW(None, false, false, None)? };
        if let Err(error) = unsafe { client.SetEventHandle(event).and_then(|_| client.Start()) } {
            unsafe {
                let _ = CloseHandle(event);
            }
            return Err(error);
        }
        Ok(Self {
            client,
            capture,
            event,
        })
    }

    fn read(&self, signal: &mut Signal) -> WinResult<bool> {
        let mut received = false;
        // Bound packet draining so control/lease checks always get a turn.
        for _ in 0..32 {
            if unsafe { self.capture.GetNextPacketSize()? } == 0 {
                break;
            }
            let mut data = std::ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            unsafe {
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)?
            };
            if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
                signal.reset();
            }
            // At 48 kHz a packet over one second is invalid for this tap.
            if frames <= SAMPLE_RATE {
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                if silent || !data.is_null() {
                    for i in 0..frames as usize {
                        let (left, right) = if silent {
                            (0.0, 0.0)
                        } else {
                            // WASAPI returns native little-endian packed PCM.
                            unsafe {
                                let samples = data.cast::<i16>();
                                (
                                    samples.add(i * 2).read_unaligned() as f32 / 32768.0,
                                    samples.add(i * 2 + 1).read_unaligned() as f32 / 32768.0,
                                )
                            }
                        };
                        signal.push(left, right);
                    }
                    received = frames > 0;
                }
            }
            unsafe { self.capture.ReleaseBuffer(frames)? };
        }
        Ok(received)
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

fn worker(request: Arc<(Mutex<Request>, Condvar)>, snapshot: Arc<Mutex<AnalysisSnapshot>>) {
    let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if initialized.is_err() {
        snapshot.lock().unwrap().status = "Windows audio initialization unavailable".into();
        return;
    }
    let mut capture: Option<Capture> = None;
    let mut signal = Signal::default();
    let mut retry_at = Instant::now();
    let mut last_pcm = Instant::now();
    let mut last_publish = Instant::now();
    let mut analyzed_generation = 0;
    loop {
        let (lock, wake) = &*request;
        let mut pending = lock.lock().unwrap();
        if pending.closing {
            break;
        }
        let wanted = pending
            .deadline
            .is_some_and(|deadline| deadline > Instant::now());
        if !wanted {
            capture = None;
            signal.reset();
            let mut frame = snapshot.lock().unwrap();
            frame.clear();
            if frame.available {
                frame.status = "inactive".into();
            }
            drop(frame);
            pending = wake
                .wait_timeout(pending, Duration::from_millis(250))
                .unwrap()
                .0;
            drop(pending);
            continue;
        }
        drop(pending);
        if capture.is_none() && Instant::now() >= retry_at {
            snapshot.lock().unwrap().status = "starting".into();
            match Capture::open() {
                Ok(client) => {
                    capture = Some(client);
                    last_pcm = Instant::now();
                    let mut frame = snapshot.lock().unwrap();
                    frame.available = true;
                    frame.status = "capturing".into();
                }
                Err(error) => {
                    let mut frame = snapshot.lock().unwrap();
                    frame.clear();
                    frame.available = false;
                    frame.status = format!(
                        "Process audio capture unavailable ({:#010X}); requires Windows build 20348 or later",
                        error.code().0
                    );
                    retry_at = Instant::now() + Duration::from_secs(5);
                }
            }
        }
        if let Some(client) = &capture {
            unsafe {
                WaitForSingleObject(client.event, 15);
            }
            match client.read(&mut signal) {
                Ok(true) => last_pcm = Instant::now(),
                Ok(false) => {}
                Err(error) => {
                    let mut frame = snapshot.lock().unwrap();
                    frame.clear();
                    frame.available = false;
                    frame.status = format!("Audio capture interrupted ({:#010X})", error.code().0);
                    capture = None;
                    retry_at = Instant::now() + Duration::from_secs(1);
                    signal.reset();
                    continue;
                }
            }
            if last_publish.elapsed() >= Duration::from_millis(40) {
                last_publish = Instant::now();
                // Recheck the lease after the audio wait: a hide/pause can
                // revoke it while capture is in progress.
                let pending = lock.lock().unwrap();
                if pending
                    .deadline
                    .is_none_or(|deadline| deadline <= Instant::now())
                {
                    continue;
                }
                let mut frame = snapshot.lock().unwrap();
                if last_pcm.elapsed() > Duration::from_millis(150) {
                    signal.reset();
                    frame.clear();
                    frame.status = "waiting for audio".into();
                } else if signal.filled >= FFT_SIZE && signal.generation != analyzed_generation {
                    analyzed_generation = signal.generation;
                    signal.analyze(&mut frame);
                    frame.active = true;
                    frame.status = "capturing".into();
                    frame.sequence = frame.sequence.wrapping_add(1);
                }
            }
        } else {
            let pending = lock.lock().unwrap();
            drop(
                wake.wait_timeout(pending, Duration::from_millis(100))
                    .unwrap(),
            );
        }
    }
    drop(capture);
    unsafe {
        CoUninitialize();
    }
}

struct Signal {
    left: [f32; FFT_SIZE],
    right: [f32; FFT_SIZE],
    cursor: usize,
    filled: usize,
    generation: u64,
}
impl Default for Signal {
    fn default() -> Self {
        Self {
            left: [0.0; FFT_SIZE],
            right: [0.0; FFT_SIZE],
            cursor: 0,
            filled: 0,
            generation: 0,
        }
    }
}
impl Signal {
    fn reset(&mut self) {
        *self = Self::default();
    }
    fn push(&mut self, left: f32, right: f32) {
        self.left[self.cursor] = if left.is_finite() { left } else { 0.0 };
        self.right[self.cursor] = if right.is_finite() { right } else { 0.0 };
        self.cursor = (self.cursor + 1) % FFT_SIZE;
        self.filled = (self.filled + 1).min(FFT_SIZE);
        self.generation = self.generation.wrapping_add(1);
    }
    fn analyze(&self, frame: &mut AnalysisSnapshot) {
        frame.rms_left = rms(&self.left);
        frame.rms_right = rms(&self.right);
        frame.left = spectrum(&self.left, self.cursor);
        frame.right = spectrum(&self.right, self.cursor);
    }
}
fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32)
        .sqrt()
        .clamp(0.0, 1.0)
}
fn band_edge(i: f32) -> f32 {
    25.0 * (20_000.0_f32 / 25.0).powf(i / BANDS as f32)
}

fn spectrum(samples: &[f32; FFT_SIZE], cursor: usize) -> Vec<f32> {
    let mut real = [0.0; FFT_SIZE];
    let mut imaginary = [0.0; FFT_SIZE];
    let mean = samples.iter().sum::<f32>() / FFT_SIZE as f32;
    for (i, value) in real.iter_mut().enumerate() {
        let window = 0.5 - 0.5 * (2.0 * PI * i as f32 / (FFT_SIZE - 1) as f32).cos();
        *value = (samples[(cursor + i) % FFT_SIZE] - mean) * window;
    }
    fft(&mut real, &mut imaginary);
    (0..BANDS)
        .map(|band| {
            let lo =
                (band_edge(band as f32) * FFT_SIZE as f32 / SAMPLE_RATE as f32).floor() as usize;
            let hi = (band_edge(band as f32 + 1.0) * FFT_SIZE as f32 / SAMPLE_RATE as f32).ceil()
                as usize;
            let peak = (lo.max(1)..hi.max(lo + 1).min(FFT_SIZE / 2))
                .map(|i| (real[i] * real[i] + imaginary[i] * imaginary[i]).sqrt())
                .fold(0.0_f32, f32::max)
                * 4.0
                / (FFT_SIZE - 1) as f32;
            ((20.0 * peak.max(1.0e-8).log10() + 80.0) / 80.0).clamp(0.0, 1.0)
        })
        .collect()
}

/// In-place radix-2 Cooley-Tukey transform; fixed size keeps work/memory bounded.
fn fft(real: &mut [f32; FFT_SIZE], imaginary: &mut [f32; FFT_SIZE]) {
    let mut j = 0;
    for i in 1..FFT_SIZE {
        let mut bit = FFT_SIZE >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            real.swap(i, j);
            imaginary.swap(i, j);
        }
    }
    let mut length = 2;
    while length <= FFT_SIZE {
        let angle = -2.0 * PI / length as f32;
        let (step_im, step_re) = angle.sin_cos();
        for start in (0..FFT_SIZE).step_by(length) {
            let (mut twiddle_re, mut twiddle_im) = (1.0, 0.0);
            for offset in 0..length / 2 {
                let a = start + offset;
                let b = a + length / 2;
                let re = real[b] * twiddle_re - imaginary[b] * twiddle_im;
                let im = real[b] * twiddle_im + imaginary[b] * twiddle_re;
                real[b] = real[a] - re;
                imaginary[b] = imaginary[a] - im;
                real[a] += re;
                imaginary[a] += im;
                (twiddle_re, twiddle_im) = (
                    twiddle_re * step_re - twiddle_im * step_im,
                    twiddle_re * step_im + twiddle_im * step_re,
                );
            }
        }
        length *= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires Windows process loopback, audio output, and local libmpv"]
    fn native_loopback_reads_real_libmpv_stereo_fft_and_expires_lease() {
        use crate::playback::mpv::{Mpv, LOCAL_AUDIO_OPTIONS};
        use std::{fs, io::Write, path::PathBuf};
        let dll = PathBuf::from(
            std::env::var("MUSICPLAYER_TEST_LIBMPV_PATH")
                .expect("Set MUSICPLAYER_TEST_LIBMPV_PATH"),
        );
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("gapless-assets");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = root.join(format!("spectrum-test-{}-{nonce}.wav", std::process::id()));
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let source_rate = 44_100_u32;
        let frames = source_rate * 10;
        let pcm_bytes = frames * 4;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let _fixture = Fixture(path.clone());
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + pcm_bytes).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16_u32.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&2_u16.to_le_bytes()).unwrap();
        file.write_all(&source_rate.to_le_bytes()).unwrap();
        file.write_all(&(source_rate * 4).to_le_bytes()).unwrap();
        file.write_all(&4_u16.to_le_bytes()).unwrap();
        file.write_all(&16_u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&pcm_bytes.to_le_bytes()).unwrap();
        for i in 0..frames {
            for hz in [1000.0, 4000.0] {
                let sample =
                    ((2.0 * PI * hz * i as f32 / source_rate as f32).sin() * 0.12 * 32767.0) as i16;
                file.write_all(&sample.to_le_bytes()).unwrap();
            }
        }
        drop(file);
        let mut mpv = Mpv::create(Some(&dll)).unwrap();
        for &(key, value) in LOCAL_AUDIO_OPTIONS {
            mpv.set_option(key, value).unwrap();
        }
        mpv.set_option("volume", "20").unwrap();
        mpv.initialize().unwrap();
        let analysis = AudioAnalysis::start();
        analysis.poll(true);
        mpv.command(&["loadfile", path.to_str().unwrap(), "replace"])
            .unwrap();
        mpv.command(&["set", "pause", "no"]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(12);
        let frame = loop {
            mpv.wait_event(0.01);
            let frame = analysis.poll(true);
            if frame.active
                && frame.sequence > 5
                && frame.rms_left > 0.0001
                && frame.rms_right > 0.0001
            {
                break frame;
            }
            assert!(
                Instant::now() < deadline,
                "No real PCM: {}, sequence {}, RMS L/R {}/{}",
                frame.status,
                frame.sequence,
                frame.rms_left,
                frame.rms_right
            );
        };
        eprintln!(
            "PCM sequence {}, RMS L/R {}/{}",
            frame.sequence, frame.rms_left, frame.rms_right
        );
        for (values, hz) in [(&frame.left, 1000.0_f32), (&frame.right, 4000.0_f32)] {
            let band = values
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap()
                .0;
            assert!(
                (frame.frequencies[band] / hz).ln().abs() < 0.16,
                "Expected {hz} Hz, observed {} Hz",
                frame.frequencies[band]
            );
        }
        assert_eq!(
            mpv.property_string("current-tracks/audio/codec")
                .unwrap()
                .as_deref(),
            Some("pcm_s16le")
        );
        assert_eq!(
            mpv.property_string("audio-params/samplerate")
                .unwrap()
                .as_deref(),
            Some("44100")
        );
        assert_eq!(
            mpv.property_string("audio-out-params/samplerate")
                .unwrap()
                .as_deref(),
            Some("48000")
        );
        eprintln!("Real process-loopback: L/R RMS {:.5}/{:.5}, sequence {}, source 44100 Hz -> output 48000 Hz", frame.rms_left, frame.rms_right, frame.sequence);
        thread::sleep(LEASE + Duration::from_millis(250));
        let expired = analysis.snapshot.lock().unwrap().clone();
        assert!(!expired.active, "Capture lease did not expire");
        assert!(expired.left.iter().chain(&expired.right).all(|v| *v == 0.0));
        analysis.poll(true);
        let resume_deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let frame = analysis.poll(true);
            if frame.active && frame.rms_left > 0.00001 {
                break;
            }
            assert!(
                Instant::now() < resume_deadline,
                "Capture did not resume: {}",
                frame.status
            );
            mpv.wait_event(0.01);
        }
        mpv.command(&["stop"]).unwrap();
        let disabled = analysis.poll(false);
        assert!(!disabled.active);
        assert!(disabled
            .left
            .iter()
            .chain(&disabled.right)
            .all(|v| *v == 0.0));
        thread::sleep(Duration::from_millis(100));
        assert!(
            !analysis.snapshot.lock().unwrap().active,
            "Disabled worker republished stale data"
        );
    }
    #[test]
    fn silence_and_dc_have_no_invented_spectrum() {
        assert!(spectrum(&[0.0; FFT_SIZE], 0).iter().all(|x| *x == 0.0));
        assert!(spectrum(&[0.5; FFT_SIZE], 733).iter().all(|x| *x == 0.0));
    }
    #[test]
    fn stereo_fft_resolves_independent_tones_and_rms() {
        let mut signal = Signal::default();
        // Exact FFT bins avoid leakage obscuring the expected dominant band.
        let left_hz = 42.0 * SAMPLE_RATE as f32 / FFT_SIZE as f32;
        let right_hz = 170.0 * SAMPLE_RATE as f32 / FFT_SIZE as f32;
        for i in 0..FFT_SIZE + 317 {
            let t = i as f32 / SAMPLE_RATE as f32;
            signal.push(
                (2.0 * PI * left_hz * t).sin() * 0.5,
                (2.0 * PI * right_hz * t).sin() * 0.25,
            );
        }
        let mut frame = AnalysisSnapshot::default();
        signal.analyze(&mut frame);
        assert!((frame.rms_left - 0.5 / 2.0_f32.sqrt()).abs() < 0.001);
        assert!((frame.rms_right - 0.25 / 2.0_f32.sqrt()).abs() < 0.001);
        for (values, hz) in [(&frame.left, left_hz), (&frame.right, right_hz)] {
            let dominant = values
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap()
                .0;
            assert!((frame.frequencies[dominant] / hz).ln().abs() < 0.13);
            assert!(values
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        }
        assert!(frame.left.iter().copied().fold(0.0_f32, f32::max) > 0.9);
    }

    #[test]
    fn increased_signal_level_increases_rms_and_db_band_amplitude() {
        fn tone(amplitude: f32) -> AnalysisSnapshot {
            let mut signal = Signal::default();
            for i in 0..FFT_SIZE {
                let sample = (2.0 * PI * 171.0 * i as f32 / FFT_SIZE as f32).sin() * amplitude;
                signal.push(sample, sample);
            }
            let mut frame = AnalysisSnapshot::default();
            signal.analyze(&mut frame);
            frame
        }
        let quiet = tone(0.025);
        let loud = tone(0.25);
        assert!((loud.rms_left / quiet.rms_left - 10.0).abs() < 0.001);
        assert!((loud.rms_right / quiet.rms_right - 10.0).abs() < 0.001);
        let peak = |values: &[f32]| values.iter().copied().fold(0.0_f32, f32::max);
        // Tenfold linear amplitude is +20 dB, or +0.25 on the -80..0 dB scale.
        assert!((peak(&loud.left) - peak(&quiet.left) - 0.25).abs() < 0.001);
        assert_eq!(loud.left, loud.right);
        assert_eq!(quiet.left, quiet.right);
        assert_eq!(loud.left.len(), 512);
        assert_eq!(loud.frequencies.len(), 512);
    }

    #[test]
    fn denser_fft_separates_neighboring_tones_in_one_signal() {
        let first_bin = 75;
        let second_bin = 79;
        let first_hz = first_bin as f32 * SAMPLE_RATE as f32 / FFT_SIZE as f32;
        let second_hz = second_bin as f32 * SAMPLE_RATE as f32 / FFT_SIZE as f32;
        assert!(second_hz - first_hz < 24.0);
        let samples = std::array::from_fn(|i| {
            0.25 * (2.0 * PI * first_bin as f32 * i as f32 / FFT_SIZE as f32).sin()
                + 0.25 * (2.0 * PI * second_bin as f32 * i as f32 / FFT_SIZE as f32).sin()
        });
        let bands = spectrum(&samples, 0);
        let band_for = |hz: f32| {
            ((hz / 25.0).ln() / (20_000.0_f32 / 25.0).ln() * BANDS as f32).floor() as usize
        };
        let first_band = band_for(first_hz);
        let second_band = band_for(second_hz);
        assert!(
            second_band > first_band,
            "Close tones collapsed into one output band"
        );
        assert!(bands[first_band] > 0.8);
        assert!(bands[second_band] > 0.8);
        // Check the actual transform too: both peaks stay resolved with a
        // deep valley between them, instead of only relabelling broad bands.
        let mut real = std::array::from_fn(|i| {
            samples[i] * (0.5 - 0.5 * (2.0 * PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
        });
        let mut imaginary = [0.0; FFT_SIZE];
        fft(&mut real, &mut imaginary);
        let magnitude = |bin: usize| (real[bin].powi(2) + imaginary[bin].powi(2)).sqrt();
        assert!(magnitude(first_bin) > 400.0);
        assert!(magnitude(second_bin) > 400.0);
        assert!(magnitude(77) < magnitude(first_bin) * 0.01);
    }
}
