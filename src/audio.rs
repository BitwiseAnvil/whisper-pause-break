use crate::{config::Config, logging::Log};
use anyhow::{Context, Result, bail, ensure};
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rubato::{FftFixedInOut, Resampler};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub const WHISPER_RATE: usize = 16_000;

#[derive(Default)]
struct Buffer {
    active: bool,
    samples: Vec<f32>,
    limit: usize,
}

pub struct Recorder {
    _stream: cpal::Stream,
    buffer: Arc<Mutex<Buffer>>,
    failed: Arc<AtomicBool>,
    pub rate: u32,
    device_id: cpal::DeviceId,
}

impl Recorder {
    pub fn open(config: &Config, log: &Log) -> Result<Self> {
        let device = cpal::default_host()
            .default_input_device()
            .context("No default microphone")?;
        let device_id = device.id()?;
        let supported = device.default_input_config()?;
        let rate = supported.sample_rate();
        let channels = supported.channels() as usize;
        ensure!(channels > 0 && rate > 0, "Invalid microphone format");
        let limit = rate as usize * config.max_recording_seconds as usize;
        let buffer = Arc::new(Mutex::new(Buffer {
            active: false,
            samples: Vec::with_capacity(limit),
            limit,
        }));
        let failed = Arc::new(AtomicBool::new(false));
        let stream_config = supported.config();
        let stream = match supported.sample_format() {
            SampleFormat::F32 => input::<f32>(
                &device,
                stream_config,
                channels,
                buffer.clone(),
                failed.clone(),
                log.clone(),
            ),
            SampleFormat::I16 => input::<i16>(
                &device,
                stream_config,
                channels,
                buffer.clone(),
                failed.clone(),
                log.clone(),
            ),
            SampleFormat::U16 => input::<u16>(
                &device,
                stream_config,
                channels,
                buffer.clone(),
                failed.clone(),
                log.clone(),
            ),
            SampleFormat::I32 => input::<i32>(
                &device,
                stream_config,
                channels,
                buffer.clone(),
                failed.clone(),
                log.clone(),
            ),
            SampleFormat::F64 => input::<f64>(
                &device,
                stream_config,
                channels,
                buffer.clone(),
                failed.clone(),
                log.clone(),
            ),
            format => bail!("Unsupported microphone sample format: {format}"),
        }?;
        stream.play()?;
        Ok(Self {
            _stream: stream,
            buffer,
            failed,
            rate,
            device_id,
        })
    }

    pub fn needs_reopen(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
            || cpal::default_host()
                .default_input_device()
                .and_then(|d| d.id().ok())
                .is_none_or(|id| id != self.device_id)
    }

    pub fn start(&self) -> Result<()> {
        ensure!(
            !self.failed.load(Ordering::Relaxed),
            "Microphone unavailable; reconnect it and try again"
        );
        let mut b = self.buffer.lock().unwrap();
        b.samples.clear();
        b.active = true;
        Ok(())
    }

    pub fn stop(&self) -> Result<Vec<f32>> {
        let mut b = self.buffer.lock().unwrap();
        b.active = false;
        ensure!(
            !self.failed.load(Ordering::Relaxed),
            "Microphone failed or recording limit reached; audio discarded"
        );
        // Leave an empty buffer with capacity ready for the next press.
        let replacement = Vec::with_capacity(b.limit);
        Ok(std::mem::replace(&mut b.samples, replacement))
    }

    pub fn discard(&self) {
        let mut b = self.buffer.lock().unwrap();
        b.active = false;
        b.samples.clear();
    }
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

fn input<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    buffer: Arc<Mutex<Buffer>>,
    failed: Arc<AtomicBool>,
    log: Log,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let callback_failure = failed.clone();
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| {
            let Ok(mut b) = buffer.lock() else {
                callback_failure.store(true, Ordering::Relaxed);
                return;
            };
            if !b.active {
                return;
            }
            for frame in data.chunks_exact(channels) {
                if b.samples.len() >= b.limit {
                    b.active = false;
                    callback_failure.store(true, Ordering::Relaxed);
                    break;
                }
                let mono =
                    frame.iter().map(|v| f32::from_sample(*v)).sum::<f32>() / channels as f32;
                b.samples.push(if mono.is_finite() {
                    mono.clamp(-1.0, 1.0)
                } else {
                    0.0
                });
            }
        },
        move |error| {
            // WASAPI can signal a recoverable startup discontinuity on USB microphones.
            // CPAL continues streaming after Xrun; reopening here would create a restart loop.
            if error.kind() != cpal::ErrorKind::Xrun {
                log.event(format!("Microphone stream failure: {error}"));
                failed.store(true, Ordering::Relaxed);
            }
        },
        None,
    )?)
}

