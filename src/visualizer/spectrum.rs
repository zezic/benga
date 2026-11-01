use std::sync::Arc;

use super::detection::BinAddress;
use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use serde::{Deserialize, Serialize};

pub const BAND_COUNT: usize = 16;
/// About 21 ms at 48 kHz, with one decision every 5.3 ms. The former
/// 4096-point window spanned 85 ms and blurred adjacent attacks.
const FFT_SIZE: usize = 1024;
const HOP_SIZE: usize = 256;
const MIN_FREQUENCY: f32 = 32.0;
const MAX_FREQUENCY: f32 = 16_000.0;
const BASS_BANDS: usize = 4;
const TREBLE_BANDS: usize = 4;

/// One physical FFT bin as seen by a selected percussion detector.
#[derive(Clone, Copy, Debug, Default)]
pub struct OnsetBinMeter {
    pub address: BinAddress,
    pub frequency_hz: f32,
    pub low_hz: f32,
    pub high_hz: f32,
    pub selected: f32,
    pub gain: f32,
    pub magnitude: f32,
    pub weighted_magnitude: f32,
    /// Gain-weighted relative positive rise; a 35 ms hold is display-only.
    pub onset: f32,
}

/// Parameters of the per-bin relative spectral excess detector.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RelativeExcessSettings {
    /// Background envelope attack and release time constants in seconds.
    pub background_attack: f32,
    pub background_release: f32,
    /// Floor added to the background before the relative excess division.
    pub floor: f32,
    /// Upper bound of one bin's relative excess.
    pub cap: f32,
}

impl Default for RelativeExcessSettings {
    fn default() -> Self {
        Self {
            background_attack: 0.320,
            background_release: 0.200,
            floor: 0.010,
            cap: 1.0,
        }
    }
}

/// One analysis hop. `bands` is normalized for the visible scene parameters.
#[derive(Clone, Copy, Debug, Default)]
pub struct Spectrum {
    pub bands: [f32; BAND_COUNT],
    /// Linear RMS of the analysis window; a full-scale signal is 1.0.
    pub rms: f32,
    /// Broadband level of the analysis window.
    pub power: f32,
    pub bass: f32,
    pub treble: f32,
    /// Mean capped relative excess of each band's FFT bins.
    pub band_excess: [f32; BAND_COUNT],
    /// Linear, uncompressed magnitude of each logarithmic frequency region.
    /// Unlike visible band levels this can be normalized without gain changing shape.
    pub linear_bands: [f32; BAND_COUNT],
}

pub struct SpectrumAnalyzer {
    samplerate: usize,
    fft_size: usize,
    hop_size: usize,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    window_sum: f32,
    ring: Vec<[f32; 2]>,
    ring_position: usize,
    filled: usize,
    since_hop: usize,
    fft_buffer: Vec<Complex32>,
    fft_scratch: Vec<Complex32>,
    #[cfg(test)]
    band_bins: [(usize, usize); BAND_COUNT],
    band_weights: Vec<Vec<(usize, f32)>>,
    frequency_edges: [f32; BAND_COUNT + 1],
    bin_background: Vec<f32>,
    excess: RelativeExcessSettings,
}

impl SpectrumAnalyzer {
    pub fn new(samplerate: usize) -> Self {
        let target = (samplerate as f32 * FFT_SIZE as f32 / 48_000.0).round() as usize;
        let high = target.max(256).next_power_of_two();
        let size = if target.abs_diff(high / 2) < target.abs_diff(high) {
            high / 2
        } else {
            high
        };
        Self::with_fft_size(samplerate, size.clamp(256, 8192))
    }

    /// Allows the offline probe to compare time/frequency resolution on the
    /// same stream. Runtime capture uses the sample-rate-scaled default.
    pub fn with_fft_size(samplerate: usize, fft_size: usize) -> Self {
        assert!(samplerate > 0, "the spectrum needs a positive sample rate");
        assert!((256..=8192).contains(&fft_size) && fft_size.is_power_of_two());
        let fft = FftPlanner::<f32>::new().plan_fft_forward(fft_size);
        let fft_scratch = vec![Complex32::ZERO; fft.get_inplace_scratch_len()];
        let window: Vec<f32> = (0..fft_size)
            .map(|index| {
                let phase = std::f32::consts::TAU * index as f32 / fft_size as f32;
                0.5 - 0.5 * phase.cos()
            })
            .collect();
        let window_sum = window.iter().sum::<f32>();
        let (_band_bins, frequency_edges, band_weights) = Self::bands(samplerate, fft_size);
        Self {
            samplerate,
            fft_size,
            hop_size: ((samplerate as f32 * HOP_SIZE as f32 / 48_000.0).round() as usize).max(1),
            fft,
            window,
            window_sum,
            ring: vec![[0.0; 2]; fft_size],
            ring_position: 0,
            filled: 0,
            since_hop: 0,
            fft_buffer: vec![Complex32::ZERO; fft_size],
            fft_scratch,
            #[cfg(test)]
            band_bins: _band_bins,
            band_weights,
            frequency_edges,
            bin_background: vec![0.0; fft_size / 2],
            excess: RelativeExcessSettings::default(),
        }
    }

