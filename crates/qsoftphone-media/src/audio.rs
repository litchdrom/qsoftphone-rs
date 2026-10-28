//! The sound card: the microphone and the speaker.
//!
//! ## The rates do not match
//!
//! G.711 is 8 kHz, always. A sound card is 44.1 or 48 kHz, almost
//! always — and it will not open at 8 kHz, because the format the
//! driver advertises is the one it wants. So the audio has to be
//! converted twice on every call:
//!
//! * **Capture** — 48 kHz down to 8 kHz, then encoded to G.711.
//! * **Playback** — G.711 decoded to 8 kHz, then up to 48 kHz.
//!
//! Both directions are done by the [`Resampler`] below. The down
//! direction gets a crude low-pass for free: when the ratio is a
//! whole number, the samples of each group are averaged instead of
//! picked, which keeps the aliasing out of the voice band. That is
//! the common case — 48 kHz to 8 kHz is six to one.
//!
//! ## The audio thread
//!
//! `cpal` calls the callbacks on a thread of its own, and that thread
//! is a deadline: if the callback takes longer than the buffer it
//! holds, the sound drops out. So the callbacks do the minimum:
//! convert, and hand the result to a queue. The RTP threads do the
//! rest.
//!
//! ## A machine with no sound card
//!
//! A server, a container, a locked-down desktop — none of them have a
//! microphone. That is not a reason to refuse the call: the RTP still
//! runs, the peer still hears our silence, and the status line says
//! what is missing. So every failure here is a message, not a
//! refusal.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, SampleFormat, Stream, StreamConfig};

use crate::codec::Codec;
use crate::rtp::FRAME_SAMPLES;

/// The rate G.711 runs at.
pub const CODEC_RATE: u32 = 8000;

/// How many frames of audio may pile up on the way out.
///
/// Twenty frames is 400 ms of audio. Beyond that the caller is not
/// keeping up and the old frames are worth less than the new ones.
const MAX_OUTBOUND_FRAMES: usize = 20;

/// How many frames of audio may pile up on the way in.
///
/// A hundred frames is two seconds. A peer that sends faster than we
/// play is either ahead of us or flooding us, and either way the
/// oldest audio is the least useful.
const MAX_INBOUND_FRAMES: usize = 100;

/// The same bound, in samples.
///
/// The queue holds samples, not frames: a frame is 160 of them, and
/// the bound has to be in the unit the queue is measured in.
const MAX_INBOUND_SAMPLES: usize = MAX_INBOUND_FRAMES * FRAME_SAMPLES;

/// What went wrong with the sound card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    /// There is no sound card at all, or none of the ones there are
    /// can be opened.
    NoDevice(String),
    /// The device does not speak a sample format this build can use.
    UnsupportedFormat(String),
    /// The backend refused.
    Backend(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDevice(what) => write!(f, "no audio device: {what}"),
            Self::UnsupportedFormat(what) => write!(f, "unsupported audio format: {what}"),
            Self::Backend(what) => write!(f, "the audio device refused: {what}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// One audio device the user can pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// The name of the device, as the system reports it.
    pub name: String,
    /// Whether the device can record.
    pub is_input: bool,
    /// Whether the device can play.
    pub is_output: bool,
}

/// The devices of this machine.
///
/// A device that is both an input and an output — a headset, say —
/// appears once, with both flags set. That is what the C++'s
/// `AudioDeviceManager` did, and it is what a picker wants.
pub fn devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let Ok(list) = host.devices() else {
        return Vec::new();
    };

    let mut out: Vec<DeviceInfo> = Vec::new();
    for device in list {
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| device.to_string());
        let is_input = device.supports_input();
        let is_output = device.supports_output();
        if !is_input && !is_output {
            continue;
        }

        match out.iter_mut().find(|d| d.name == name) {
            // The same name twice means the same physical device seen
            // through two endpoints — a headset, say. Merge them.
            Some(existing) => {
                existing.is_input |= is_input;
                existing.is_output |= is_output;
            }
            None => out.push(DeviceInfo {
                name,
                is_input,
                is_output,
            }),
        }
    }
    out
}