/// FFT resampling applies an anti-aliasing filter and compensates its delay.
pub fn resample(samples: &[f32], rate: u32) -> Result<Vec<f32>> {
    ensure!(
        (8_000..=384_000).contains(&rate),
        "Unsupported sample rate {rate}"
    );
    if rate as usize == WHISPER_RATE {
        return Ok(samples.to_vec());
    }
    if samples.is_empty() {
        return Ok(Vec::new());
    }
    let wanted = (samples.len() as u64 * WHISPER_RATE as u64 / u64::from(rate)) as usize;
    let mut resampler = FftFixedInOut::<f32>::new(rate as usize, WHISPER_RATE, 1024, 1)?;
    let delay = resampler.output_delay();
    let chunk = resampler.input_frames_next();
    let mut out = Vec::with_capacity(wanted + delay + resampler.output_frames_max());
    let mut pos = 0;
    while out.len() < wanted + delay {
        let mut block = vec![0.0; chunk];
        let n = chunk.min(samples.len().saturating_sub(pos));
        block[..n].copy_from_slice(&samples[pos..pos + n]);
        pos += n;
        let result = resampler.process(&[block], None)?;
        out.extend_from_slice(&result[0]);
    }
    Ok(out[delay..delay + wanted].to_vec())
}

pub fn prepare(samples: &[f32], rate: u32, config: &Config) -> Result<Option<Vec<f32>>> {
    if samples.len() < rate as usize * config.min_recording_ms as usize / 1000 {
        return Ok(None);
    }
    let mut mono = resample(samples, rate)?;
    // Require at least 120ms above the noise floor. A single cue/click cannot trigger Whisper.
    let voiced = mono
        .chunks(320)
        .filter(|frame| {
            let rms = (frame.iter().map(|x| x * x).sum::<f32>() / frame.len() as f32).sqrt();
            rms > config.silence_rms
        })
        .count();
    if voiced < 6 {
        return Ok(None);
    }
    // Whisper expects a useful minimum window; padding does not discard short words.
    mono.resize(mono.len().max(WHISPER_RATE), 0.0);
    Ok(Some(mono))
}

pub fn read_wav(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    ensure!(
        spec.channels > 0 && spec.bits_per_sample > 0 && spec.bits_per_sample <= 32,
        "Unsupported WAV format"
    );
    ensure!(
        reader.duration() <= spec.sample_rate * 300,
        "WAV exceeds 5-minute limit"
    );
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<std::result::Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1_u64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<std::result::Result<_, _>>()?
        }
    };
    let mono = interleaved
        .chunks_exact(spec.channels as usize)
        .map(|frame| frame.iter().sum::<f32>() / spec.channels as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}

#[derive(Clone, Copy)]
pub enum Cue {
    Start,
    Stop,
    Complete,
    Cancel,
}

pub struct Cues {
    _stream: cpal::Stream,
    tx: crossbeam_channel::Sender<Cue>,
    device_id: cpal::DeviceId,
    failed: Arc<AtomicBool>,
}

impl Cues {
    pub fn open(volume: f32) -> Result<Self> {
        let device = cpal::default_host()
            .default_output_device()
            .context("No default speaker")?;
        let supported = device.default_output_config()?;
        let (tx, rx) = crossbeam_channel::bounded(8);
        let config = supported.config();
        let device_id = device.id()?;
        let failed = Arc::new(AtomicBool::new(false));
        let stream = match supported.sample_format() {
            SampleFormat::F32 => output::<f32>(&device, config, rx, volume, failed.clone()),
            SampleFormat::I16 => output::<i16>(&device, config, rx, volume, failed.clone()),
            SampleFormat::U16 => output::<u16>(&device, config, rx, volume, failed.clone()),
            SampleFormat::I32 => output::<i32>(&device, config, rx, volume, failed.clone()),
            SampleFormat::F64 => output::<f64>(&device, config, rx, volume, failed.clone()),
            format => bail!("Unsupported speaker format: {format}"),
        }?;
        stream.play()?;
        Ok(Self {
            _stream: stream,
            tx,
            device_id,
            failed,
        })
    }
    pub fn play(&self, cue: Cue) {
        let _ = self.tx.try_send(cue);
    }
    pub fn needs_reopen(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
            || cpal::default_host()
                .default_output_device()
                .and_then(|d| d.id().ok())
                .is_none_or(|id| id != self.device_id)
    }
}

fn output<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    rx: crossbeam_channel::Receiver<Cue>,
    volume: f32,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let sounds = [
        sound(Cue::Start, config.sample_rate, volume),
        sound(Cue::Stop, config.sample_rate, volume),
        sound(Cue::Complete, config.sample_rate, volume),
        sound(Cue::Cancel, config.sample_rate, volume),
    ];
    let mut playback = Playback {
        pos: sounds[0].len(),
        active: 0,
        sounds,
        rx,
    };
    Ok(device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            for frame in data.chunks_mut(channels) {
                frame.fill(T::from_sample(playback.next_sample()));
            }
        },
        move |error| {
            if error.kind() != cpal::ErrorKind::Xrun {
                failed.store(true, Ordering::Relaxed);
            }
        },
        None,
    )?)
}

