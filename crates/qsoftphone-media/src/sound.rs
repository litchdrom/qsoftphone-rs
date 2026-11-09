//! The sounds the softphone makes on its own.
//!
//! Two of them, and neither of them belongs to a call:
//!
//! * the **ringtone** — what the user hears when a call comes in;
//! * the **ringback** — what the user hears when the phone they called
//!   is ringing and there is no early media to hear.
//!
//! The C++ has both (`RingtonePlayer` and `RingbackPlayer`). This is the
//! same thing without pjsip: the tones are made here, and the ringtone
//! can be a WAV file the user picks in the settings.
//!
//! ## The interface thread
//!
//! §6 of the brief: the interface must not wait for anything. Opening a
//! sound card takes a moment, and a ringtone lasts as long as the call
//! rings — so the player runs on a thread of its own and the interface
//! only ever says "play this" or "stop".
//!
//! ## The sound card is shared
//!
//! The player opens the speaker on its own, beside a call's own stream.
//! On Windows the device is opened in shared mode, so the two mix —
//! which is what the C++ does as well, where the ringtone goes into the
//! same conference bridge the calls use.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};

use crate::audio::{self, AudioError, FromNormalised};

/// How often the playing thread looks at the stop flag.
///
/// The flag is set by the interface thread and read by this one. Twenty
/// milliseconds is short enough that the sound stops when the user
/// answers, and long enough not to spin.
const STOP_POLL: Duration = Duration::from_millis(20);

/// The ringtone's cadence: two seconds on, four off.
///
/// The North American cadence, and the one the C++'s `ringtone.wav`
/// holds: it is what a telephone in North America sounds like.
const RINGTONE_ON: f64 = 2.0;
const RINGTONE_OFF: f64 = 4.0;

/// The ringback's cadence: one second on, three off.
///
/// The C++'s `RingbackPlayer`: a single tone, a second in every four.
const RINGBACK_ON: f64 = 1.0;
const RINGBACK_OFF: f64 = 3.0;

/// How long the fade at each end of a burst of tone lasts.
///
/// A tone that starts at full volume clicks, and a click in a
/// softphone is heard as a fault in the call.
const FADE: f64 = 0.02;

/// The longest a sound may play.
///
/// The C++'s `RingtonePlayer` and `RingbackPlayer` both have this
/// watchdog, and the reason is worth keeping: pjsip 2.14 has a path
/// where the call is torn down so fast that the disconnected state
/// never reaches the UI, and without a cap the tone would play for
/// ever. This build has no such path — but a call that rings for ever
/// is still a fault, and the cap is the safety net for both.
const MAX_PLAYING: Duration = Duration::from_secs(60);

/// The sound the softphone plays to itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sound {
    /// The ringtone: what the user hears when a call comes in.
    ///
    /// The file is the one the user picked in the settings. `None` is
    /// the built-in tone, which is the C++'s own `ringtone.wav` made
    /// here instead of read from a file.
    Ringtone { file: Option<PathBuf> },
    /// The ringback: what the user hears when the phone they called is
    /// ringing and there is no early media to hear.
    Ringback,
}

/// How a sound is played.
#[derive(Debug, Clone, Default)]
pub struct SoundOptions {
    /// The speaker to open. `None` is the system default.
    pub speaker: Option<String>,
    /// How loud, from 0.0 to 1.0.
    pub volume: f32,
}

/// A sound that is playing.
pub struct SoundPlayer {
    playing: Mutex<Option<Playing>>,
    /// Whether the player makes a sound at all.
    ///
    /// A silent player still knows what it would be playing, so the
    /// decision is still tested — it just does not open a device. The
    /// tests use it: a test suite that beeps through every run is a
    /// suite people stop running.
    audible: bool,
    /// The longest a sound may play.
    ///
    /// The watchdog: a call that rings for ever is a fault, whatever
    /// the call stack says.
    max_playing: Duration,
}

/// The sound that is playing, and the flag that stops it.
struct Playing {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// When the sound started — the watchdog counts from here.
    started: Instant,
}

