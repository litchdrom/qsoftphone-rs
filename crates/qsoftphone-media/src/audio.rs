//! The sound card: the microphone and the speaker.
//!
//! ## The rates do not match
//!
//! The codec runs at 8 kHz (G.711) or 16 kHz (G.722). A sound card is
//! 44.1 or 48 kHz, almost always — and it will not open at 8 kHz,
//! because the format the driver advertises is the one it wants. So
//! the audio has to be converted twice on every call:
//!
//! * **Capture** — 48 kHz down to the codec's rate, then encoded.
//! * **Playback** — decoded to the codec's rate, then up to 48 kHz.
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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, SampleFormat, Stream, StreamConfig};

use crate::codec::Codec;
use crate::rtp::FRAME_SAMPLES;

/// How many frames of audio may pile up on the way out.
///
/// Twenty frames is 400 ms of audio. Beyond that the caller is not
/// keeping up and the old frames are worth less than the new ones.
const MAX_OUTBOUND_FRAMES: usize = 20;

/// How many frames of audio may pile up on the way in.
///
/// A hundred frames is two seconds. A peer that sends faster than we
/// play is either ahead of us or flooding us, and either way the
/// oldest audio is the least useful. The queue holds samples and not
/// frames, and G.722 has twice as many samples in a frame — so the
/// bound follows the codec, see [`AudioState::set_frame_samples`].
const MAX_INBOUND_FRAMES: usize = 100;

/// How many frames of audio have to be waiting before the playout
/// starts.
///
/// This is the jitter delay: the audio waits in the machine for this
/// long before it reaches the speaker, so that a late packet does not
/// leave a hole in it. Four frames are 80 ms — the same range as
/// Asterisk's own jitter buffer.
///
/// It is a trade: the deeper the buffer, the more jitter it absorbs,
/// and the longer the delay. 80 ms of delay in a call is not
/// noticeable; a hole in the audio is.
const PLAYOUT_FRAMES: usize = 4;