/// The state the audio threads and the media session share.
#[derive(Debug)]
pub struct AudioState {
    /// The G.711 frames waiting to be sent.
    outbound: Mutex<VecDeque<Vec<u8>>>,
    /// Woken when a frame is queued, or when the session stops.
    outbound_signal: Condvar,
    /// The 8 kHz PCM waiting to be played.
    inbound: Mutex<VecDeque<i16>>,
    /// The microphone is muted.
    mic_muted: AtomicBool,
    /// The speaker is muted.
    speaker_muted: AtomicBool,
    /// The microphone gain, as the bits of an `f32`.
    mic_gain: AtomicU32,
    /// The speaker gain, as the bits of an `f32`.
    speaker_gain: AtomicU32,
}

impl Default for AudioState {
    fn default() -> Self {
        Self::new(1.0, 1.0)
    }
}

impl AudioState {
    /// A state with the given gains.
    pub fn new(mic_gain: f32, speaker_gain: f32) -> Self {
        Self {
            outbound: Mutex::new(VecDeque::new()),
            outbound_signal: Condvar::new(),
            inbound: Mutex::new(VecDeque::new()),
            mic_muted: AtomicBool::new(false),
            speaker_muted: AtomicBool::new(false),
            mic_gain: AtomicU32::new(mic_gain.clamp(0.0, 1.0).to_bits()),
            speaker_gain: AtomicU32::new(speaker_gain.clamp(0.0, 1.0).to_bits()),
        }
    }

    /// Mute or unmute the microphone.
    pub fn set_mic_muted(&self, muted: bool) {
        self.mic_muted.store(muted, Ordering::Relaxed);
    }

    /// Whether the microphone is muted.
    pub fn mic_muted(&self) -> bool {
        self.mic_muted.load(Ordering::Relaxed)
    }

    /// Mute or unmute the speaker.
    pub fn set_speaker_muted(&self, muted: bool) {
        self.speaker_muted.store(muted, Ordering::Relaxed);
    }

    /// Whether the speaker is muted.
    pub fn speaker_muted(&self) -> bool {
        self.speaker_muted.load(Ordering::Relaxed)
    }

    /// The microphone gain, 0.0 to 1.0.
    pub fn mic_gain(&self) -> f32 {
        f32::from_bits(self.mic_gain.load(Ordering::Relaxed))
    }