impl Default for SoundPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl SoundPlayer {
    /// A player with nothing playing.
    pub fn new() -> Self {
        Self::with_max_playing(MAX_PLAYING)
    }

    /// A player that makes no sound.
    ///
    /// It still tracks what it would be playing — so `is_playing` is
    /// true and the decision is still tested — but it never opens a
    /// device. This is what the tests use.
    pub fn silent() -> Self {
        Self {
            audible: false,
            ..Self::with_max_playing(MAX_PLAYING)
        }
    }

    /// A player with a different watchdog.
    ///
    /// The cap is a minute in the app; a test needs it to be shorter
    /// than a test.
    pub fn with_max_playing(max_playing: Duration) -> Self {
        Self {
            playing: Mutex::new(None),
            audible: true,
            max_playing,
        }
    }

    /// Play a sound, replacing whatever was playing.
    ///
    /// It returns at once: opening a device takes a moment, and §6 of
    /// the brief forbids the interface waiting for it. The sound plays
    /// on a thread of its own until it is stopped.
    pub fn play(&self, sound: Sound, options: &SoundOptions) {
        // The sound that is playing has to let go of the device before
        // the new one opens it. That wait happens on the **new**
        // thread, so the interface does not sit through it.
        let previous = self.playing.lock().expect("sound lock").take();

        let stop = Arc::new(AtomicBool::new(false));

        if !self.audible {
            // A silent player: the state is kept, the device is never
            // opened.
            *self.playing.lock().expect("sound lock") = Some(Playing {
                stop,
                thread: None,
                started: Instant::now(),
            });
            return;
        }

        let flag = stop.clone();
        let options = options.clone();
        let max_playing = self.max_playing;

        let thread = std::thread::Builder::new()
            .name("qsoftphone-sound".to_string())
            .spawn(move || {
                if let Some(mut previous) = previous {
                    previous.stop.store(true, Ordering::Relaxed);
                    if let Some(thread) = previous.thread.take() {
                        let _ = thread.join();
                    }
                }
                if let Err(e) = play_loop(&sound, &options, &flag, max_playing) {
                    tracing::warn!(error = %e, "the sound could not be played");
                } else {
                    tracing::info!(sound = ?sound, "the sound has stopped");
                }
                // The sound is over: say so, so that `is_playing` does
                // not go on saying that it is.
                flag.store(true, Ordering::Relaxed);
            });

        match thread {
            Ok(thread) => {
                *self.playing.lock().expect("sound lock") = Some(Playing {
                    stop,
                    thread: Some(thread),
                    started: Instant::now(),
                });
            }
            Err(e) => tracing::warn!(error = %e, "the sound thread could not be started"),
        }
    }

    /// Stop the sound.
    ///
    /// It does not wait for the thread to finish: the flag is set, and
    /// the thread lets go of the device on its own. The interface must
    /// not block (§6).
    pub fn stop(&self) {
        let playing = self.playing.lock().expect("sound lock").take();
        if let Some(mut playing) = playing {
            playing.stop.store(true, Ordering::Relaxed);
            // The handle is dropped, and the thread finishes on its own.
            playing.thread.take();
        }
    }

    /// Whether a sound is playing.
    ///
    /// A sound that has been playing for longer than the watchdog
    /// allows is not playing any more: the cap is the safety net for a
    /// call that never ends.
    pub fn is_playing(&self) -> bool {
        let playing = self.playing.lock().expect("sound lock");
        let Some(playing) = playing.as_ref() else {
            return false;
        };
        if playing.stop.load(Ordering::Relaxed) {
            return false;
        }
        playing.started.elapsed() < self.max_playing
    }
}