    fn bands(
        samplerate: usize,
        fft_size: usize,
    ) -> (
        [(usize, usize); BAND_COUNT],
        [f32; BAND_COUNT + 1],
        Vec<Vec<(usize, f32)>>,
    ) {
        let minimum = MIN_FREQUENCY;
        let maximum = MAX_FREQUENCY
            .min(samplerate as f32 * 0.45)
            .max(minimum * 2.0);
        let ratio = maximum / minimum;
        let edges =
            std::array::from_fn(|index| minimum * ratio.powf(index as f32 / BAND_COUNT as f32));
        let bin_hz = samplerate as f32 / fft_size as f32;
        let nyquist_bin = fft_size / 2;
        let mut bins = [(0, 1); BAND_COUNT];
        let mut weights = vec![Vec::new(); BAND_COUNT];
        for band in 0..BAND_COUNT {
            // Integrate the frequency interval represented by each FFT bin.
            // Narrow bass regions may share a bin but never update its history
            // twice, avoiding the old overlap-dependent background envelope.
            for bin in 1..nyquist_bin {
                let lo = (bin as f32 - 0.5) * bin_hz;
                let hi = (bin as f32 + 0.5) * bin_hz;
                let overlap = (hi.min(edges[band + 1]) - lo.max(edges[band])).max(0.0);
                if overlap > 0.0 {
                    weights[band].push((bin, overlap / bin_hz));
                }
            }
            if let (Some(&(start, _)), Some(&(end, _))) =
                (weights[band].first(), weights[band].last())
            {
                bins[band] = (start, end + 1);
            }
        }
        (bins, edges, weights)
    }

    pub fn set_samplerate(&mut self, samplerate: usize) {
        assert!(samplerate > 0, "the spectrum needs a positive sample rate");
        if self.samplerate == samplerate {
            return;
        }
        let excess = self.excess;
        *self = Self::new(samplerate);
        self.excess = excess;
    }

    pub fn reset(&mut self) {
        self.ring.fill([0.0; 2]);
        self.ring_position = 0;
        self.filled = 0;
        self.since_hop = 0;
        self.bin_background.fill(0.0);
    }

    pub fn set_relative_excess(&mut self, settings: RelativeExcessSettings) {
        assert!(
            settings.background_attack.is_finite()
                && settings.background_attack > 0.0
                && settings.background_release.is_finite()
                && settings.background_release > 0.0
                && settings.floor.is_finite()
                && settings.floor >= 0.0
                && settings.cap.is_finite()
                && settings.cap > 0.0,
            "relative excess settings must be positive and finite"
        );
        self.excess = settings;
    }

    pub fn samplerate(&self) -> usize {
        self.samplerate
    }

    pub fn hop_seconds(&self) -> f32 {
        self.hop_size as f32 / self.samplerate as f32
    }

    pub fn band_range(&self, index: usize) -> (f32, f32) {
        let index = index.min(BAND_COUNT - 1);
        (self.frequency_edges[index], self.frequency_edges[index + 1])
    }

    /// Feed one post-preamp mono sample. A hop returns the analyzed spectrum.
    #[cfg(test)]
    pub fn push(&mut self, sample: f32) -> Option<Spectrum> {
        self.push_stereo([sample, sample])
    }

    /// Stereo power analysis preserves percussion in wide/anti-phase mixes.
    /// Two real transforms are packed into one complex FFT.
    pub fn push_stereo(&mut self, sample: [f32; 2]) -> Option<Spectrum> {
        self.ring[self.ring_position] = sample.map(|v| if v.is_finite() { v } else { 0.0 });
        self.ring_position = (self.ring_position + 1) % self.fft_size;
        self.filled = (self.filled + 1).min(self.fft_size);
        self.since_hop += 1;
        if self.since_hop < self.hop_size {
            return None;
        }
        self.since_hop = 0;
        (self.filled == self.fft_size).then(|| self.analyze())
    }