    /// Set the microphone gain.
    pub fn set_mic_gain(&self, gain: f32) {
        self.mic_gain
            .store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// The speaker gain, 0.0 to 1.0.
    pub fn speaker_gain(&self) -> f32 {
        f32::from_bits(self.speaker_gain.load(Ordering::Relaxed))
    }

    /// Set the speaker gain.
    pub fn set_speaker_gain(&self, gain: f32) {
        self.speaker_gain
            .store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Queue a G.711 frame for sending.
    pub fn push_outbound(&self, frame: Vec<u8>) {
        {
            let mut queue = self.outbound.lock().expect("outbound lock");
            if queue.len() >= MAX_OUTBOUND_FRAMES {
                queue.pop_front();
            }
            queue.push_back(frame);
        }
        // The sender is asleep most of the time; this is what wakes
        // it. It costs a syscall only when there is a waiter.
        self.outbound_signal.notify_one();
    }

    /// Wait for the next frame to send.
    ///
    /// Returns `None` when the wait timed out — the caller looks at
    /// its stop flag and comes back. The timeout is what makes the
    /// sender stoppable: without it the thread would sleep in the
    /// condvar until a frame arrived, and a session that is over
    /// would never release its threads.
    pub(crate) fn wait_for_outbound(&self, timeout: Duration) -> Option<Vec<u8>> {
        let mut queue = self.outbound.lock().expect("outbound lock");
        loop {
            if let Some(frame) = queue.pop_front() {
                return Some(frame);
            }
            let (guard, timed_out) = self
                .outbound_signal
                .wait_timeout(queue, timeout)
                .expect("outbound wait");
            queue = guard;
            if timed_out.timed_out() {
                return None;
            }
        }
    }

    /// Wake the sender up — it is stopping.
    pub(crate) fn wake_sender(&self) {
        self.outbound_signal.notify_all();
    }

    /// Take the next G.711 frame to send, if there is one.
    #[cfg(test)]
    pub(crate) fn pop_outbound(&self) -> Option<Vec<u8>> {
        self.outbound.lock().expect("outbound lock").pop_front()
    }

    /// How many frames are waiting to be sent.
    pub fn outbound_frames(&self) -> usize {
        self.outbound.lock().expect("outbound lock").len()
    }

    /// Queue 8 kHz PCM for playing.
    pub(crate) fn push_inbound(&self, samples: &[i16]) {
        let mut queue = self.inbound.lock().expect("inbound lock");
        for sample in samples {
            if queue.len() >= MAX_INBOUND_SAMPLES {
                // The oldest audio is the least useful: a peer that
                // sends faster than we play is either ahead of us or
                // flooding us, and either way the old frames are worth
                // less than the new ones.
                queue.pop_front();
            }
            queue.push_back(*sample);
        }
    }

    /// Take the next 8 kHz sample to play, if there is one.
    pub(crate) fn pop_inbound(&self) -> Option<i16> {
        self.inbound.lock().expect("inbound lock").pop_front()
    }

    /// Take everything that is waiting to be played.
    pub fn take_inbound(&self) -> Vec<i16> {
        self.inbound
            .lock()
            .expect("inbound lock")
            .drain(..)
            .collect()
    }

    /// How many samples are waiting to be played.
    pub fn inbound_samples(&self) -> usize {
        self.inbound.lock().expect("inbound lock").len()
    }
}

/// Resamples a stream of samples between two rates.
///
/// The state — the read position and the samples not yet consumed —
/// carries across calls, so a stream that arrives in buffers comes out
/// the same as one that did not.
#[derive(Debug)]
pub struct Resampler {
    /// The input samples per output sample.
    step: f64,
    /// The position of the next output, in input samples.
    position: f64,
    /// The input samples not yet consumed.
    pending: VecDeque<f32>,
    /// The index of `pending.front()` in the input stream.
    base: u64,
    /// The finished output samples.
    output: VecDeque<f32>,
    /// The averaging group, when the ratio is a whole number.
    group: Option<Group>,
}

/// The averaging state of a decimating resampler.
#[derive(Debug)]
struct Group {
    /// How many samples are in the group.
    size: usize,
    /// Their sum.
    sum: f32,
    /// How many have arrived.
    count: usize,
}

impl Resampler {
    /// A resampler from `from_rate` to `to_rate`.
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        let step = from_rate as f64 / to_rate as f64;

        // A whole-number ratio that goes *down* is averaged rather
        // than interpolated: the average of six samples is a low-pass
        // filter, and a point sample every sixth one is not.
        let group = if from_rate > to_rate && from_rate % to_rate == 0 {
            Some(Group {
                size: (from_rate / to_rate) as usize,
                sum: 0.0,
                count: 0,
            })
        } else {
            None
        };

        Self {
            step,
            position: 0.0,
            pending: VecDeque::new(),
            base: 0,
            output: VecDeque::new(),
            group,
        }
    }

    /// Feed one input sample.
    pub fn push(&mut self, sample: f32) {
        if let Some(group) = &mut self.group {
            group.sum += sample;
            group.count += 1;
            if group.count == group.size {
                self.output.push_back(group.sum / group.size as f32);
                group.sum = 0.0;
                group.count = 0;
            }
            return;
        }

        self.pending.push_back(sample);
        let index = self.base + self.pending.len() as u64 - 1;

        // An output at `position` interpolates between the samples at
        // `floor(position)` and `floor(position) + 1`, so both have to
        // have arrived.
        while self.position + 1.0 <= index as f64 {
            let floor = self.position.floor();
            let fraction = (self.position - floor) as f32;
            let offset = (floor as i64 - self.base as i64) as usize;

            let first = self.pending[offset];
            let second = self.pending[offset + 1];
            self.output.push_back(first + (second - first) * fraction);
            self.position += self.step;
        }

        // Drop what no output will read any more.
        let keep_from = self.position.floor() as i64;
        while (self.base as i64) < keep_from && !self.pending.is_empty() {
            self.pending.pop_front();
            self.base += 1;
        }
    }

    /// Take the next output sample, if there is one.
    pub fn next_output(&mut self) -> Option<f32> {
        self.output.pop_front()
    }

    /// The number of output samples waiting.
    pub fn pending_outputs(&self) -> usize {
        self.output.len()
    }
}

/// A device sample that can be turned into a normalised float.
pub trait DeviceSample: Copy + Send + 'static {
    /// The sample as a float in -1.0 to 1.0.
    fn to_normalised(self) -> f32;
}