/// Play a sound until the flag is set.
fn play_loop(
    sound: &Sound,
    options: &SoundOptions,
    stop: &Arc<AtomicBool>,
    max_playing: Duration,
) -> Result<(), AudioError> {
    let host = cpal::default_host();
    let device = audio::find_device(&host, options.speaker.as_deref(), false)
        .ok_or_else(|| AudioError::NoDevice("there is no speaker to open".to_string()))?;

    let supported = device
        .default_output_config()
        .map_err(|e| AudioError::Backend(e.to_string()))?;
    let rate = supported.sample_rate();
    let channels = supported.channels().max(1);
    let config: StreamConfig = supported.config();
    let format = supported.sample_format();

    let samples = samples_for(sound, rate);
    if samples.is_empty() {
        return Ok(());
    }
    let samples = apply_volume(&samples, options.volume);

    let flag = stop.clone();
    let stream = match format {
        SampleFormat::I8 => build_output::<i8>(&device, config, channels, samples, flag),
        SampleFormat::I16 => build_output::<i16>(&device, config, channels, samples, flag),
        SampleFormat::I32 => build_output::<i32>(&device, config, channels, samples, flag),
        SampleFormat::I64 => build_output::<i64>(&device, config, channels, samples, flag),
        SampleFormat::U8 => build_output::<u8>(&device, config, channels, samples, flag),
        SampleFormat::U16 => build_output::<u16>(&device, config, channels, samples, flag),
        SampleFormat::U32 => build_output::<u32>(&device, config, channels, samples, flag),
        SampleFormat::U64 => build_output::<u64>(&device, config, channels, samples, flag),
        SampleFormat::F32 => build_output::<f32>(&device, config, channels, samples, flag),
        SampleFormat::F64 => build_output::<f64>(&device, config, channels, samples, flag),
        other => {
            return Err(AudioError::UnsupportedFormat(format!(
                "{other} is not a format this build can write"
            )));
        }
    }?;

    stream
        .play()
        .map_err(|e| AudioError::Backend(e.to_string()))?;

    tracing::info!(sound = ?sound, "the sound is playing");

    // The sound plays until it is stopped — or until the watchdog says
    // the call has been ringing for too long.
    let started = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if started.elapsed() >= max_playing {
            break;
        }
        std::thread::sleep(STOP_POLL);
    }

    Ok(())
}

/// The samples of a sound, in the device's own rate.
fn samples_for(sound: &Sound, rate: u32) -> Vec<f32> {
    match sound {
        Sound::Ringtone { file: None } => ringtone_samples(rate),
        Sound::Ringtone { file: Some(path) } => match read_wav(path) {
            Ok((samples, file_rate)) => {
                if file_rate == rate {
                    samples
                } else {
                    resample(&samples, file_rate, rate)
                }
            }
            Err(e) => {
                // A ringtone file that cannot be read is not a reason
                // to be silent: the built-in tone is the same shape of
                // sound, and the file is the user's next problem.
                tracing::warn!(error = %e, "the ringtone file could not be read");
                ringtone_samples(rate)
            }
        },
        Sound::Ringback => ringback_samples(rate),
    }
}

/// The built-in ringtone: two tones, 440 Hz and 480 Hz, two seconds on
/// and four off.
///
/// This is the North American cadence, and it is what the C++'s
/// `ringtone.wav` holds — the same sound, made here rather than read
/// from a file, so that a softphone with no ringtone file still rings.
pub fn ringtone_samples(rate: u32) -> Vec<f32> {
    cadence_samples(rate, RINGTONE_ON, RINGTONE_OFF, &[440.0, 480.0])
}

/// The ringback: one tone, 440 Hz, one second on and three off.
///
/// The C++'s `RingbackPlayer`: a single tone, a second in every four.
pub fn ringback_samples(rate: u32) -> Vec<f32> {
    cadence_samples(rate, RINGBACK_ON, RINGBACK_OFF, &[440.0])
}