    fn analyze(&mut self) -> Spectrum {
        let mut energy = 0.0;
        for index in 0..self.fft_size {
            let sample = self.ring[(self.ring_position + index) % self.fft_size];
            energy += (sample[0] * sample[0] + sample[1] * sample[1]) * 0.5;
            self.fft_buffer[index] = Complex32::new(
                sample[0] * self.window[index],
                sample[1] * self.window[index],
            );
        }
        self.fft
            .process_with_scratch(&mut self.fft_buffer, &mut self.fft_scratch);
        let norm = 2.0 / self.window_sum;
        let hop = self.hop_seconds();
        let attack_alpha = (-hop / self.excess.background_attack).exp();
        let release_alpha = (-hop / self.excess.background_release).exp();
        let bins = self.fft_size / 2;
        // Scratch lives in the FFT output's imaginary component after the transform.
        // All magnitudes are materialized before measuring overlapping bands.
        for bin in 0..bins {
            let direct = self.fft_buffer[bin];
            let mirrored = self.fft_buffer[(self.fft_size - bin) % self.fft_size].conj();
            let left = (direct + mirrored) * 0.5;
            let right = (direct - mirrored) * Complex32::new(0.0, -0.5);
            self.fft_buffer[bin].re = ((left.norm_sqr() + right.norm_sqr()) * 0.5).sqrt() * norm;
        }
        let mut bands = [0.0; BAND_COUNT];
        let mut band_excess = [0.0; BAND_COUNT];
        let mut linear_bands = [0.0; BAND_COUNT];
        for band in 0..BAND_COUNT {
            let (amplitude, excess) = self.measure_region(&self.band_weights[band]);
            linear_bands[band] = amplitude;
            bands[band] = normalize_db(amplitude, -72.0, 24.0);
            band_excess[band] = excess;
        }
        // One update per physical bin, including bins shared by bass regions.
        for bin in 0..bins {
            let magnitude = self.fft_buffer[bin].re;
            let alpha = if magnitude > self.bin_background[bin] {
                attack_alpha
            } else {
                release_alpha
            };
            self.bin_background[bin] = alpha * self.bin_background[bin] + (1.0 - alpha) * magnitude;
        }
        let rms = (energy / self.fft_size as f32).sqrt();
        let power = normalize_db(rms, -60.0, 12.0);
        let bass = bass_energy(&bands);
        let treble = bands[BAND_COUNT - TREBLE_BANDS..].iter().sum::<f32>() / TREBLE_BANDS as f32;
        Spectrum {
            bands,
            rms,
            power,
            bass,
            treble,
            band_excess,
            linear_bands,
        }
    }

    fn measure_region(&self, weights: &[(usize, f32)]) -> (f32, f32) {
        let mut power = 0.0;
        let mut excess = 0.0;
        let mut weight_sum = 0.0;
        for &(bin, weight) in weights {
            let magnitude = self.fft_buffer[bin].re;
            power += magnitude * magnitude * weight;
            let background = self.bin_background[bin];
            excess += ((magnitude - background) / (background + self.excess.floor).max(1e-9))
                .clamp(0.0, self.excess.cap)
                * weight;
            weight_sum += weight;
        }
        (power.sqrt(), excess / weight_sum.max(1e-9))
    }

    #[cfg(test)]
    pub(crate) fn frequency_edges(&self) -> &[f32; BAND_COUNT + 1] {
        &self.frequency_edges
    }

    #[cfg(test)]
    pub(crate) fn band_bins(&self) -> &[(usize, usize); BAND_COUNT] {
        &self.band_bins
    }
}

fn bass_energy(bands: &[f32; BAND_COUNT]) -> f32 {
    bands[..BASS_BANDS]
        .iter()
        .copied()
        .fold(0.0, f32::max)
        .clamp(0.0, 1.0)
}