/// A normalised float that can be turned into a device sample.
pub trait FromNormalised: Copy + Send + 'static {
    /// The sample, from a float in -1.0 to 1.0.
    fn from_normalised(value: f32) -> Self;
}

macro_rules! impl_sample_conversion {
    ($($ty:ty => |$v:ident| $to:expr, |$w:ident| $from:expr;)*) => {
        $(impl DeviceSample for $ty {
            fn to_normalised(self) -> f32 {
                let $v = self;
                $to
            }
        }

        impl FromNormalised for $ty {
            fn from_normalised(value: f32) -> Self {
                let $w = value;
                $from
            }
        })*
    };
}

impl_sample_conversion! {
    i8 => |v| v as f32 / 128.0,
          |v| (v * 127.0).round().clamp(-128.0, 127.0) as i8;
    i16 => |v| v as f32 / 32768.0,
          |v| (v * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
    i32 => |v| v as f32 / 2147483648.0,
          |v| (v * 2147483647.0).round().clamp(-2147483648.0, 2147483647.0) as i32;
    i64 => |v| v as f32 / 9223372036854775808.0,
          |v| (v * 9223372036854775807.0).round() as i64;
    u8 => |v| (v as f32 - 128.0) / 128.0,
          |v| (v * 127.0 + 128.0).round().clamp(0.0, 255.0) as u8;
    u16 => |v| (v as f32 - 32768.0) / 32768.0,
          |v| (v * 32767.0 + 32768.0).round().clamp(0.0, 65535.0) as u16;
    u32 => |v| (v as f32 - 2147483648.0) / 2147483648.0,
          |v| (v * 2147483647.0 + 2147483648.0).round().clamp(0.0, 4294967295.0) as u32;
    u64 => |v| ((v as f64 - 9223372036854775808.0) as f32) / 9223372036854775808.0,
          |v| ((v as f64 * 9223372036854775807.0) + 9223372036854775808.0).round() as u64;
    f32 => |v| v,
          |v| v;
    f64 => |v| v as f32,
          |v| v as f64;
}

/// The capture side: the device's samples become G.711 frames.
struct Capture {
    resampler: Resampler,
    encoder: Encoder,
}

/// The playback side: the G.711 frames become the device's samples.
struct Playback {
    resampler: Resampler,
}

/// Accumulates 8 kHz samples into whole G.711 frames.
struct Encoder {
    codec: Codec,
    buffer: [i16; FRAME_SAMPLES],
    filled: usize,
}

impl Encoder {
    fn new(codec: Codec) -> Self {
        Self {
            codec,
            buffer: [0i16; FRAME_SAMPLES],
            filled: 0,
        }
    }

    fn push(&mut self, sample: f32, state: &AudioState) {
        let scaled = (sample * 32767.0).round().clamp(-32768.0, 32767.0);
        self.buffer[self.filled] = scaled as i16;
        self.filled += 1;

        if self.filled == FRAME_SAMPLES {
            // A muted microphone still sends: the RTP clock has to
            // keep moving, or the peer thinks the stream died and
            // tears the call down. What it sends is silence.
            let frame = if state.mic_muted() {
                self.codec.silence_frame(FRAME_SAMPLES)
            } else {
                self.codec.encode(&self.buffer)
            };
            state.push_outbound(frame);
            self.filled = 0;
        }
    }
}

/// The microphone, capturing.
pub struct Microphone {
    /// The stream. It is held and not used: dropping it closes the
    /// device and stops the capture.
    #[allow(dead_code)]
    stream: Stream,
    rate: u32,
    name: String,
}

impl Microphone {
    /// The rate the device is running at.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The name of the device.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The speaker, playing.
pub struct Speaker {
    /// The stream. It is held and not used: dropping it closes the
    /// device and stops the playback.
    #[allow(dead_code)]
    stream: Stream,
    rate: u32,
    name: String,
}

impl Speaker {
    /// The rate the device is running at.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The name of the device.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Find a device by name, or the system default when the name is
/// empty.
fn find_device(host: &Host, name: Option<&str>, input: bool) -> Option<Device> {
    let Some(name) = name.filter(|n| !n.trim().is_empty()) else {
        return if input {
            host.default_input_device()
        } else {
            host.default_output_device()
        };
    };

    let mut list = host.devices().ok()?;
    list.find(|device| {
        let matches = device
            .description()
            .map(|d| d.name() == name)
            .unwrap_or_else(|_| device.to_string() == name);
        if !matches {
            return false;
        }
        if input {
            device.supports_input()
        } else {
            device.supports_output()
        }
    })
}

/// Open the microphone and start capturing.
///
/// The capture starts immediately: the frames land in the outbound
/// queue and the RTP thread picks them up. `state` is where they go.
pub fn open_microphone(
    name: Option<&str>,
    state: Arc<AudioState>,
    codec: Codec,
) -> Result<Microphone, AudioError> {
    let host = cpal::default_host();
    let device = find_device(&host, name, true)
        .ok_or_else(|| AudioError::NoDevice("there is no microphone to open".to_string()))?;

    let supported = device
        .default_input_config()
        .map_err(|e| AudioError::Backend(e.to_string()))?;
    let rate = supported.sample_rate();
    let channels = supported.channels().max(1);
    let config: StreamConfig = supported.config();
    let format = supported.sample_format();
    let device_name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| device.to_string());

    let capture = Capture {
        resampler: Resampler::new(rate, CODEC_RATE),
        encoder: Encoder::new(codec),
    };

    let stream = match format {
        SampleFormat::I8 => build_input::<i8>(&device, config, channels, state.clone(), capture),
        SampleFormat::I16 => build_input::<i16>(&device, config, channels, state.clone(), capture),
        SampleFormat::I32 => build_input::<i32>(&device, config, channels, state.clone(), capture),
        SampleFormat::I64 => build_input::<i64>(&device, config, channels, state.clone(), capture),
        SampleFormat::U8 => build_input::<u8>(&device, config, channels, state.clone(), capture),
        SampleFormat::U16 => build_input::<u16>(&device, config, channels, state.clone(), capture),
        SampleFormat::U32 => build_input::<u32>(&device, config, channels, state.clone(), capture),
        SampleFormat::U64 => build_input::<u64>(&device, config, channels, state.clone(), capture),
        SampleFormat::F32 => build_input::<f32>(&device, config, channels, state.clone(), capture),
        SampleFormat::F64 => build_input::<f64>(&device, config, channels, state.clone(), capture),
        other => {
            return Err(AudioError::UnsupportedFormat(format!(
                "{other} is not a format this build can read"
            )));
        }
    }?;

    stream
        .play()
        .map_err(|e| AudioError::Backend(e.to_string()))?;

    Ok(Microphone {
        stream,
        rate,
        name: device_name,
    })
}

/// Open the speaker and start playing.
pub fn open_speaker(name: Option<&str>, state: Arc<AudioState>) -> Result<Speaker, AudioError> {
    let host = cpal::default_host();
    let device = find_device(&host, name, false)
        .ok_or_else(|| AudioError::NoDevice("there is no speaker to open".to_string()))?;

    let supported = device
        .default_output_config()
        .map_err(|e| AudioError::Backend(e.to_string()))?;
    let rate = supported.sample_rate();
    let config: StreamConfig = supported.config();
    let format = supported.sample_format();
    let device_name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| device.to_string());

    let playback = Playback {
        resampler: Resampler::new(CODEC_RATE, rate),
    };

    let stream = match format {
        SampleFormat::I8 => build_output::<i8>(&device, config, state.clone(), playback),
        SampleFormat::I16 => build_output::<i16>(&device, config, state.clone(), playback),
        SampleFormat::I32 => build_output::<i32>(&device, config, state.clone(), playback),
        SampleFormat::I64 => build_output::<i64>(&device, config, state.clone(), playback),
        SampleFormat::U8 => build_output::<u8>(&device, config, state.clone(), playback),
        SampleFormat::U16 => build_output::<u16>(&device, config, state.clone(), playback),
        SampleFormat::U32 => build_output::<u32>(&device, config, state.clone(), playback),
        SampleFormat::U64 => build_output::<u64>(&device, config, state.clone(), playback),
        SampleFormat::F32 => build_output::<f32>(&device, config, state.clone(), playback),
        SampleFormat::F64 => build_output::<f64>(&device, config, state.clone(), playback),
        other => {
            return Err(AudioError::UnsupportedFormat(format!(
                "{other} is not a format this build can write"
            )));
        }
    }?;

    stream
        .play()
        .map_err(|e| AudioError::Backend(e.to_string()))?;

    Ok(Speaker {
        stream,
        rate,
        name: device_name,
    })
}

/// Build the capture stream for one sample format.
fn build_input<T: DeviceSample + cpal::SizedSample>(
    device: &Device,
    config: StreamConfig,
    channels: u16,
    state: Arc<AudioState>,
    capture: Capture,
) -> Result<Stream, AudioError> {
    // The callback owns the capture state: it is the only thing that
    // touches it, and it has to be a `FnMut`.
    let mut capture = capture;

    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _| {
                let channels = channels.max(1) as usize;
                let frames = data.len() / channels;
                for frame in 0..frames {
                    let mut sum = 0.0f32;
                    for channel in 0..channels {
                        sum += data[frame * channels + channel].to_normalised();
                    }
                    let mono = sum / channels as f32;
                    capture.push(mono, &state);
                }
            },
            |error| {
                tracing::warn!(error = %error, "the microphone reported an error");
            },
            None,
        )
        .map_err(|e| AudioError::Backend(e.to_string()))
}