/// A tone in a cadence: the tone, then the silence, and round again.
fn cadence_samples(rate: u32, on: f64, off: f64, hz: &[f64]) -> Vec<f32> {
    if rate == 0 || hz.is_empty() {
        return Vec::new();
    }

    let on_count = (rate as f64 * on) as usize;
    let off_count = (rate as f64 * off) as usize;
    let fade = (rate as f64 * FADE) as usize;

    let mut samples = Vec::with_capacity(on_count + off_count);
    for i in 0..on_count {
        let time = i as f64 / rate as f64;
        let value: f64 = hz
            .iter()
            .map(|frequency| (2.0 * std::f64::consts::PI * frequency * time).sin())
            .sum::<f64>()
            / hz.len() as f64;

        // The fade at each end of the burst, so that it does not click.
        let envelope = if fade == 0 {
            1.0
        } else if i < fade {
            i as f64 / fade as f64
        } else if i + fade >= on_count {
            (on_count - i) as f64 / fade as f64
        } else {
            1.0
        };

        samples.push((value * 0.4 * envelope) as f32);
    }
    samples.resize(on_count + off_count, 0.0);
    samples
}

/// Scale the samples by the volume.
fn apply_volume(samples: &[f32], volume: f32) -> Vec<f32> {
    let volume = volume.clamp(0.0, 1.0);
    samples.iter().map(|s| s * volume).collect()
}

/// Build the output stream that plays the samples, for one sample
/// format.
///
/// The samples are played in a loop: a ringtone is a cadence and not a
/// clip, and the player is stopped rather than run out.
fn build_output<T: FromNormalised + cpal::SizedSample>(
    device: &Device,
    config: StreamConfig,
    channels: u16,
    samples: Vec<f32>,
    stop: Arc<AtomicBool>,
) -> Result<Stream, AudioError> {
    let count = samples.len();
    let cursor = Arc::new(AtomicUsize::new(0));
    let frames = channels.max(1) as usize;

    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                if stop.load(Ordering::Relaxed) {
                    // Silence while the thread notices the flag.
                    for slot in data.iter_mut() {
                        *slot = T::from_normalised(0.0);
                    }
                    return;
                }

                let wanted = data.len() / frames;
                let base = cursor.fetch_add(wanted, Ordering::Relaxed) % count;
                for frame in 0..wanted {
                    let value = samples[(base + frame) % count];
                    let start = frame * frames;
                    for slot in data[start..start + frames].iter_mut() {
                        *slot = T::from_normalised(value);
                    }
                }
            },
            |error| {
                tracing::warn!(error = %error, "the speaker failed while a sound was playing");
            },
            None,
        )
        .map_err(|e| AudioError::Backend(e.to_string()))
}

/// Why a sound file could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundError {
    /// The file could not be opened or read.
    Unreadable(String),
    /// The file is not a WAV this build can play.
    Unsupported(String),
}

