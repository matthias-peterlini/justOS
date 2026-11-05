//! Sound outputs registered by drivers, and playing sounds on them.
//!
//! A sound is converted to the output's rate and channel count (linear
//! interpolation), scaled by the volume and queued on the first output
//! from a thread of its own, so the caller does not wait. A new sound stops
//! the one playing.

use crate::sync::{IrqLock, Mutex};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use justdk::{AudioDesc, Status, E_INVAL};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub struct Output {
    pub device: u32,
    pub name: String,
    pub rate: u32,
    pub channels: u32,
    desc: *const AudioDesc,
}

// SAFETY: the descriptor belongs to the driver; calls are serialised by PLAYER.
unsafe impl Send for Output {}

static OUTPUTS: IrqLock<Vec<Option<Output>>> = IrqLock::new(Vec::new());
/// Held while a sound is being queued: one at a time.
static PLAYER: Mutex<()> = Mutex::new(());
/// Asks the sound being played to stop.
static STOP: AtomicBool = AtomicBool::new(false);
static VOLUME: AtomicU32 = AtomicU32::new(80);
/// The sound being played: a number given to each new one, how many of its
/// frames have been queued, how many it has, and whether it is still going.
static TRACK: AtomicU64 = AtomicU64::new(0);
static POSITION: AtomicU64 = AtomicU64::new(0);
static LENGTH: AtomicU64 = AtomicU64::new(0);
static PLAYING: AtomicBool = AtomicBool::new(false);

pub fn register(device: u32, desc: *const AudioDesc) -> Status {
    // SAFETY: the driver keeps the descriptor alive.
    let d = unsafe { &*desc };
    if d.rate < 8000 || d.rate > 192_000 || d.channels == 0 || d.channels > 8 {
        return E_INVAL;
    }
    let len = d.name.iter().position(|&b| b == 0).unwrap_or(d.name.len());
    let name = String::from(core::str::from_utf8(&d.name[..len]).unwrap_or("sound output"));
    let mut outs = OUTPUTS.lock();
    let n = outs.len();
    crate::info!("audio", "audio{n}: {name}, {} Hz, {} channels", d.rate, d.channels);
    outs.push(Some(Output { device, name, rate: d.rate, channels: d.channels, desc }));
    n as Status
}

pub fn remove_device(device: u32) {
    for slot in OUTPUTS.lock().iter_mut() {
        if slot.as_ref().is_some_and(|o| o.device == device) {
            *slot = None;
        }
    }
}

pub struct OutputInfo {
    pub index: usize,
    pub name: String,
    pub rate: u32,
    pub channels: u32,
}

pub fn list() -> Vec<OutputInfo> {
    OUTPUTS
        .lock()
        .iter()
        .enumerate()
        .filter_map(|(i, o)| {
            let o = o.as_ref()?;
            Some(OutputInfo { index: i, name: o.name.clone(), rate: o.rate, channels: o.channels })
        })
        .collect()
}

pub fn available() -> bool {
    OUTPUTS.lock().iter().any(|o| o.is_some())
}

/// Volume, 0 to 100.
pub fn volume() -> u32 {
    VOLUME.load(Ordering::Relaxed)
}

pub fn set_volume(v: u32) {
    VOLUME.store(v.min(100), Ordering::Relaxed);
}

/// A sound in memory: interleaved signed 16-bit samples.
pub struct Sound {
    pub rate: u32,
    pub channels: u32,
    pub samples: Vec<i16>,
}