struct Playback {
    sounds: [Vec<f32>; 4],
    rx: crossbeam_channel::Receiver<Cue>,
    active: usize,
    pos: usize,
}

impl Playback {
    fn next_sample(&mut self) -> f32 {
        // Finish each cue before the next: very fast CUDA inference must not
        // replace the stop cue with the completion cue in the same callback.
        if self.pos >= self.sounds[self.active].len()
            && let Ok(cue) = self.rx.try_recv()
        {
            self.active = cue as usize;
            self.pos = 0;
        }
        let value = self.sounds[self.active]
            .get(self.pos)
            .copied()
            .unwrap_or(0.0);
        self.pos = self.pos.saturating_add(1);
        value
    }
}

pub fn sound(cue: Cue, rate: u32, volume: f32) -> Vec<f32> {
    fn tone(rate: u32, ms: u32, start: f32, end: f32, volume: f32) -> Vec<f32> {
        let n = (rate * ms / 1000) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / rate as f32;
                let p = i as f32 / n as f32;
                let envelope = (std::f32::consts::PI * p).sin().powi(2);
                let phase = std::f32::consts::TAU * (start * t + (end - start) * t * p * 0.5);
                volume * envelope * phase.sin()
            })
            .collect()
    }
    match cue {
        Cue::Start => tone(rate, 35, 760.0, 1120.0, volume),
        Cue::Stop => tone(rate, 45, 620.0, 400.0, volume),
        Cue::Complete => {
            let mut out = tone(rate, 25, 1100.0, 1100.0, volume);
            out.resize(out.len() + (rate * 25 / 1000) as usize, 0.0);
            out.extend(tone(rate, 30, 1450.0, 1450.0, volume));
            out
        }
        Cue::Cancel => {
            let mut out = tone(rate, 30, 480.0, 360.0, volume);
            out.resize(out.len() + (rate * 20 / 1000) as usize, 0.0);
            out.extend(tone(rate, 35, 300.0, 180.0, volume));
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_cues_play_in_order_without_truncation() {
        for sequence in [
            &[Cue::Start, Cue::Stop, Cue::Complete][..],
            &[Cue::Start, Cue::Cancel][..],
        ] {
            let sounds = [Cue::Start, Cue::Stop, Cue::Complete, Cue::Cancel]
                .map(|cue| sound(cue, 16_000, 0.1));
            let expected: Vec<_> = sequence
                .iter()
                .flat_map(|cue| sounds[*cue as usize].iter().copied())
                .collect();
            let (tx, rx) = crossbeam_channel::bounded(8);
            for cue in sequence {
                tx.send(*cue).unwrap();
            }
            let mut playback = Playback {
                pos: sounds[0].len(),
                active: 0,
                sounds,
                rx,
            };
            let actual: Vec<_> = (0..expected.len())
                .map(|_| playback.next_sample())
                .collect();
            assert_eq!(actual, expected);
            assert_eq!(playback.next_sample(), 0.0);
        }
    }
    #[test]
    fn rates_preserve_length_and_dc_gain() {
        for rate in [16_000, 44_100, 48_000, 96_000] {
            let result = resample(&vec![0.25; rate as usize], rate).unwrap();
            assert_eq!(result.len(), 16_000);
            assert!((result[8000] - 0.25).abs() < 0.001);
        }
    }
    #[test]
    fn downsampling_rejects_aliases() {
        let s: Vec<_> = (0..48_000)
            .map(|i| (std::f32::consts::TAU * 12_000.0 * i as f32 / 48_000.0).sin())
            .collect();
        let r = resample(&s, 48_000).unwrap();
        let rms = (r[1000..15000].iter().map(|x| x * x).sum::<f32>() / 14000.0).sqrt();
        assert!(rms < 0.01, "Aliasing RMS: {rms}");
    }
    #[test]
    fn silence_taps_and_a_single_cue_do_not_transcribe() {
        let c = Config::default();
        assert!(prepare(&[0.0; 32_000], 16_000, &c).unwrap().is_none());
        assert!(prepare(&[0.5; 1000], 16_000, &c).unwrap().is_none());
        let mut click = sound(Cue::Start, 16_000, 0.1);
        click.resize(32_000, 0.0);
        assert!(prepare(&click, 16_000, &c).unwrap().is_none());
    }
    #[test]
    fn cues_are_distinct_short_and_bounded() {
        let cues =
            [Cue::Start, Cue::Stop, Cue::Complete, Cue::Cancel].map(|c| sound(c, 48_000, 0.1));
        for cue in &cues {
            assert!(cue.len() < 4800);
            assert!(cue.iter().all(|x| x.abs() <= 0.1));
        }
        for (i, cue) in cues.iter().enumerate() {
            for other in &cues[i + 1..] {
                assert_ne!(cue, other);
            }
        }
    }
}