impl std::fmt::Display for SoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SoundError::Unreadable(why) => write!(f, "the file could not be read: {why}"),
            SoundError::Unsupported(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for SoundError {}

/// Read a WAV file: sixteen-bit PCM, mono or stereo.
///
/// The ringtone a user picks in the settings is a WAV file, and this
/// is the reader for it. Only sixteen-bit PCM is read — it is what a
/// ringtone is, and every other format is a file this build would have
/// to guess at.
///
/// The samples come back as one value per frame: a stereo file is mixed
/// down to mono, because the speaker is one stream of frames.
pub fn read_wav(path: &Path) -> Result<(Vec<f32>, u32), SoundError> {
    let bytes = std::fs::read(path)
        .map_err(|e| SoundError::Unreadable(format!("{}: {e}", path.display())))?;

    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(SoundError::Unsupported(format!(
            "{} is not a WAV file",
            path.display()
        )));
    }

    let mut format = 0u16;
    let mut channels = 0u16;
    let mut rate = 0u32;
    let mut bits = 0u16;
    let mut data: Option<&[u8]> = None;

    let mut at = 12usize;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]])
            as usize;
        let body = at + 8;
        let end = body.saturating_add(size).min(bytes.len());

        match id {
            b"fmt " => {
                if size < 16 || body + 16 > bytes.len() {
                    return Err(SoundError::Unsupported(format!(
                        "{} has no usable format chunk",
                        path.display()
                    )));
                }
                format = u16::from_le_bytes([bytes[body], bytes[body + 1]]);
                channels = u16::from_le_bytes([bytes[body + 2], bytes[body + 3]]);
                rate = u32::from_le_bytes([
                    bytes[body + 4],
                    bytes[body + 5],
                    bytes[body + 6],
                    bytes[body + 7],
                ]);
                bits = u16::from_le_bytes([bytes[body + 14], bytes[body + 15]]);
            }
            b"data" => data = Some(&bytes[body..end]),
            _ => {}
        }

        // The chunks are word aligned: an odd size is followed by a
        // padding byte.
        at = body + size + (size & 1);
    }

    if format != 1 {
        return Err(SoundError::Unsupported(format!(
            "{} is not uncompressed PCM",
            path.display()
        )));
    }
    if bits != 16 {
        return Err(SoundError::Unsupported(format!(
            "{} is {bits}-bit; this build reads sixteen-bit files",
            path.display()
        )));
    }

    let channels = channels.max(1) as usize;
    let data = data.ok_or_else(|| {
        SoundError::Unsupported(format!("{} has no samples in it", path.display()))
    })?;

    let frames = data.len() / 2 / channels;
    let mut samples = Vec::with_capacity(frames);
    for frame in 0..frames {
        let mut sum = 0.0f32;
        for channel in 0..channels {
            let at = (frame * channels + channel) * 2;
            let value = i16::from_le_bytes([data[at], data[at + 1]]) as f32 / 32768.0;
            sum += value;
        }
        samples.push(sum / channels as f32);
    }

    Ok((samples, rate))
}

