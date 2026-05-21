use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use librespot::playback::audio_backend::{Sink, SinkResult};
use librespot::playback::convert::Converter;
use librespot::playback::decoder::AudioPacket;

pub const NUM_BANDS: usize = 24;
pub const FFT_SIZE: usize = 512;
const SAMPLE_RATE: f32 = 44100.0;
const F_MIN: f32 = 40.0;
const F_MAX: f32 = 16000.0;

// Keep just enough recent audio for one FFT window plus a little slack.
// Stereo, so FFT_SIZE * 2 channels * 2 for safety = 2048 samples (~23 ms).
pub const CAPTURE_BUFFER_LEN: usize = FFT_SIZE * 4;

// Wraps the real audio backend sink; intercepts PCM samples for FFT analysis.
pub struct CaptureSink {
    pub inner: Box<dyn Sink>,
    pub buffer: Arc<Mutex<VecDeque<f32>>>,
}

impl Sink for CaptureSink {
    fn start(&mut self) -> SinkResult<()> {
        self.inner.start()
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.inner.stop()
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        if let AudioPacket::Samples(ref samples) = packet {
            if let Ok(mut buf) = self.buffer.lock() {
                // Drop overflow in one bulk drain, then append the whole packet —
                // far cheaper than per-sample pop_front/push_back at 44.1 kHz stereo.
                let incoming = samples.len();
                if incoming >= CAPTURE_BUFFER_LEN {
                    buf.clear();
                } else {
                    let overflow = (buf.len() + incoming).saturating_sub(CAPTURE_BUFFER_LEN);
                    if overflow > 0 {
                        buf.drain(..overflow);
                    }
                }
                let take = incoming.min(CAPTURE_BUFFER_LEN);
                buf.extend(samples[incoming - take..].iter().map(|&s| s as f32));
            }
        }
        self.inner.write(packet, converter)
    }
}

pub struct SpectrumAnalyzer {
    fft: Arc<dyn rustfft::Fft<f32>>,
    // Pre-computed Hann window — same every frame.
    hann: Vec<f32>,
    // Pre-computed log-spaced band bin ranges — same every frame.
    band_bins: Vec<(usize, usize)>,
    // Reusable scratch buffers — never reallocate after construction.
    fft_buffer: Vec<rustfft::num_complex::Complex<f32>>,
    magnitudes: Vec<f32>,
    bands: Vec<f32>,
    prev_bands: Vec<f32>,
    global_peak: f32,
}

impl SpectrumAnalyzer {
    pub fn new() -> Self {
        let mut planner = rustfft::FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);

        let hann: Vec<f32> = (0..FFT_SIZE)
            .map(|i| 0.5 * (1.0 - (std::f32::consts::TAU * i as f32 / (FFT_SIZE - 1) as f32).cos()))
            .collect();

        let mag_len = FFT_SIZE / 2;
        let band_bins: Vec<(usize, usize)> = (0..NUM_BANDS)
            .map(|i| {
                let f_low = F_MIN * (F_MAX / F_MIN).powf(i as f32 / NUM_BANDS as f32);
                let f_high = F_MIN * (F_MAX / F_MIN).powf((i + 1) as f32 / NUM_BANDS as f32);
                let lo = ((f_low / SAMPLE_RATE) * FFT_SIZE as f32) as usize;
                let hi = ((f_high / SAMPLE_RATE) * FFT_SIZE as f32) as usize;
                let lo = lo.min(mag_len.saturating_sub(1));
                let hi = hi.min(mag_len).max(lo + 1);
                (lo, hi)
            })
            .collect();

        Self {
            fft,
            hann,
            band_bins,
            fft_buffer: vec![rustfft::num_complex::Complex::new(0.0, 0.0); FFT_SIZE],
            magnitudes: vec![0.0; mag_len],
            bands: vec![0.0; NUM_BANDS],
            prev_bands: vec![0.0; NUM_BANDS],
            global_peak: 1.0,
        }
    }

    pub fn reset(&mut self) {
        self.prev_bands.fill(0.0);
    }

    /// Computes smoothed frequency band amplitudes (0.0–1.0) from raw PCM samples.
    /// Returns a borrowed slice — caller may copy if it needs ownership.
    pub fn process(&mut self, samples: &[f32]) -> &[f32] {
        use rustfft::num_complex::Complex;

        // Most recent FFT_SIZE mono frames (samples are interleaved stereo).
        let mono_len = samples.len() / 2;
        let take = FFT_SIZE.min(mono_len);
        let start = mono_len - take;

        // Fill FFT buffer directly: mono-mix + Hann window in one pass; zero-pad remainder.
        for i in 0..take {
            let l = samples[(start + i) * 2];
            let r = samples[(start + i) * 2 + 1];
            let s = (l + r) * 0.5;
            self.fft_buffer[i] = Complex::new(s * self.hann[i], 0.0);
        }
        for i in take..FFT_SIZE {
            self.fft_buffer[i] = Complex::new(0.0, 0.0);
        }

        self.fft.process(&mut self.fft_buffer);

        // Magnitude of positive-frequency bins.
        for (i, c) in self.fft_buffer[..self.magnitudes.len()].iter().enumerate() {
            self.magnitudes[i] = c.norm();
        }

        // Map to log-spaced bands using pre-computed ranges.
        for (i, &(lo, hi)) in self.band_bins.iter().enumerate() {
            let sum: f32 = self.magnitudes[lo..hi].iter().sum();
            self.bands[i] = sum / (hi - lo) as f32;
        }

        // Auto-gain.
        let frame_peak = self.bands.iter().cloned().fold(0.0f32, f32::max);
        self.global_peak = (self.global_peak * 0.9998).max(frame_peak).max(0.01);

        for v in &mut self.bands {
            *v = (*v / self.global_peak).min(1.0).sqrt();
        }

        // Fast attack, slower decay smoothing — decay tuned for 30 fps.
        // Coefficient 0.83 ≈ 0.91² so the perceived fall-off speed is
        // unchanged when the loop runs at half the original 60 Hz rate.
        for i in 0..NUM_BANDS {
            if self.bands[i] > self.prev_bands[i] {
                self.prev_bands[i] = self.bands[i];
            } else {
                self.prev_bands[i] = self.prev_bands[i] * 0.83 + self.bands[i] * 0.17;
            }
        }

        &self.prev_bands
    }
}
