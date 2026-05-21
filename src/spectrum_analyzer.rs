use std::sync::{Arc, Mutex};
use librespot::playback::audio_backend::{Sink, SinkResult};
use librespot::playback::convert::Converter;
use librespot::playback::decoder::AudioPacket;

pub const NUM_BANDS: usize = 24;
pub const FFT_SIZE: usize = 1024;
const SAMPLE_RATE: f32 = 44100.0;
const F_MIN: f32 = 40.0;
const F_MAX: f32 = 16000.0;

// Wraps the real audio backend sink; intercepts PCM samples for FFT analysis.
pub struct CaptureSink {
    pub inner: Box<dyn Sink>,
    pub buffer: Arc<Mutex<Vec<f32>>>,
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
                for &s in samples.iter() {
                    buf.push(s as f32);
                }
                // Keep ~2s at 44100 Hz stereo (2 ch * 44100 * 2 = 176400)
                let max_len = 176_400;
                if buf.len() > max_len {
                    let excess = buf.len() - max_len;
                    buf.drain(0..excess);
                }
            }
        }
        self.inner.write(packet, converter)
    }
}

pub struct SpectrumAnalyzer {
    fft: Arc<dyn rustfft::Fft<f32>>,
    prev_bands: Vec<f32>,
    global_peak: f32,
}

impl SpectrumAnalyzer {
    pub fn new() -> Self {
        let mut planner = rustfft::FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        Self {
            fft,
            prev_bands: vec![0.0; NUM_BANDS],
            global_peak: 1.0,
        }
    }

    pub fn reset(&mut self) {
        self.prev_bands.fill(0.0);
    }

    /// Computes smoothed frequency band amplitudes (0.0–1.0) from raw PCM samples.
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        use rustfft::num_complex::Complex;

        // Take the most recent FFT_SIZE samples (librespot is stereo; treat as mono by averaging)
        let mono_len = samples.len() / 2;
        let start = mono_len.saturating_sub(FFT_SIZE);
        let mono_chunk: Vec<f32> = (start..start.min(mono_len) + FFT_SIZE.min(mono_len - start))
            .map(|i| {
                let l = *samples.get(i * 2).unwrap_or(&0.0);
                let r = *samples.get(i * 2 + 1).unwrap_or(&0.0);
                (l + r) * 0.5
            })
            .collect();

        // Hann window + zero-pad to FFT_SIZE
        let mut buffer: Vec<Complex<f32>> = (0..FFT_SIZE)
            .map(|i| {
                let s = mono_chunk.get(i).copied().unwrap_or(0.0);
                let w = 0.5 * (1.0 - (std::f32::consts::TAU * i as f32 / (FFT_SIZE - 1) as f32).cos());
                Complex::new(s * w, 0.0)
            })
            .collect();

        self.fft.process(&mut buffer);

        // Magnitude of positive-frequency bins only
        let magnitudes: Vec<f32> = buffer[..FFT_SIZE / 2]
            .iter()
            .map(|c| c.norm())
            .collect();

        // Map to NUM_BANDS log-spaced bands
        let mut bands = vec![0.0f32; NUM_BANDS];
        for i in 0..NUM_BANDS {
            let f_low = F_MIN * (F_MAX / F_MIN).powf(i as f32 / NUM_BANDS as f32);
            let f_high = F_MIN * (F_MAX / F_MIN).powf((i + 1) as f32 / NUM_BANDS as f32);
            let bin_low = ((f_low / SAMPLE_RATE) * FFT_SIZE as f32) as usize;
            let bin_high = ((f_high / SAMPLE_RATE) * FFT_SIZE as f32) as usize;
            let bin_low = bin_low.min(magnitudes.len().saturating_sub(1));
            let bin_high = bin_high.min(magnitudes.len()).max(bin_low + 1);

            let sum: f32 = magnitudes[bin_low..bin_high].iter().sum();
            bands[i] = sum / (bin_high - bin_low) as f32;
        }

        // Auto-gain: track a slowly-decaying global peak so bars always fill height
        let frame_peak = bands.iter().cloned().fold(0.0f32, f32::max);
        self.global_peak = (self.global_peak * 0.9998).max(frame_peak).max(0.01);

        // Normalize, apply sqrt curve (lifts quieter signals), clamp
        for v in &mut bands {
            *v = (*v / self.global_peak).min(1.0).sqrt();
        }

        // Fast attack, slower decay smoothing — decay tuned for 60 fps
        for i in 0..NUM_BANDS {
            if bands[i] > self.prev_bands[i] {
                self.prev_bands[i] = bands[i];
            } else {
                self.prev_bands[i] = self.prev_bands[i] * 0.91 + bands[i] * 0.09;
            }
        }

        self.prev_bands.clone()
    }
}