impl Sound {
    pub fn seconds_x10(&self) -> u64 {
        let frames = self.samples.len() as u64 / self.channels.max(1) as u64;
        frames * 10 / self.rate.max(1) as u64
    }
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Reads a WAV file: integer PCM of 8, 16, 24 or 32 bits, any rate, one or
/// more channels.
pub fn parse_wav(b: &[u8]) -> Result<Sound, &'static str> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a WAV file");
    }
    let mut at = 12;
    let mut fmt: Option<(u16, u32, u16, u16)> = None;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let len = le32(b, at + 4) as usize;
        let body = &b[at + 8..(at + 8 + len).min(b.len())];
        if id == b"fmt " && body.len() >= 16 {
            let mut format = le16(body, 0);
            if format == 0xFFFE && body.len() >= 26 {
                format = le16(body, 24); // WAVE_FORMAT_EXTENSIBLE: the sub-format
            }
            fmt = Some((format, le32(body, 4), le16(body, 2), le16(body, 14)));
        } else if id == b"data" {
            let Some((format, rate, channels, bits)) = fmt else { return Err("WAV data before its format") };
            if format != 1 {
                return Err("compressed WAV files are not supported");
            }
            if channels == 0 || !(1000..=192_000).contains(&rate) {
                return Err("unsupported WAV format");
            }
            let width = match bits {
                8 | 16 | 24 | 32 => bits as usize / 8,
                _ => return Err("unsupported sample size"),
            };
            let samples = body
                .chunks_exact(width)
                .map(|s| match width {
                    1 => ((s[0] as i16) - 128) << 8,
                    2 => i16::from_le_bytes([s[0], s[1]]),
                    3 => i16::from_le_bytes([s[1], s[2]]),
                    _ => i16::from_le_bytes([s[2], s[3]]),
                })
                .collect();
            return Ok(Sound { rate, channels: channels as u32, samples });
        }
        at += 8 + len + (len & 1);
    }
    Err("no sound in the WAV file")
}

/// A sine-ish tone (a triangle wave, which needs no floating point).
pub fn tone(hz: u32, ms: u32) -> Sound {
    let rate = 48_000u32;
    let frames = (rate as u64 * ms as u64 / 1000) as usize;
    let period = (rate / hz.clamp(20, 20_000)).max(2) as usize;
    let mut samples = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let p = (i % period) as i32 * 4 * 12000 / period as i32;
        let mut v = if p < 2 * 12000 { p - 12000 } else { 3 * 12000 - p };
        // 5 ms fade in and out, against clicks.
        let edge = (rate as usize / 200).max(1);
        let fade = i.min(frames - 1 - i).min(edge) as i32;
        v = v * fade / edge as i32;
        samples.push(v as i16);
        samples.push(v as i16);
    }
    Sound { rate, channels: 2, samples }
}

/// Converts a sound to `rate` and `channels`, with the volume applied.
fn convert(s: &Sound, rate: u32, channels: u32, from: usize, frames_out: usize) -> Vec<i16> {
    let ch_in = s.channels as usize;
    let frames_in = s.samples.len() / ch_in;
    // Volume on a curve that sounds even: (v/100)^2, in 1/65536 units.
    let v = volume() as i64;
    let gain = v * v * 65536 / 10_000;
    let mut out = Vec::with_capacity(frames_out * channels as usize);
    for i in from..from + frames_out {
        // Position in the source, in 1/65536 frames.
        let pos = i as u64 * s.rate as u64 * 65536 / rate as u64;
        let (j, frac) = ((pos >> 16) as usize, (pos & 0xFFFF) as i64);
        if j >= frames_in {
            break;
        }
        let k = (j + 1).min(frames_in - 1);
        for c in 0..channels as usize {
            // Mono goes to every channel; extra source channels are dropped.
            let src = c.min(ch_in - 1);
            let a = s.samples[j * ch_in + src] as i64;
            let b = s.samples[k * ch_in + src] as i64;
            let x = a + (b - a) * frac / 65536;
            out.push((x * gain / 65536).clamp(-32768, 32767) as i16);
        }
    }
    out
}

fn first_output() -> Option<(*const AudioDesc, u32, u32)> {
    OUTPUTS.lock().iter().flatten().next().map(|o| (o.desc, o.rate, o.channels))
}

/// Plays a sound to its end (or until [`stop`]) on the calling thread.
pub fn play_now(s: &Sound) -> Result<(), &'static str> {
    if s.samples.is_empty() || s.channels == 0 {
        return Ok(());
    }
    STOP.store(true, Ordering::Release);
    let _player = PLAYER.lock();
    STOP.store(false, Ordering::Release);
    queue(s, 0, 0)
}