/// The capture side: the device's samples become G.711 frames.
impl Capture {
    fn push(&mut self, mono: f32, state: &AudioState) {
        self.resampler.push(mono);
        while let Some(value) = self.resampler.next_output() {
            self.encoder.push(value, state);
        }
    }
}

/// Build the playback stream for one sample format.
fn build_output<T: FromNormalised + cpal::SizedSample>(
    device: &Device,
    config: StreamConfig,
    state: Arc<AudioState>,
    playback: Playback,
) -> Result<Stream, AudioError> {
    // The callback owns the playback state, for the same reason.
    let mut playback = playback;

    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                for slot in data.iter_mut() {
                    let value = playback.next(&state);
                    *slot = T::from_normalised(value);
                }
            },
            |error| {
                tracing::warn!(error = %error, "the speaker reported an error");
            },
            None,
        )
        .map_err(|e| AudioError::Backend(e.to_string()))
}

/// The playback side: the G.711 frames become the device's samples.
impl Playback {
    /// The next sample for the speaker.
    ///
    /// A muted speaker still consumes what arrives, so the queue does
    /// not grow without bound while the mute is on.
    fn next(&mut self, state: &AudioState) -> f32 {
        let muted = state.speaker_muted();
        let gain = state.speaker_gain();

        loop {
            if let Some(value) = self.resampler.next_output() {
                return if muted { 0.0 } else { value * gain };
            }
            match state.pop_inbound() {
                Some(sample) => self.resampler.push(sample as f32 / 32768.0),
                None => return 0.0,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resampler_at_the_same_rate_is_a_pass_through() {
        let mut resampler = Resampler::new(8000, 8000);
        for i in 0..10 {
            resampler.push(i as f32);
        }
        let mut out = Vec::new();
        while let Some(value) = resampler.next_output() {
            out.push(value);
        }
        // The first sample cannot be interpolated before the second
        // arrives, so there is one fewer output than inputs.
        assert_eq!(out.len(), 9);
        for (index, value) in out.iter().enumerate() {
            assert!((value - index as f32).abs() < 0.001, "{value} at {index}");
        }
    }

    #[test]
    fn a_resampler_halves_the_rate() {
        let mut resampler = Resampler::new(8000, 4000);
        for i in 0..20 {
            resampler.push(i as f32);
        }
        let mut out = Vec::new();
        while let Some(value) = resampler.next_output() {
            out.push(value);
        }
        // Half the rate, half the samples.
        assert_eq!(out.len(), 10);
        // 8000 to 4000 is a whole number, so the samples of each pair
        // are averaged: that is a low-pass filter.
        assert!((out[0] - 0.5).abs() < 0.001, "{}", out[0]);
        assert!((out[1] - 2.5).abs() < 0.001, "{}", out[1]);
        assert!((out[2] - 4.5).abs() < 0.001, "{}", out[2]);
    }

    #[test]
    fn a_whole_number_downsample_averages() {
        // 48 kHz to 8 kHz is six to one, and the group is averaged:
        // that is a low-pass filter, and it keeps the aliasing out of
        // the voice band.
        let mut resampler = Resampler::new(48000, 8000);
        for _ in 0..6 {
            resampler.push(1.0);
        }
        for _ in 0..6 {
            resampler.push(0.0);
        }
        let mut out = Vec::new();
        while let Some(value) = resampler.next_output() {
            out.push(value);
        }
        assert_eq!(out.len(), 2);
        assert!((out[0] - 1.0).abs() < 0.001, "{}", out[0]);
        assert!((out[1] - 0.0).abs() < 0.001, "{}", out[1]);
    }

    #[test]
    fn a_resampler_six_times_the_rate() {
        let mut resampler = Resampler::new(8000, 48000);
        for i in 0..10 {
            resampler.push(i as f32);
        }
        let mut out = Vec::new();
        while let Some(value) = resampler.next_output() {
            out.push(value);
        }
        // Six outputs per input — but the last one needs a sample
        // that has not arrived yet, so the last sixth of a frame is
        // still waiting. The exact count depends on how the
        // fractional position accumulates, so it is a range.
        assert!((48..=49).contains(&out.len()), "{} outputs", out.len());
        // And the values are the inputs, interpolated.
        assert!((out[0] - 0.0).abs() < 0.001);
        assert!((out[5] - 0.8333).abs() < 0.01, "{}", out[5]);
        assert!((out[6] - 1.0).abs() < 0.001, "{}", out[6]);
    }

    #[test]
    fn a_resampler_keeps_its_state_across_buffers() {
        // One stream of twenty samples, in one buffer and in four, has
        // to come out the same.
        let whole = {
            let mut resampler = Resampler::new(8000, 4000);
            for i in 0..20 {
                resampler.push(i as f32);
            }
            let mut out = Vec::new();
            while let Some(value) = resampler.next_output() {
                out.push(value);
            }
            out
        };

        let split = {
            let mut resampler = Resampler::new(8000, 4000);
            let mut out = Vec::new();
            for chunk in 0..4 {
                for i in 0..5 {
                    resampler.push((chunk * 5 + i) as f32);
                }
                while let Some(value) = resampler.next_output() {
                    out.push(value);
                }
            }
            out
        };

        assert_eq!(whole, split);
    }

    #[test]
    fn the_sample_conversions_round_trip() {
        assert!(((-32768i16).to_normalised() + 1.0).abs() < 0.0001);
        assert!(((32767i16).to_normalised() - 1.0).abs() < 0.0001);
        assert!((0i16).to_normalised().abs() < 0.0001);
        assert!((128u8).to_normalised().abs() < 0.0001);
        assert!(((-1.0f32).to_normalised() + 1.0).abs() < 0.0001);
        assert_eq!(i16::from_normalised(0.0), 0);
        assert_eq!(i16::from_normalised(1.0), 32767);
        assert_eq!(i16::from_normalised(-1.0), -32767);
        assert_eq!(u8::from_normalised(0.0), 128);
        assert_eq!(f32::from_normalised(0.5), 0.5);
    }

    #[test]
    fn the_outbound_queue_is_bounded() {
        let state = AudioState::default();
        for _ in 0..100 {
            state.push_outbound(vec![0; FRAME_SAMPLES]);
        }
        assert_eq!(state.outbound_frames(), MAX_OUTBOUND_FRAMES);
    }

    #[test]
    fn the_inbound_queue_is_bounded() {
        let state = AudioState::default();
        state.push_inbound(&vec![0i16; MAX_INBOUND_SAMPLES + 500]);
        assert_eq!(state.inbound_samples(), MAX_INBOUND_SAMPLES);
    }

    #[test]
    fn a_muted_microphone_sends_silence() {
        let state = AudioState::default();
        let mut encoder = Encoder::new(Codec::Pcma);
        state.set_mic_muted(true);
        for _ in 0..FRAME_SAMPLES {
            encoder.push(0.5, &state);
        }
        let frame = state.pop_outbound().expect("a frame");
        assert_eq!(frame.len(), FRAME_SAMPLES);
        assert!(frame.iter().all(|&b| b == 0xD5), "A-law silence");
    }

    #[test]
    fn a_loud_microphone_sends_the_voice() {
        let state = AudioState::default();
        let mut encoder = Encoder::new(Codec::Pcma);
        for _ in 0..FRAME_SAMPLES {
            encoder.push(0.5, &state);
        }
        let frame = state.pop_outbound().expect("a frame");
        assert_eq!(frame.len(), FRAME_SAMPLES);
        assert!(frame.iter().any(|&b| b != 0xD5), "not silence");
    }

    #[test]
    fn a_muted_speaker_says_nothing() {
        let state = AudioState::default();
        state.set_speaker_muted(true);
        state.push_inbound(&[1000, 1000, 1000]);

        let mut playback = Playback {
            resampler: Resampler::new(CODEC_RATE, 48000),
        };
        assert_eq!(playback.next(&state), 0.0);
        // The samples are still consumed: two of the three.
        assert_eq!(state.inbound_samples(), 1);
    }

    #[test]
    fn the_devices_of_this_machine_can_be_listed() {
        // There may be no sound card at all — that is not a failure,
        // it is a fact about the machine.
        let list = devices();
        for device in &list {
            assert!(!device.name.is_empty());
            assert!(device.is_input || device.is_output);
        }
    }
}