fn normalize_db(amplitude: f32, floor_db: f32, ceiling_db: f32) -> f32 {
    let db = 20.0 * amplitude.max(1e-9).log10();
    ((db - floor_db) / (ceiling_db - floor_db)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_tone(analyzer: &mut SpectrumAnalyzer, frequency: f32, samples: usize) -> Spectrum {
        let mut last = Spectrum::default();
        for sample in 0..samples {
            let phase = std::f32::consts::TAU * frequency * sample as f32 / 48_000.0;
            if let Some(spectrum) = analyzer.push(phase.sin()) {
                last = spectrum;
            }
        }
        last
    }

    fn expected_band(analyzer: &SpectrumAnalyzer, frequency: f32) -> usize {
        analyzer
            .frequency_edges()
            .windows(2)
            .position(|edge| edge[0] <= frequency && frequency < edge[1])
            .expect("the tone lies inside the analyzed range")
    }

    #[test]
    fn log_bands_place_tones_in_the_matching_area() {
        for frequency in [80.0, 440.0, 1_000.0, 6_000.0, 12_000.0] {
            let mut analyzer = SpectrumAnalyzer::new(48_000);
            let spectrum = feed_tone(&mut analyzer, frequency, 24_000);
            let expected = expected_band(&analyzer, frequency);
            let strongest = spectrum
                .bands
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.total_cmp(b))
                .map(|(index, _)| index)
                .unwrap();
            assert_eq!(
                strongest, expected,
                "{frequency} Hz landed in band {strongest} instead of {expected}"
            );
            assert!(spectrum.bands[expected] > 0.6);
            assert!(spectrum.rms > 0.6);
            assert!(spectrum.power > 0.7);
        }
    }

    #[test]
    fn relative_excess_caps_on_a_rise_and_decays_into_the_background() {
        let mut analyzer = SpectrumAnalyzer::new(48_000);
        analyzer.set_relative_excess(RelativeExcessSettings {
            background_attack: 0.05,
            background_release: 0.05,
            cap: 0.5,
            ..RelativeExcessSettings::default()
        });
        for _ in 0..FFT_SIZE {
            analyzer.push(0.0);
        }
        let band = expected_band(&analyzer, 40.0);
        let mut peak = 0.0_f32;
        let mut last = 0.0_f32;
        for sample in 0..48_000 {
            let phase = std::f32::consts::TAU * 40.0 * sample as f32 / 48_000.0;
            if let Some(spectrum) = analyzer.push(phase.sin()) {
                peak = peak.max(spectrum.band_excess[band]);
                last = spectrum.band_excess[band];
            }
        }
        assert!((peak - 0.5).abs() < 1e-5, "the ratio cap bounds the rise");
        assert!(last < 0.05, "a held tone sinks into the background: {last}");
    }

    #[test]
    fn log_bands_are_ordered_and_cover_the_audio_range() {
        let analyzer = SpectrumAnalyzer::new(48_000);
        let edges = analyzer.frequency_edges();
        assert_eq!(edges[0], MIN_FREQUENCY);
        assert!(edges[BAND_COUNT] <= MAX_FREQUENCY);
        for pair in edges.windows(2) {
            assert!(pair[0] < pair[1]);
        }
        for (band, &(start, end)) in analyzer.band_bins().iter().enumerate() {
            assert!(start >= 1, "band {band} includes DC");
            assert!(start < end, "band {band} needs at least one bin");
            if band > 0 {
                assert!(
                    start >= analyzer.band_bins()[band - 1].0,
                    "band {band} overlaps its lower neighbour"
                );
            }
        }
    }

    #[test]
    fn silence_after_a_tone_returns_bands_to_rest() {
        let mut analyzer = SpectrumAnalyzer::new(48_000);
        let tone = feed_tone(&mut analyzer, 1_000.0, 24_000);
        assert!(tone.bands.iter().any(|&band| band > 0.6));
        let mut silence = Spectrum::default();
        for _ in 0..12_000 {
            if let Some(spectrum) = analyzer.push(0.0) {
                silence = spectrum;
            }
        }
        assert!(silence.bands.iter().all(|&band| band < 0.01));
        assert!(silence.rms < 0.01);
        assert!(silence.power < 0.01);
        assert_eq!(silence.bass, 0.0);
    }

    #[test]
    fn stereo_power_preserves_antiphase_audio_and_matches_mono() {
        let mut mono = SpectrumAnalyzer::new(48_000);
        let mut stereo = SpectrumAnalyzer::new(48_000);
        let mut left = Spectrum::default();
        let mut right = Spectrum::default();
        for frame in 0..12_000 {
            let sample = (frame as f32 * std::f32::consts::TAU * 140.0 / 48_000.0).sin() * 0.5;
            if let Some(spectrum) = mono.push(sample) {
                left = spectrum;
            }
            if let Some(spectrum) = stereo.push_stereo([sample, -sample]) {
                right = spectrum;
            }
        }
        assert!((left.rms - right.rms).abs() < 1e-6);
        for (mono, anti) in left.linear_bands.iter().zip(right.linear_bands) {
            assert!((mono - anti).abs() < 1e-5);
        }
    }

    #[test]
    fn invalid_capture_samples_do_not_poison_future_analysis() {
        let mut analyzer = SpectrumAnalyzer::new(48_000);
        for _ in 0..2000 {
            analyzer.push_stereo([f32::NAN, f32::INFINITY]);
        }
        let clean = feed_tone(&mut analyzer, 1000.0, 12_000);
        assert!(clean.rms.is_finite() && clean.rms > 0.6);
        assert!(clean.linear_bands.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn default_window_scales_with_capture_rate() {
        for rate in [8000, 44_100, 48_000, 96_000, 192_000, 384_000] {
            let analyzer = SpectrumAnalyzer::new(rate);
            let seconds = analyzer.fft_size as f32 / rate as f32;
            assert!((0.015..=0.0321).contains(&seconds), "{rate}: {seconds}s");
            assert!((analyzer.hop_seconds() - 256.0 / 48_000.0).abs() < 0.0001);
        }
    }
}