fn queue(s: &Sound, from: usize, track: u64) -> Result<(), &'static str> {
    let Some((desc, rate, channels)) = first_output() else { return Err("no sound output") };
    let frames_in = s.samples.len() / s.channels as usize;
    let frames_out = (frames_in as u64 * rate as u64 / s.rate as u64) as usize;
    // About 40 ms at a time, so stopping and volume changes are quick.
    let step = (rate as usize / 25).max(256);
    let mut at = (from as u64 * rate as u64 / s.rate as u64) as usize;
    if track != 0 {
        LENGTH.store(frames_in as u64, Ordering::Relaxed);
        PLAYING.store(true, Ordering::Release);
    }
    while at < frames_out && !STOP.load(Ordering::Acquire) {
        if track != 0 {
            POSITION.store(at as u64 * s.rate as u64 / rate as u64, Ordering::Relaxed);
        }
        let n = step.min(frames_out - at);
        let chunk = convert(s, rate, channels, at, n);
        let frames = chunk.len() / channels as usize;
        if frames == 0 {
            break;
        }
        // The sound pauses while the machine sleeps.
        super::pm::wait_awake();
        // SAFETY: the output exists while its driver does; the driver
        // copies the samples before returning.
        unsafe { ((*desc).write)((*desc).ctx, chunk.as_ptr(), frames) };
        at += n;
    }
    super::pm::wait_awake();
    // SAFETY: as above.
    unsafe { ((*desc).drain)((*desc).ctx) };
    if track != 0 && TRACK.load(Ordering::Acquire) == track {
        if !STOP.load(Ordering::Acquire) {
            POSITION.store(frames_in as u64, Ordering::Relaxed);
        }
        PLAYING.store(false, Ordering::Release);
    }
    Ok(())
}

/// Plays a sound in the background from frame `from`, keeping track of
/// where it is ([`progress`]). Returns the number it is known by.
pub fn play_track(s: Arc<Sound>, from: usize) -> Result<u64, &'static str> {
    if !available() {
        return Err("no sound output");
    }
    STOP.store(true, Ordering::Release);
    let track = TRACK.fetch_add(1, Ordering::AcqRel) + 1;
    POSITION.store(from as u64, Ordering::Relaxed);
    PLAYING.store(true, Ordering::Release);
    crate::task::spawn("audio", move || {
        let _player = PLAYER.lock();
        if TRACK.load(Ordering::Acquire) != track {
            return; // a newer sound came meanwhile
        }
        STOP.store(false, Ordering::Release);
        let _ = queue(&s, from, track);
    });
    Ok(track)
}

/// The sound started by [`play_track`]: its number, the frame being played,
/// its length in frames and whether it is still playing.
pub fn progress() -> (u64, u64, u64, bool) {
    (TRACK.load(Ordering::Acquire), POSITION.load(Ordering::Relaxed), LENGTH.load(Ordering::Relaxed), PLAYING.load(Ordering::Acquire))
}

/// Plays a sound in the background.
pub fn play(s: Sound) -> Result<(), &'static str> {
    if !available() {
        return Err("no sound output");
    }
    STOP.store(true, Ordering::Release);
    let sound = Arc::new(s);
    crate::task::spawn("audio", move || {
        let _ = play_now(&sound);
    });
    Ok(())
}

/// Stops the sound being played.
pub fn stop() {
    STOP.store(true, Ordering::Release);
    PLAYING.store(false, Ordering::Release);
}

/// Reads only the header of a WAV file: (rate, channels, frames).
pub fn wav_info(b: &[u8]) -> Option<(u32, u32, u64)> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let mut fmt = None;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let len = le32(b, at + 4) as usize;
        if id == b"fmt " && at + 8 + 16 <= b.len() {
            fmt = Some((le32(b, at + 12), le16(b, at + 10) as u32, le16(b, at + 22) as u32));
        } else if id == b"data" {
            let (rate, ch, bits) = fmt?;
            let frame = (ch * bits / 8).max(1) as u64;
            return Some((rate, ch, len as u64 / frame));
        }
        at += 8 + len + (len & 1);
    }
    None
}