/// Resample from one rate to another, linearly.
///
/// A ringtone is a WAV file, and a WAV file is usually at 44.1 kHz
/// while the sound card wants 48 kHz. The interpolation is crude, but
/// it is a ringtone: nobody has ever listened to one and said the
/// high end was wrong.
fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || samples.is_empty() || from == 0 || to == 0 {
        return samples.to_vec();
    }

    let ratio = from as f64 / to as f64;
    let count = (samples.len() as f64 / ratio) as usize;
    let last = samples.len() - 1;
    (0..count)
        .map(|i| {
            let position = i as f64 * ratio;
            let index = (position as usize).min(last);
            let fraction = (position - index as f64) as f32;
            let a = samples[index];
            let b = samples[(index + 1).min(last)];
            a + (b - a) * fraction
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV file with the given samples, as a ringtone file would be.
    fn wav(samples: &[i16], rate: u32, channels: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&((36 + samples.len() * 2) as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        bytes.extend_from_slice(&(channels * 2).to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&((samples.len() * 2) as u32).to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn the_built_in_ringtone_is_the_north_american_cadence() {
        let rate = 8000;
        let samples = ringtone_samples(rate);

        // Two seconds on, four off.
        assert_eq!(samples.len(), (rate as usize) * 6);

        // The tone is there...
        let loudest = samples[..(rate as usize) * 2]
            .iter()
            .fold(0.0f32, |loudest, s| loudest.max(s.abs()));
        assert!(loudest > 0.2, "the ringtone is too quiet: {loudest}");

        // ...and the silence after it is not.
        let quietest = samples[(rate as usize) * 2..]
            .iter()
            .fold(0.0f32, |loudest, s| loudest.max(s.abs()));
        assert_eq!(quietest, 0.0, "the ringtone does not fall silent");
    }

    #[test]
    fn the_ringback_is_one_tone_a_second_in_four() {
        let rate = 8000;
        let samples = ringback_samples(rate);

        assert_eq!(samples.len(), (rate as usize) * 4);
        let loudest = samples[..(rate as usize)]
            .iter()
            .fold(0.0f32, |loudest, s| loudest.max(s.abs()));
        assert!(loudest > 0.2, "the ringback is too quiet: {loudest}");
        let quietest = samples[(rate as usize)..]
            .iter()
            .fold(0.0f32, |loudest, s| loudest.max(s.abs()));
        assert_eq!(quietest, 0.0, "the ringback does not fall silent");
    }

    #[test]
    fn the_tone_fades_in_and_out_so_it_does_not_click() {
        let rate = 8000;
        let samples = ringtone_samples(rate);
        let first = samples[0].abs();
        let middle = samples[(rate as usize) / 2].abs();

        assert!(
            first < middle / 4.0,
            "the tone starts at full volume: {first} against {middle}"
        );
    }

    #[test]
    fn a_wav_file_is_read_as_mono_samples() {
        let path = std::env::temp_dir().join("qsoftphone-ringtone-test.wav");
        let samples: Vec<i16> = (0..100).map(|i| (i * 100) as i16).collect();
        std::fs::write(&path, wav(&samples, 8000, 1)).expect("write the wav");

        let (read, rate) = read_wav(&path).expect("the wav was not read");

        assert_eq!(rate, 8000);
        assert_eq!(read.len(), 100);
        assert!((read[10] - 1000.0 / 32768.0).abs() < 0.0001, "{}", read[10]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_stereo_wav_file_is_mixed_down_to_mono() {
        let path = std::env::temp_dir().join("qsoftphone-ringtone-stereo-test.wav");
        // Two channels: 1000 and 3000 on every frame.
        let mut samples = Vec::new();
        for _ in 0..50 {
            samples.push(1000i16);
            samples.push(3000i16);
        }
        std::fs::write(&path, wav(&samples, 8000, 2)).expect("write the wav");

        let (read, rate) = read_wav(&path).expect("the wav was not read");

        assert_eq!(rate, 8000);
        assert_eq!(read.len(), 50);
        assert!(
            (read[0] - 2000.0 / 32768.0).abs() < 0.0001,
            "the channels were not mixed: {}",
            read[0]
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_file_that_is_not_a_wav_is_an_error() {
        let path = std::env::temp_dir().join("qsoftphone-not-a-ringtone.wav");
        std::fs::write(&path, b"this is not a wav file at all").expect("write the file");

        let error = read_wav(&path).expect_err("the file was read");

        assert!(
            matches!(error, SoundError::Unsupported(_)),
            "the wrong error came back: {error}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_file_that_is_not_there_is_an_error() {
        let path = std::env::temp_dir().join("qsoftphone-no-such-ringtone.wav");

        let error = read_wav(&path).expect_err("the file was read");

        assert!(
            matches!(error, SoundError::Unreadable(_)),
            "the wrong error came back: {error}"
        );
    }

    #[test]
    fn a_sound_does_not_play_for_ever() {
        // The watchdog: a call that rings for ever is a fault, and the
        // cap is the safety net for it.
        let mut player = SoundPlayer::silent();
        player.max_playing = Duration::from_millis(50);
        player.play(Sound::Ringback, &SoundOptions::default());
        assert!(player.is_playing(), "the sound did not start");

        std::thread::sleep(Duration::from_millis(120));
        assert!(
            !player.is_playing(),
            "the sound was still playing after the watchdog"
        );
    }

    #[test]
    fn a_stopped_sound_is_not_playing() {
        let player = SoundPlayer::silent();
        player.play(Sound::Ringtone { file: None }, &SoundOptions::default());
        assert!(player.is_playing(), "the sound did not start");

        player.stop();
        assert!(!player.is_playing(), "the sound did not stop");
    }

    #[test]
    fn the_samples_are_resampled_to_the_device() {
        // A second at 8 kHz becomes a second at 16 kHz.
        let samples = vec![0.5f32; 8000];
        let resampled = resample(&samples, 8000, 16000);

        assert_eq!(resampled.len(), 16000);
        assert!((resampled[0] - 0.5).abs() < 0.0001);
    }

    #[test]
    fn the_volume_scales_the_samples() {
        let samples = vec![1.0f32, -1.0];
        assert_eq!(apply_volume(&samples, 0.5), vec![0.5, -0.5]);
        assert_eq!(apply_volume(&samples, 2.0), vec![1.0, -1.0]);
    }
}