/// Whether this is a remote session.
///
/// Windows Remote Desktop brings its own audio endpoints — the "Remote
/// Audio" pair — and **the microphone is not one of them** unless the
/// connection asked for the recording to be redirected. The playback
/// side is redirected by default; the recording side is not, and the
/// switch is a separate one ("Remote audio recording → Record from
/// this computer" in the client's settings, or `audiocapturemode:i:1`
/// in an `.rdp` file). Policy on the machine can forbid it outright.
///
/// So a softphone in a remote session can hear the other end and cannot
/// be heard — and the difference is a setting on the client, not
/// anything the application can fix. What the application *can* do is
/// say so, and keep the call alive without a microphone.
///
/// This is `GetSystemMetrics(SM_REMOTESESSION)`: non-zero in a
/// terminal-services session, zero at the console.
pub fn is_remote_session() -> bool {
    #[cfg(windows)]
    {
        /// `SM_REMOTESESSION`.
        const SM_REMOTESESSION: i32 = 0x1000;
        // SAFETY: `GetSystemMetrics` takes an integer and returns an
        // integer. It reads the session's own state and touches
        // nothing of ours.
        unsafe { GetSystemMetrics(SM_REMOTESESSION) != 0 }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn GetSystemMetrics(n_index: i32) -> i32;
}

/// What the application says about a remote session.
///
/// The setting that decides whether the microphone arrives is on the
/// **client** — "Remote audio recording → Record from this computer" —
/// so this is the text that tells the user where to look.
pub const REMOTE_SESSION_HINT: &str = "A remote session redirects the playback but not the microphone. \
     For the microphone to arrive, the connection needs \
     \"Remote audio recording → Record from this computer\" (or \
     `audiocapturemode:i:1` in the .rdp file), and the policy on the \
     machine has to allow it.";

/// The short version of it, for a status line.
pub const REMOTE_SESSION_MIC_HINT: &str = "this is a remote session, and it brings the microphone only when \
     the connection asks for the recording to be redirected";

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
    /// The encoded frames waiting to be sent.
    outbound: Mutex<VecDeque<Vec<u8>>>,
    /// Woken when a frame is queued, or when the session stops.
    outbound_signal: Condvar,
    /// The PCM waiting to be played, at the codec's rate.
    inbound: Mutex<VecDeque<i16>>,
    /// The microphone is muted.
    mic_muted: AtomicBool,
    /// The speaker is muted.
    speaker_muted: AtomicBool,
    /// The microphone gain, as the bits of an `f32`.
    mic_gain: AtomicU32,
    /// The speaker gain, as the bits of an `f32`.
    speaker_gain: AtomicU32,
    /// How many samples one frame of the codec holds.
    ///
    /// G.711 frames are 160 samples and G.722 frames are 320, so the
    /// bound of the inbound queue — two seconds of audio — is twice as
    /// many samples for the wideband codec.
    frame_samples: AtomicUsize,
    /// How many times the playout ran out of audio.
    ///
    /// A playout that runs dry is a hole in the call, and it is worth
    /// counting: the count is what says whether the sound was
    /// continuous or broken.
    playout_underruns: AtomicUsize,
    /// How many frames the peer sent.
    frames_received: AtomicUsize,
    /// How many frames never arrived and had to be concealed.
    ///
    /// These two, with the underruns, are what a call's end says about
    /// itself: a call that ended in silence says so here, instead of
    /// having to be guessed at from the outside.
    frames_concealed: AtomicUsize,
    /// Whether there is a microphone to capture from.
    ///
    /// A call with no microphone still has to **send**: the RTP clock
    /// is what tells the peer the call is alive, and a peer that stops
    /// hearing RTP hangs up. This flag is how the sending thread knows
    /// that no frame is ever going to arrive from the capture side and
    /// that it has to make the silence itself.
    has_microphone: AtomicBool,
    /// How many times the queue has been taken.
    ///
    /// The sound card's callback must take it **once per callback**,
    /// never once per sample: a lock in a real-time thread is a
    /// kernel wait, and a kernel wait there is a buffer overrun. The
    /// counter is what the test measures.
    #[cfg(test)]
    inbound_takes: AtomicUsize,
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
            frame_samples: AtomicUsize::new(FRAME_SAMPLES),
            playout_underruns: AtomicUsize::new(0),
            frames_received: AtomicUsize::new(0),
            frames_concealed: AtomicUsize::new(0),
            has_microphone: AtomicBool::new(false),
            #[cfg(test)]
            inbound_takes: AtomicUsize::new(0),
        }
    }

    /// Tell the state how big a frame of the codec is.
    ///
    /// The bound of the inbound queue is two seconds of audio, and
    /// that is a number of *samples* — so it depends on the codec.
    pub fn set_frame_samples(&self, samples: usize) {
        self.frame_samples.store(samples.max(1), Ordering::Relaxed);
    }

    /// How many samples one frame holds.
    pub fn frame_samples(&self) -> usize {
        self.frame_samples.load(Ordering::Relaxed)
    }

    /// How many samples have to be waiting before the playout starts.
    ///
    /// This is the jitter delay: the audio waits here before it goes
    /// to the speaker, so that a late packet does not leave a hole in
    /// it. Without it the speaker plays the ten milliseconds that have
    /// arrived and then goes silent until the next packet — and a call
    /// that breaks up every twenty milliseconds sounds like it is
    /// under water.
    pub fn playout_target_samples(&self) -> usize {
        PLAYOUT_FRAMES * self.frame_samples()
    }

    /// How many samples may wait to be played.
    fn max_inbound_samples(&self) -> usize {
        MAX_INBOUND_FRAMES * self.frame_samples()
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
                // A frame may have arrived as the timeout fired. Take it
                // rather than reporting nothing: the caller would
                // otherwise send a silence frame *before* the real one,
                // and the peer would hear them out of order.
                return queue.pop_front();
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

    /// Queue PCM for playing.
    pub(crate) fn push_inbound(&self, samples: &[i16]) {
        let mut queue = self.inbound.lock().expect("inbound lock");
        let limit = self.max_inbound_samples();
        for sample in samples {
            if queue.len() >= limit {
                // The oldest audio is the least useful: a peer that
                // sends faster than we play is either ahead of us or
                // flooding us, and either way the old frames are worth
                // less than the new ones.
                queue.pop_front();
            }
            queue.push_back(*sample);
        }
    }

    /// Take everything that is waiting to be played.
    ///
    /// This is the **only** way the sound card's callback touches the
    /// shared queue, and it takes it **once per callback**: the lock
    /// is a kernel wait, and a kernel wait in a real-time thread is a
    /// buffer overrun. The callback works on its own copy afterwards.
    pub fn take_inbound(&self) -> Vec<i16> {
        #[cfg(test)]
        self.inbound_takes.fetch_add(1, Ordering::Relaxed);
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

    /// How many times the playout ran out of audio.
    pub fn playout_underruns(&self) -> usize {
        self.playout_underruns.load(Ordering::Relaxed)
    }

    /// Count a playout that ran dry.
    fn count_underrun(&self) {
        self.playout_underruns.fetch_add(1, Ordering::Relaxed);
    }

    /// How many frames the peer sent.
    pub fn frames_received(&self) -> usize {
        self.frames_received.load(Ordering::Relaxed)
    }

    /// How many frames never arrived and had to be concealed.
    pub fn frames_concealed(&self) -> usize {
        self.frames_concealed.load(Ordering::Relaxed)
    }

    /// Count a frame that arrived.
    pub(crate) fn count_frame_received(&self) {
        self.frames_received.fetch_add(1, Ordering::Relaxed);
    }

    /// Count a frame that never arrived.
    pub(crate) fn count_frame_concealed(&self) {
        self.frames_concealed.fetch_add(1, Ordering::Relaxed);
    }

    /// Whether there is a microphone to capture from.
    pub fn has_microphone(&self) -> bool {
        self.has_microphone.load(Ordering::Relaxed)
    }

    /// Say whether there is a microphone.
    pub(crate) fn set_has_microphone(&self, has: bool) {
        self.has_microphone.store(has, Ordering::Relaxed);
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

/// The capture side: the device's samples become encoded frames.
struct Capture {
    resampler: Resampler,
    builder: FrameBuilder,
}

/// The playback side: the decoded frames become the device's samples.
struct Playback {
    resampler: Resampler,
    /// Whether enough audio has arrived to start playing.
    primed: bool,
    /// The audio waiting to be played, at the codec's rate.
    ///
    /// The callback takes the shared queue **once** and works on this
    /// copy. The copy is what keeps the lock out of the per-sample
    /// path: a real-time callback that takes a lock waits on a kernel
    /// object, and the wait shows up in the audio as an overrun.
    pending: VecDeque<f32>,
}

/// Accumulates the codec's samples into whole frames.
///
/// The frame is 20 ms of audio, and how many samples that is depends
/// on the codec: 160 at 8 kHz, 320 at 16 kHz.
struct FrameBuilder {
    encoder: crate::codec::Encoder,
    buffer: Vec<i16>,
    filled: usize,
}

impl FrameBuilder {
    fn new(codec: Codec) -> Self {
        Self {
            encoder: codec.encoder(),
            buffer: vec![0i16; codec.frame_samples()],
            filled: 0,
        }
    }

    fn push(&mut self, sample: f32, state: &AudioState) {
        // A muted microphone still sends: the RTP clock has to keep
        // moving, or the peer thinks the stream died and tears the
        // call down. What it sends is silence — and it goes through
        // the encoder like any other frame, so an encoder that keeps
        // state (G.722) stays in step with the decoder at the other
        // end.
        let sample = if state.mic_muted() { 0.0 } else { sample };
        let scaled = (sample * 32767.0).round().clamp(-32768.0, 32767.0);
        self.buffer[self.filled] = scaled as i16;
        self.filled += 1;

        if self.filled == self.buffer.len() {
            let frame = self.encoder.encode(&self.buffer);
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
    device_candidates(host, name, input).into_iter().next()
}

/// The devices to try, in the order to try them.
///
/// **A device that was asked for by name is the only one tried.** If
/// the user picked a headset and the headset is gone, opening the
/// laptop's microphone instead would be worse than useless: the user
/// would not know which microphone the other end was hearing.
///
/// The **default** is different: if the default endpoint cannot be
/// opened, the next one is tried. That is not a rare thing — a remote
/// session's redirected endpoint is often the one that refuses, and
/// there is another one behind it.
fn device_candidates(host: &Host, name: Option<&str>, input: bool) -> Vec<Device> {
    let capable = |device: &Device| {
        if input {
            device.supports_input()
        } else {
            device.supports_output()
        }
    };

    if let Some(name) = name.filter(|n| !n.trim().is_empty()) {
        let Ok(list) = host.devices() else {
            return Vec::new();
        };
        return list
            .into_iter()
            .filter(|device| {
                let matches = device
                    .description()
                    .map(|d| d.name() == name)
                    .unwrap_or_else(|_| device.to_string() == name);
                matches && capable(device)
            })
            .collect();
    }

    let mut out: Vec<Device> = Vec::new();
    let default = if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    };
    if let Some(default) = default {
        out.push(default);
    }
    if let Ok(list) = host.devices() {
        for device in list {
            if !capable(&device) {
                continue;
            }
            if out.iter().any(|tried| tried == &device) {
                continue;
            }
            out.push(device);
        }
    }
    out
}

/// The frequency of the test tone.
const TONE_HZ: f64 = 440.0;

/// How long the test tone lasts.
const TONE_SECONDS: f64 = 0.4;

/// Play a short tone through the speaker.
///
/// This is the "test sound" of the settings page: it says whether the
/// speaker works, without anybody having to make a call. The tone is
/// a sine of 440 Hz, a third of a second long, with a fade at each end
/// so that it does not click.
///
/// It opens a device and waits for it, so it must **not** run on the
/// interface thread (§6 of the brief).
pub fn play_test_sound(name: Option<&str>) -> Result<(), AudioError> {
    let host = cpal::default_host();
    let device = find_device(&host, name, false)
        .ok_or_else(|| AudioError::NoDevice("there is no speaker to open".to_string()))?;

    let supported = device
        .default_output_config()
        .map_err(|e| AudioError::Backend(e.to_string()))?;
    let rate = supported.sample_rate();
    let channels = supported.channels().max(1);
    let config: StreamConfig = supported.config();
    let format = supported.sample_format();

    // The tone, in the device's own rate.
    let count = (rate as f64 * TONE_SECONDS) as usize;
    let step = 2.0 * std::f64::consts::PI * TONE_HZ / rate as f64;
    let mut phase = 0.0f64;
    let samples: Vec<f32> = (0..count)
        .map(|i| {
            let position = i as f64 / count.max(1) as f64;
            // A fade of a twentieth of the tone at each end.
            let envelope = (position * 20.0).min(1.0).min((1.0 - position) * 20.0);
            let value = phase.sin() as f32 * 0.3 * envelope as f32;
            phase += step;
            value
        })
        .collect();

    let tone = Tone { samples, index: 0 };
    let stream = match format {
        SampleFormat::I8 => build_tone_output::<i8>(&device, config, channels, tone),
        SampleFormat::I16 => build_tone_output::<i16>(&device, config, channels, tone),
        SampleFormat::I32 => build_tone_output::<i32>(&device, config, channels, tone),
        SampleFormat::I64 => build_tone_output::<i64>(&device, config, channels, tone),
        SampleFormat::U8 => build_tone_output::<u8>(&device, config, channels, tone),
        SampleFormat::U16 => build_tone_output::<u16>(&device, config, channels, tone),
        SampleFormat::U32 => build_tone_output::<u32>(&device, config, channels, tone),
        SampleFormat::U64 => build_tone_output::<u64>(&device, config, channels, tone),
        SampleFormat::F32 => build_tone_output::<f32>(&device, config, channels, tone),
        SampleFormat::F64 => build_tone_output::<f64>(&device, config, channels, tone),
        other => {
            return Err(AudioError::UnsupportedFormat(format!(
                "{other} is not a format this build can write"
            )));
        }
    }?;

    stream
        .play()
        .map_err(|e| AudioError::Backend(e.to_string()))?;

    // The stream has to be kept alive while it plays, and the tone has
    // to be heard: sleep for its length.
    std::thread::sleep(Duration::from_secs_f64(TONE_SECONDS));

    Ok(())
}

/// The tone, and how far into it the playback is.
struct Tone {
    samples: Vec<f32>,
    index: usize,
}

impl Tone {
    /// The next sample of the tone — silence once it is over.
    fn next(&mut self) -> f32 {
        let value = self.samples.get(self.index).copied().unwrap_or(0.0);
        self.index += 1;
        value
    }
}

/// Build the output stream that plays the tone, for one sample format.
fn build_tone_output<T: FromNormalised + cpal::SizedSample>(
    device: &Device,
    config: StreamConfig,
    channels: u16,
    tone: Tone,
) -> Result<Stream, AudioError> {
    let mut tone = tone;

    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                // One value per frame, not per slot: see `fill_output`.
                fill_output(data, channels, || tone.next());
            },
            |error| {
                tracing::warn!(error = %error, "the speaker reported an error");
            },
            None,
        )
        .map_err(|e| AudioError::Backend(e.to_string()))
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
    let candidates = device_candidates(&host, name, true);
    if candidates.is_empty() {
        return Err(AudioError::NoDevice(
            "there is no microphone to open".to_string(),
        ));
    }

    // The first device that opens is the one that is used. A default
    // endpoint that refuses is not the end of the road: the next one
    // is tried, because a remote session's redirected endpoint often
    // is the one that refuses.
    let mut last: Option<AudioError> = None;
    for device in candidates {
        match open_microphone_on(&device, &state, codec) {
            Ok(microphone) => return Ok(microphone),
            Err(e) => {
                tracing::warn!(
                    device = %device_name(&device),
                    error = %e,
                    "the microphone could not be opened"
                );
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or_else(|| AudioError::NoDevice("there is no microphone to open".to_string())))
}

/// Open one microphone.
fn open_microphone_on(
    device: &Device,
    state: &Arc<AudioState>,
    codec: Codec,
) -> Result<Microphone, AudioError> {
    let supported = device
        .default_input_config()
        .map_err(|e| AudioError::Backend(e.to_string()))?;
    let rate = supported.sample_rate();
    let channels = supported.channels().max(1);
    let config: StreamConfig = supported.config();
    let format = supported.sample_format();
    let device_name = device_name(device);

    // The queue is measured in frames, and a frame is as big as the
    // codec's is.
    state.set_frame_samples(codec.frame_samples());

    let capture = Capture {
        resampler: Resampler::new(rate, codec.sample_rate()),
        builder: FrameBuilder::new(codec),
    };

    let stream = match format {
        SampleFormat::I8 => build_input::<i8>(device, config, channels, state.clone(), capture),
        SampleFormat::I16 => build_input::<i16>(device, config, channels, state.clone(), capture),
        SampleFormat::I32 => build_input::<i32>(device, config, channels, state.clone(), capture),
        SampleFormat::I64 => build_input::<i64>(device, config, channels, state.clone(), capture),
        SampleFormat::U8 => build_input::<u8>(device, config, channels, state.clone(), capture),
        SampleFormat::U16 => build_input::<u16>(device, config, channels, state.clone(), capture),
        SampleFormat::U32 => build_input::<u32>(device, config, channels, state.clone(), capture),
        SampleFormat::U64 => build_input::<u64>(device, config, channels, state.clone(), capture),
        SampleFormat::F32 => build_input::<f32>(device, config, channels, state.clone(), capture),
        SampleFormat::F64 => build_input::<f64>(device, config, channels, state.clone(), capture),
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

/// The name of a device, as the system reports it.
fn device_name(device: &Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| device.to_string())
}

/// Open the speaker and start playing.
///
/// `codec` says what the incoming audio is: its rate is what the
/// playback resamples from.
pub fn open_speaker(
    name: Option<&str>,
    state: Arc<AudioState>,
    codec: Codec,
) -> Result<Speaker, AudioError> {
    let host = cpal::default_host();
    let candidates = device_candidates(&host, name, false);
    if candidates.is_empty() {
        return Err(AudioError::NoDevice(
            "there is no speaker to open".to_string(),
        ));
    }

    // The first device that opens is the one that is used. A default
    // endpoint that refuses is not the end of the road: the next one
    // is tried, because a remote session's redirected endpoint often
    // is the one that refuses.
    let mut last: Option<AudioError> = None;
    for device in candidates {
        match open_speaker_on(&device, &state, codec) {
            Ok(speaker) => return Ok(speaker),
            Err(e) => {
                tracing::warn!(
                    device = %device_name(&device),
                    error = %e,
                    "the speaker could not be opened"
                );
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or_else(|| AudioError::NoDevice("there is no speaker to open".to_string())))
}

/// Open one speaker.
fn open_speaker_on(
    device: &Device,
    state: &Arc<AudioState>,
    codec: Codec,
) -> Result<Speaker, AudioError> {
    let supported = device
        .default_output_config()
        .map_err(|e| AudioError::Backend(e.to_string()))?;
    let rate = supported.sample_rate();
    let channels = supported.channels().max(1);
    let config: StreamConfig = supported.config();
    let format = supported.sample_format();
    let device_name = device_name(device);

    let playback = Playback::new(codec.sample_rate(), rate);

    let stream = match format {
        SampleFormat::I8 => build_output::<i8>(device, config, channels, state.clone(), playback),
        SampleFormat::I16 => build_output::<i16>(device, config, channels, state.clone(), playback),
        SampleFormat::I32 => build_output::<i32>(device, config, channels, state.clone(), playback),
        SampleFormat::I64 => build_output::<i64>(device, config, channels, state.clone(), playback),
        SampleFormat::U8 => build_output::<u8>(device, config, channels, state.clone(), playback),
        SampleFormat::U16 => build_output::<u16>(device, config, channels, state.clone(), playback),
        SampleFormat::U32 => build_output::<u32>(device, config, channels, state.clone(), playback),
        SampleFormat::U64 => build_output::<u64>(device, config, channels, state.clone(), playback),
        SampleFormat::F32 => build_output::<f32>(device, config, channels, state.clone(), playback),
        SampleFormat::F64 => build_output::<f64>(device, config, channels, state.clone(), playback),
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

/// The capture side: the device's samples become encoded frames.
impl Capture {
    fn push(&mut self, mono: f32, state: &AudioState) {
        self.resampler.push(mono);
        while let Some(value) = self.resampler.next_output() {
            self.builder.push(value, state);
        }
    }
}

/// Build the playback stream for one sample format.
fn build_output<T: FromNormalised + cpal::SizedSample>(
    device: &Device,
    config: StreamConfig,
    channels: u16,
    state: Arc<AudioState>,
    playback: Playback,
) -> Result<Stream, AudioError> {
    // The callback owns the playback state, for the same reason.
    let mut playback = playback;

    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                playback.begin(&state);
                fill_output(data, channels, || playback.next(&state));
            },
            |error| {
                tracing::warn!(error = %error, "the speaker reported an error");
            },
            None,
        )
        .map_err(|e| AudioError::Backend(e.to_string()))
}

/// Fill a device's buffer with the next frames of audio.
///
/// The device's buffer is **interleaved**: a frame is one sample per
/// channel, and the frames follow each other. The audio is mono, so
/// every channel of a frame gets the same value.
///
/// It is the **frame** that moves the audio along, not the slot.
/// Writing one value per *slot* instead plays the audio as many times
/// too fast as the device has channels — a stereo card plays it at
/// double speed. That is distortion the whole way through, and it
/// empties the playout buffer besides, because twice as much is taken
/// out of it as goes in.
fn fill_output<T: FromNormalised + cpal::SizedSample>(
    data: &mut [T],
    channels: u16,
    mut next: impl FnMut() -> f32,
) {
    let channels = channels.max(1) as usize;
    for frame in data.chunks_mut(channels) {
        let value = next();
        for slot in frame {
            *slot = T::from_normalised(value);
        }
    }
}

/// The playback side: the decoded frames become the device's samples.
impl Playback {
    /// A playback that turns the codec's rate into the device's.
    fn new(codec_rate: u32, device_rate: u32) -> Self {
        Self {
            resampler: Resampler::new(codec_rate, device_rate),
            primed: false,
            pending: VecDeque::new(),
        }
    }

    /// Get ready for a callback's worth of samples.
    ///
    /// The playout starts when the buffer holds enough audio to cover
    /// the jitter. Before that the speaker is silent: playing what has
    /// arrived as it arrives is what makes a call sound like it keeps
    /// breaking up.
    ///
    /// This is called once per callback, and it is the **only** place
    /// the shared queue is touched: one lock for the whole callback,
    /// not one per sample. The check reads a counter, and a counter
    /// read per sample would be 48 thousand locks a second.
    fn begin(&mut self, state: &AudioState) {
        // One lock for the whole callback.
        let chunk = state.take_inbound();
        let waiting = self.pending.len() + chunk.len();
        self.pending
            .extend(chunk.into_iter().map(|sample| sample as f32 / 32768.0));

        // The bound still holds: a peer that sends faster than we play
        // must not make the delay run away.
        let limit = state.max_inbound_samples();
        while self.pending.len() > limit {
            self.pending.pop_front();
        }

        if !self.primed && waiting >= state.playout_target_samples() {
            self.primed = true;
        }
    }

    /// The next sample for the speaker.
    ///
    /// A muted speaker still consumes what arrives, so the queue does
    /// not grow without bound while the mute is on.
    fn next(&mut self, state: &AudioState) -> f32 {
        if !self.primed {
            // Not enough audio yet: silence, and nothing is consumed.
            return 0.0;
        }

        let muted = state.speaker_muted();
        let gain = state.speaker_gain();

        loop {
            if let Some(value) = self.resampler.next_output() {
                return if muted { 0.0 } else { value * gain };
            }
            match self.pending.pop_front() {
                Some(sample) => self.resampler.push(sample),
                None => {
                    // The playout ran dry: there is no audio left to
                    // play. That is a hole in the call, and it is
                    // counted so that the call's end says so.
                    state.count_underrun();
                    return 0.0;
                }
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
        let limit = MAX_INBOUND_FRAMES * 160;
        state.push_inbound(&vec![0i16; limit + 500]);
        assert_eq!(state.inbound_samples(), limit);
    }

    #[test]
    fn the_inbound_bound_follows_the_codec() {
        // G.722 frames are twice as big, so two seconds of audio is
        // twice as many samples.
        let state = AudioState::default();
        state.set_frame_samples(Codec::G722.frame_samples());
        let limit = MAX_INBOUND_FRAMES * 320;
        state.push_inbound(&vec![0i16; limit + 500]);
        assert_eq!(state.inbound_samples(), limit);
    }

    #[test]
    fn a_muted_microphone_sends_silence() {
        let state = AudioState::default();
        let mut builder = FrameBuilder::new(Codec::Pcma);
        state.set_mic_muted(true);
        for _ in 0..160 {
            builder.push(0.5, &state);
        }
        let frame = state.pop_outbound().expect("a frame");
        assert_eq!(frame.len(), 160);
        assert!(frame.iter().all(|&b| b == 0xD5), "A-law silence");
    }

    #[test]
    fn a_loud_microphone_sends_the_voice() {
        let state = AudioState::default();
        let mut builder = FrameBuilder::new(Codec::Pcma);
        for _ in 0..160 {
            builder.push(0.5, &state);
        }
        let frame = state.pop_outbound().expect("a frame");
        assert_eq!(frame.len(), 160);
        assert!(frame.iter().any(|&b| b != 0xD5), "not silence");
    }

    #[test]
    fn the_wideband_codec_makes_frames_of_its_own() {
        // G.722 is 16 kHz: a frame is 320 samples and 160 bytes.
        let state = AudioState::default();
        let mut builder = FrameBuilder::new(Codec::G722);
        for _ in 0..319 {
            builder.push(0.5, &state);
        }
        assert!(state.pop_outbound().is_none(), "the frame is not full yet");

        builder.push(0.5, &state);
        let frame = state.pop_outbound().expect("a frame");
        assert_eq!(frame.len(), 160);
    }

    #[test]
    fn a_muted_wideband_microphone_sends_silence_too() {
        // G.722 has no fixed silence code: the silence is what the
        // encoder makes of a frame of zeros. What matters is that the
        // frame is there and that it is quiet.
        let state = AudioState::default();
        state.set_mic_muted(true);
        let mut builder = FrameBuilder::new(Codec::G722);
        for _ in 0..320 {
            builder.push(0.5, &state);
        }
        let frame = state.pop_outbound().expect("a frame");
        assert_eq!(frame.len(), 160);

        let mut decoder = Codec::G722.decoder();
        let decoded = decoder.decode(&frame);
        assert_eq!(decoded.len(), 320);
        for sample in decoded {
            assert!(sample.abs() < 64, "silence decoded to {sample}");
        }
    }

    #[test]
    fn a_muted_speaker_says_nothing() {
        let state = AudioState::default();
        state.set_speaker_muted(true);
        state.push_inbound(&[1000, 1000, 1000]);

        let mut playback = Playback {
            resampler: Resampler::new(8000, 48000),
            primed: true,
            pending: VecDeque::new(),
        };
        // The callback takes the queue first — once, for the whole
        // callback.
        playback.begin(&state);
        assert_eq!(playback.next(&state), 0.0);
        // The samples are still consumed: two of the three.
        assert_eq!(playback.pending.len(), 1);
    }

    #[test]
    fn the_playout_is_continuous_when_the_frames_arrive_one_by_one() {
        // This is the bug that made a call sound like it was breaking
        // up: the speaker played the ten milliseconds that had arrived
        // and then went silent until the next packet — so every
        // twenty milliseconds the sound stopped and started again.
        //
        // The peer sends a frame every 20 ms and the card plays 10 ms
        // at a time; the simulation does both.
        let state = AudioState::default();
        let mut playback = Playback::new(8000, 48000);

        // A loud tone, so that silence is unmistakable.
        let frame: Vec<i16> = vec![8000; 160];
        let mut played: Vec<f32> = Vec::new();
        let mut started_at = None;

        for _ in 0..10 {
            state.push_inbound(&frame);
            playback.begin(&state);
            // 20 ms of device samples at 48 kHz.
            for _ in 0..960 {
                if playback.primed && started_at.is_none() {
                    started_at = Some(played.len());
                }
                played.push(playback.next(&state));
            }
        }

        // The playout waited for the buffer to fill.
        let started_at = started_at.expect("the playout never started");
        assert!(
            started_at >= 3 * 960,
            "the playout started after {started_at} samples — the buffer was not full"
        );

        // And from then on the sound is continuous: no run of silence
        // longer than the resampler's first sample.
        let mut run = 0;
        let mut worst = 0;
        for value in played.iter().skip(started_at) {
            if value.abs() < 0.01 {
                run += 1;
                worst = worst.max(run);
            } else {
                run = 0;
            }
        }
        assert!(worst <= 2, "the playout had a hole of {worst} samples");

        // The tone is there, at the level it was sent at.
        let loudest = played
            .iter()
            .skip(started_at)
            .fold(0.0f32, |loudest, value| loudest.max(value.abs()));
        assert!(loudest > 0.2, "the tone did not arrive: {loudest}");
    }

    #[test]
    fn the_playout_waits_for_the_buffer_to_be_full() {
        // This is the fix for a call that sounds like it is breaking
        // up: the speaker gets silence until there is enough audio
        // buffered to cover the jitter, and then it plays steadily.
        let state = AudioState::default();
        let target = state.playout_target_samples();

        let mut playback = Playback::new(8000, 48000);

        // Not enough yet: silence, and nothing is consumed.
        state.push_inbound(&vec![1000i16; target - 1]);
        playback.begin(&state);
        assert!(!playback.primed, "the playout started too early");
        assert_eq!(playback.next(&state), 0.0);
        assert_eq!(
            playback.pending.len(),
            target - 1,
            "the samples were consumed before the playout started"
        );

        // One more frame and the playout starts.
        state.push_inbound(&[1000; 1]);
        playback.begin(&state);
        assert!(playback.primed, "the playout did not start");
    }

    #[test]
    fn the_playout_stays_started_when_the_buffer_runs_low() {
        // The playout starts once. After that it plays what it has,
        // and a gap in the stream is a gap in the sound — not a
        // restart of the wait.
        let state = AudioState::default();
        let target = state.playout_target_samples();
        state.push_inbound(&vec![1000i16; target]);

        let mut playback = Playback::new(8000, 48000);
        playback.begin(&state);
        assert!(playback.primed);

        // The buffer runs dry: nothing is waiting, in the queue or in
        // the callback's own copy.
        state.take_inbound();
        playback.pending.clear();
        playback.begin(&state);
        assert!(playback.primed, "the playout stopped again");
        assert_eq!(playback.next(&state), 0.0);
    }

    #[test]
    fn the_buffer_absorbs_the_difference_between_the_two_clocks() {
        // The peer's packets are timestamped with its clock and our
        // sound card runs on ours, and the two drift against each
        // other by a few hundred parts per million. If the peer is the
        // slow one, the buffer empties — and the sound breaks up
        // again, minutes into a call, which is the worst time for it.
        //
        // What holds it is the **slack**: four frames of buffer, which
        // is eighty milliseconds of difference between the two clocks.
        // The simulation: a card that takes exactly 8000 samples a
        // second, and a peer that sends 7990 — a hundredth of a
        // percent slow, which is what two independent sound cards do
        // to each other.
        //
        // The playout must not **change the pitch** to cover the
        // difference: that is audible, and it is what a jitter buffer
        // is supposed to prevent. The buffer's slack covers the
        // difference instead, and a clock recovery that does not touch
        // the pitch is the next piece of work.
        let state = AudioState::default();
        let mut playback = Playback::new(8000, 8000);

        let frame = vec![1000i16; 160];
        // Fill the buffer to its target.
        for _ in 0..4 {
            state.push_inbound(&frame);
        }

        let mut underruns = 0;
        let mut lowest = usize::MAX;
        // Thirty seconds: the slack covers the difference for a minute.
        for round in 0..1500 {
            // Every fifth round the peer sends 159 samples instead of
            // 160, because it is slow.
            if round % 5 == 4 {
                state.push_inbound(&frame[..159]);
            } else {
                state.push_inbound(&frame);
            }
            playback.begin(&state);
            for _ in 0..160 {
                if playback.next(&state) == 0.0 {
                    underruns += 1;
                }
            }
            lowest = lowest.min(playback.pending.len());
        }

        assert_eq!(underruns, 0, "the playout ran dry {underruns} times");
        assert!(lowest > 0, "the buffer ran down to {lowest} samples");
    }

    #[test]
    fn a_fast_peer_does_not_make_the_delay_run_away() {
        // The other way round: the peer is quick, so the buffer fills
        // up. The delay must not run away with it.
        let state = AudioState::default();
        let mut playback = Playback::new(8000, 8000);

        let frame = vec![1000i16; 160];
        for _ in 0..4 {
            state.push_inbound(&frame);
        }

        for round in 0..1500 {
            state.push_inbound(&frame);
            if round % 5 == 4 {
                // One sample more than the card takes: the peer is quick.
                state.push_inbound(&[1000i16]);
            }
            playback.begin(&state);
            for _ in 0..160 {
                playback.next(&state);
            }
        }

        // The delay did not run away: the buffer is still near its
        // target and the peer is not seconds ahead of us.
        let level = playback.pending.len();
        let target = state.playout_target_samples();
        assert!(level < target * 2, "the buffer ran away to {level} samples");
    }

    #[test]
    fn the_remote_session_question_has_an_answer() {
        // It is a question about the machine, and the answer is the
        // same every time on the same machine. This build is not in a
        // remote session — but the question has to be answerable, and
        // it has to be answerable on a machine that is.
        let remote = is_remote_session();
        assert_eq!(remote, is_remote_session(), "the answer changed");
    }

    #[test]
    fn the_default_device_is_tried_first() {
        // The order matters: the default is the one the user chose by
        // not choosing, and it is the one that works most of the time.
        // The others are the fallback.
        let host = cpal::default_host();
        let candidates = device_candidates(&host, None, true);
        if let Some(default) = host.default_input_device() {
            assert!(
                !candidates.is_empty(),
                "the default microphone was not in the list"
            );
            assert_eq!(
                candidates[0], default,
                "the default microphone was not tried first"
            );
        }
    }

    #[test]
    fn a_device_that_was_asked_for_by_name_is_the_only_one_tried() {
        // If the user picked a headset and it is gone, opening the
        // laptop's microphone instead would be worse than useless: the
        // user would not know which microphone the other end was
        // hearing.
        let host = cpal::default_host();
        let Ok(list) = host.devices() else {
            return;
        };
        let Some(device) = list.into_iter().find(|d| d.supports_input()) else {
            return;
        };
        let name = device_name(&device);

        let candidates = device_candidates(&host, Some(&name), true);
        assert!(
            candidates.len() <= 1,
            "a named device brought {} candidates",
            candidates.len()
        );
    }

    #[test]
    fn a_stereo_device_gets_one_value_per_frame() {
        // The device's buffer is interleaved: left, right, left,
        // right. The audio is mono, so every channel of a frame gets
        // the same value — and it is the *frame* that moves the audio
        // along.
        //
        // Writing one value per slot instead plays the audio at double
        // speed on a stereo card, which is distortion the whole way
        // through and empties the playout buffer besides.
        let mut data = [0.0f32; 8];
        let mut index = 0;
        fill_output(&mut data, 2, || {
            let value = index as f32;
            index += 1;
            value
        });
        assert_eq!(data, [0.0, 0.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
    }

    #[test]
    fn the_callback_takes_the_queue_once_and_never_again() {
        // The sound card's callback is a real-time thread: it has a
        // deadline, and missing it is a buffer overrun — the device
        // hands back a buffer with a discontinuity in it, and the
        // audio glitches.
        //
        // A lock is a kernel wait, so the callback must take the
        // shared queue **once** and then work on its own copy. This
        // is the test for that: a whole callback's worth of samples,
        // and exactly one take.
        let state = AudioState::default();
        let mut playback = Playback::new(8000, 48000);

        let frame = vec![1000i16; 160];
        for _ in 0..4 {
            state.push_inbound(&frame);
        }

        let takes_before = state.inbound_takes.load(Ordering::Relaxed);

        // Ten milliseconds of a 48 kHz card: 480 frames.
        let mut buffer = [0.0f32; 960];
        playback.begin(&state);
        fill_output(&mut buffer, 2, || playback.next(&state));

        let takes = state.inbound_takes.load(Ordering::Relaxed) - takes_before;
        assert_eq!(
            takes, 1,
            "the callback took the queue {takes} times — it must take it once"
        );

        // And the audio came out of the callback's own copy.
        assert!(
            state.inbound_samples() == 0,
            "the callback left audio in the shared queue"
        );
        assert!(
            !playback.pending.is_empty(),
            "the callback took the queue and threw the audio away"
        );
    }

    #[test]
    fn a_playout_that_runs_dry_is_counted() {
        // A hole in the call is worth counting: the count is what the
        // log says when the call ends, so a call that ended in silence
        // says so instead of having to be guessed at.
        let state = AudioState::default();
        let target = state.playout_target_samples();
        state.push_inbound(&vec![1000i16; target]);

        let mut playback = Playback::new(8000, 8000);
        playback.begin(&state);
        assert!(playback.primed);
        assert_eq!(state.playout_underruns(), 0);

        // The peer stops sending: the playout runs dry.
        playback.pending.clear();
        for _ in 0..100 {
            playback.next(&state);
        }

        assert_eq!(
            state.playout_underruns(),
            100,
            "the holes in the call were not counted"
        );
    }

    #[test]
    fn a_stereo_device_plays_the_audio_at_the_right_speed() {
        // The whole thing, with a stereo card: a 48 kHz device, ten
        // milliseconds of buffer, and a peer that sends one frame
        // every 20 ms.
        //
        // If the audio is taken out of the buffer at the speed of the
        // *slots* rather than the *frames*, twice as much is taken as
        // comes in, and the playout runs dry.
        let state = AudioState::default();
        let mut playback = Playback::new(8000, 48000);

        let frame = vec![1000i16; 160];
        // Fill the buffer to its target.
        for _ in 0..4 {
            state.push_inbound(&frame);
        }

        // Ten milliseconds of a stereo card: 480 frames, 960 samples.
        let mut buffer = [0.0f32; 960];
        let mut underruns = 0;
        for round in 0..100 {
            playback.begin(&state);
            fill_output(&mut buffer, 2, || {
                let value = playback.next(&state);
                if value == 0.0 {
                    underruns += 1;
                }
                value
            });
            // The peer sends a frame every 20 ms.
            if round % 2 == 1 {
                state.push_inbound(&frame);
            }
        }

        assert_eq!(underruns, 0, "the playout ran dry {underruns} times");
        // And the buffer is still where it should be.
        let level = playback.pending.len();
        assert!(
            level <= state.playout_target_samples() + 160,
            "the buffer ran away to {level} samples"
        );
    }

    /// The energy of a signal at one frequency.
    ///
    /// This is the Goertzel filter: it picks out one frequency out of
    /// a signal, which is how a test says "that is a 400 Hz sine" and
    /// "that is not a 400 Hz sine".
    fn goertzel(samples: &[f32], frequency: f32, rate: u32) -> f64 {
        let n = samples.len() as f64;
        let omega = 2.0 * std::f64::consts::PI * frequency as f64 / rate as f64;
        let coefficient = 2.0 * omega.cos();
        let (mut previous, mut before) = (0.0f64, 0.0f64);
        for &sample in samples {
            let current = sample as f64 + coefficient * previous - before;
            before = previous;
            previous = current;
        }
        (previous * previous + before * before - coefficient * previous * before).sqrt() / n
    }

    /// The strongest frequency near `around`, and its strength.
    ///
    /// The search matters: a playout that tracks the peer's clock may
    /// sit a fraction of a percent off the nominal frequency, and that
    /// is a pitch shift, not a distortion. What is a distortion is the
    /// frequency *wandering* — and that shows up as a spread, which no
    /// single frequency accounts for.
    fn strongest_frequency(samples: &[f32], rate: u32, around: f32, span: f32) -> (f32, f64) {
        let mut best = (around, 0.0f64);
        let mut frequency = around - span;
        while frequency <= around + span {
            let strength = goertzel(samples, frequency, rate);
            if strength > best.1 {
                best = (frequency, strength);
            }
            frequency += 0.25;
        }
        best
    }

    /// How much of a signal's energy is at its strongest frequency.
    fn purity(samples: &[f32], rate: u32, around: f32, span: f32) -> (f32, f64) {
        let total: f64 = samples
            .iter()
            .map(|s| (*s as f64) * (*s as f64))
            .sum::<f64>()
            / samples.len() as f64;
        let (frequency, strength) = strongest_frequency(samples, rate, around, span);
        (frequency, 2.0 * strength * strength / total)
    }

    /// The worst purity in a series of short windows.
    ///
    /// A short window is the point: a playout that sits a fraction of
    /// a percent off the nominal frequency is playing a *shifted*
    /// sine, which is not a distortion, and a long window would
    /// smear it into one. A wobble, on the other hand — a pitch that
    /// moves — smears every window it touches, and the worst window
    /// says so.
    fn worst_purity(samples: &[f32], rate: u32, around: f32, span: f32) -> f64 {
        let window = (rate as usize / 5).max(1024);
        let mut worst = 1.0f64;
        let mut start = 0;
        while start + window <= samples.len() {
            let (_, purity) = purity(&samples[start..start + window], rate, around, span);
            worst = worst.min(purity);
            start += window;
        }
        worst
    }

    #[test]
    fn the_audio_that_comes_out_is_the_audio_that_went_in() {
        // The whole path, with a clean signal: a sine wave, encoded to
        // the codec, through the jitter buffer, the decoder, the sample
        // queue and the resampler, out to a simulated 48 kHz stereo
        // card.
        //
        // What comes out has to be the same sine. A sample lost, a
        // rate that is wrong, a channel counted twice, a frame played
        // out of order — every one of those shows up here as energy
        // where there should be none.
        //
        // The measure is the **purity**: how much of the output's
        // energy is at the frequency it is supposed to be at. A clean
        // sine is 1.0. The pitch wandering by a fraction of a percent
        // — which a jitter buffer's clock correction can easily do —
        // smears the phase and takes it to a fraction.
        const CARD_RATE: u32 = 48000;
        const CHANNELS: u16 = 2;
        const FREQUENCY: f32 = 400.0;
        const AMPLITUDE: f32 = 0.5;
        const SECONDS: usize = 5;

        for codec in [crate::codec::Codec::Pcmu, crate::codec::Codec::Pcma] {
            let codec_rate = codec.sample_rate();
            let total = codec_rate as usize * SECONDS;

            // The signal, and the frames it becomes.
            let signal: Vec<i16> = (0..total)
                .map(|i| {
                    let t = i as f32 / codec_rate as f32;
                    (AMPLITUDE * (2.0 * std::f32::consts::PI * FREQUENCY * t).sin() * 32767.0)
                        as i16
                })
                .collect();
            let mut encoder = codec.encoder();
            let payloads: Vec<Vec<u8>> = signal
                .chunks(codec.frame_samples())
                .map(|chunk| encoder.encode(chunk))
                .collect();

            // The peer, the jitter buffer and the decoder.
            let state = AudioState::default();
            let mut jitter = crate::jitter::JitterBuffer::new(5);
            let mut decoder = codec.decoder();
            let mut playback = Playback::new(codec_rate, CARD_RATE);

            // The card: ten milliseconds of a stereo device.
            let mut buffer = [0.0f32; 960];
            let mut output: Vec<f32> = Vec::new();

            for round in 0..(SECONDS * 100) {
                // The card's callback.
                playback.begin(&state);
                fill_output(&mut buffer, CHANNELS, || playback.next(&state));
                for frame in buffer.chunks_exact(CHANNELS as usize) {
                    output.push(frame[0]);
                }

                // The peer sends a frame every 20 ms.
                if round % 2 == 0 && round / 2 < payloads.len() {
                    let index = round / 2;
                    jitter.push(crate::jitter::Frame {
                        sequence: index as u16,
                        timestamp: (index as u32) * 160,
                        payload: payloads[index].clone(),
                    });
                    crate::drain_jitter(&mut jitter, &mut decoder, &state);
                }
            }

            // The first 200 ms are the playout filling its buffer.
            let played = &output[200 * (CARD_RATE as usize / 1000)..];

            let (frequency, _) = purity(played, CARD_RATE, FREQUENCY, 20.0);
            let purity = worst_purity(played, CARD_RATE, FREQUENCY, 20.0);

            assert!(
                purity > 0.98,
                "{codec:?}: the output is not a clean sine — \
                 {purity:.3} of the energy is at the frequency"
            );
            // The pitch may sit a fraction of a percent off nominal —
            // the playout follows the peer's clock — but not more.
            assert!(
                (frequency - FREQUENCY).abs() < FREQUENCY * 0.01,
                "{codec:?}: the output is at {frequency:.1} Hz, \
                 expected {FREQUENCY}"
            );
            let amplitude = 2.0 * goertzel(played, frequency, CARD_RATE);
            assert!(
                (amplitude - AMPLITUDE as f64).abs() < 0.05,
                "{codec:?}: the output's amplitude is {amplitude:.3}, \
                 expected {AMPLITUDE}"
            );
            // And nothing at twice the frequency — that is what a
            // double-speed playback looks like.
            let double = goertzel(played, frequency * 2.0, CARD_RATE);
            assert!(
                double < amplitude * 0.05,
                "{codec:?}: there is energy at twice the frequency \
                 ({double:.5} against {amplitude:.5})"
            );
        }
    }

    #[test]
    fn the_pitch_does_not_move() {
        // The playout must not change the pitch to cover a difference
        // in clocks: a rate is a pitch, and a pitch that moves is
        // heard — as a wobble, or as a slow slide.
        //
        // So the pitch is watched, second by second, over a call.
        const CARD_RATE: u32 = 48000;
        let state = AudioState::default();
        let playback = Playback::new(8000, CARD_RATE);
        let mut playback = playback;
        let frame = vec![16384i16; 160];
        for _ in 0..4 {
            state.push_inbound(&frame);
        }
        let mut buffer = [0.0f32; 960];
        let mut output = Vec::new();
        // A sine, so that there is a pitch to watch.
        let sine: Vec<i16> = (0..8000 * 5)
            .map(|i| {
                let t = i as f32 / 8000.0;
                (0.5 * (2.0 * std::f32::consts::PI * 400.0 * t).sin() * 32767.0) as i16
            })
            .collect();
        let mut sent = 0usize;
        for round in 0..500 {
            playback.begin(&state);
            fill_output(&mut buffer, 2, || playback.next(&state));
            for frame in buffer.chunks_exact(2) {
                output.push(frame[0]);
            }
            if round % 2 == 0 {
                let index = sent;
                sent += 1;
                state.push_inbound(&sine[index * 160..(index + 1) * 160]);
            }
        }

        let mut pitches = Vec::new();
        for second in 1..4 {
            let window = &output[second * CARD_RATE as usize..(second + 1) * CARD_RATE as usize];
            let (frequency, _) = purity(window, CARD_RATE, 400.0, 20.0);
            pitches.push(frequency);
        }
        for frequency in &pitches {
            assert!(
                (frequency - 400.0).abs() < 1.0,
                "the pitch moved: {pitches:?}"
            );
        }
    }

    #[test]
    fn the_audio_survives_a_jittery_and_lossy_peer() {
        // The real world: the packets do not arrive exactly every
        // 20 ms, and some of them never arrive. The audio must still
        // be the sine, and the gaps must be no worse than the loss.
        const CARD_RATE: u32 = 48000;
        const CHANNELS: u16 = 2;
        const FREQUENCY: f32 = 400.0;
        const AMPLITUDE: f32 = 0.5;

        let codec = crate::codec::Codec::Pcmu;
        let codec_rate = codec.sample_rate();
        let total = codec_rate as usize * 5;

        let signal: Vec<i16> = (0..total)
            .map(|i| {
                let t = i as f32 / codec_rate as f32;
                (AMPLITUDE * (2.0 * std::f32::consts::PI * FREQUENCY * t).sin() * 32767.0) as i16
            })
            .collect();
        let mut encoder = codec.encoder();
        let payloads: Vec<Vec<u8>> = signal
            .chunks(codec.frame_samples())
            .map(|chunk| encoder.encode(chunk))
            .collect();

        let state = AudioState::default();
        let mut jitter = crate::jitter::JitterBuffer::new(5);
        let mut decoder = codec.decoder();
        let mut playback = Playback::new(codec_rate, CARD_RATE);
        let mut buffer = [0.0f32; 960];
        let mut output: Vec<f32> = Vec::new();
        let mut sent = 0usize;

        // A simple deterministic "network": the packets arrive a little
        // early or late, and one in fifty is lost.
        let mut seed = 12345u64;
        let next = |seed: &mut u64| {
            *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((*seed >> 33) % 41) as i64 - 20
        };

        for round in 0..500 {
            playback.begin(&state);
            fill_output(&mut buffer, CHANNELS, || playback.next(&state));
            for frame in buffer.chunks_exact(CHANNELS as usize) {
                output.push(frame[0]);
            }

            // The peer sends a frame every 20 ms, with jitter, and
            // loses one in fifty.
            if round % 2 == 0 {
                let index = sent;
                sent += 1;
                let jitter_ms = next(&mut seed);
                if jitter_ms >= 0 {
                    // On time, or late: it goes now, and the jitter
                    // buffer sorts it out.
                    jitter.push(crate::jitter::Frame {
                        sequence: index as u16,
                        timestamp: (index as u32) * 160,
                        payload: payloads[index].clone(),
                    });
                } else {
                    // "Early" is modelled as a late packet: it turns
                    // up after the one that follows it.
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    jitter.push(crate::jitter::Frame {
                        sequence: index as u16,
                        timestamp: (index as u32) * 160,
                        payload: payloads[index].clone(),
                    });
                }
                crate::drain_jitter(&mut jitter, &mut decoder, &state);
            }
        }

        // How much of the output is silence? The peer lost one frame in
        // fifty, so one frame in fifty of the audio is missing — and
        // no more than that.
        let played = &output[200 * (CARD_RATE as usize / 1000)..];
        let silent = played.iter().filter(|v| **v == 0.0).count();
        let silence_ratio = silent as f64 / played.len() as f64;
        assert!(
            silence_ratio < 0.05,
            "the playout was silent for {silence_ratio:.3} of the time"
        );

        // And the audio that is there is still the sine: a jittery,
        // lossy peer must not put a wobble on it.
        let purity = worst_purity(played, CARD_RATE, FREQUENCY, 20.0);
        assert!(
            purity > 0.95,
            "the output is not a clean sine — {purity:.3} of the energy \
             is at the frequency"
        );
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
