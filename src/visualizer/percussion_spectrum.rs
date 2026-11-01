//! Independent percussion front end. Arrangement analysis keeps its original
//! transform, power scale, startup fill and hop cadence.
//!
//! Left and right have independent transforms and power spectra. Combining
//! physical powers is safe for level displays; attack movement is rectified on
//! each side first so a falling side cannot mask a rising side.
//!
//! The fast transform is configurable (256/512 samples at 48 kHz). Kick uses a
//! separate 2048 sample Hann window: zero padding a short transform would not
//! resolve its 60–200 Hz region. Every reported timestamp is availability time,
//! including window fill; no group delay is subtracted from the audio clock.

use std::sync::Arc;

use rustfft::{num_complex::Complex32, Fft, FftPlanner};

use super::detection::{BinAddress, PercussionBinGains};
use super::percussion::PercussionSettings;
use super::spectrum::OnsetBinMeter;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PercussionFrame {
    /// Window-weighted stereo mean-square regional power (FS²).
    pub power: [f32; 3],
    /// Separate normalized [left, right] regional powers. The onset path keeps
    /// these separate until it has measured each side's positive movement.
    pub stereo_power: [[f32; 2]; 3],
    pub available: [bool; 3],
    /// Fast-window broadband RMS, independent of instrument masks/faders.
    pub rms: f32,
    pub dt: f32,
    pub revisions: [u64; 3],
    pub at: f64,
    pub window_seconds: [f32; 3],
    pub bin_hz: [f32; 3],
}

struct Transform {
    size: usize,
    ring: Vec<[f32; 2]>,
    invalid: Vec<bool>,
    invalid_count: usize,
    position: usize,
    filled: usize,
    window: Vec<f32>,
    normalization: f32,
    fft: Arc<dyn Fft<f32>>,
    buffers: [Vec<Complex32>; 2],
    scratch: Vec<Complex32>,
    stereo_power: Vec<[f32; 2]>,
    power: Vec<f32>,
    broadband_power: f32,
}

impl Transform {
    fn new(size: usize) -> Self {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(size);
        let scratch = vec![Complex32::ZERO; fft.get_inplace_scratch_len()];
        let window: Vec<_> = (0..size)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / size as f32).cos())
            .collect();
        let normalization = 1.0 / (size as f32 * window.iter().map(|h| h * h).sum::<f32>());
        Self {
            size,
            ring: vec![[0.0; 2]; size],
            invalid: vec![false; size],
            invalid_count: 0,
            position: 0,
            filled: 0,
            window,
            normalization,
            fft,
            buffers: std::array::from_fn(|_| vec![Complex32::ZERO; size]),
            scratch,
            stereo_power: vec![[0.0; 2]; size / 2 + 1],
            power: vec![0.0; size / 2 + 1],
            broadband_power: 0.0,
        }
    }

    fn push(&mut self, sample: [f32; 2]) {
        self.invalid_count -= usize::from(self.invalid[self.position]);
        let invalid = sample.iter().any(|v| !v.is_finite());
        self.invalid[self.position] = invalid;
        self.invalid_count += usize::from(invalid);
        self.ring[self.position] = sample.map(|v| if v.is_finite() { v } else { 0.0 });
        self.position = (self.position + 1) % self.size;
        self.filled = (self.filled + 1).min(self.size);
    }

    fn ready(&self) -> bool {
        self.filled == self.size && self.invalid_count == 0
    }

    fn analyze(&mut self) {
        for index in 0..self.size {
            let sample = self.ring[(self.position + index) % self.size];
            for (side, &value) in sample.iter().enumerate() {
                self.buffers[side][index] = Complex32::new(value * self.window[index], 0.0);
            }
        }
        // One transform per real channel. The plan and scratch are reused;
        // neither the FFT nor the following movement analysis allocates a hop.
        for buffer in &mut self.buffers {
            self.fft.process_with_scratch(buffer, &mut self.scratch);
        }
        self.broadband_power = 0.0;
        for bin in 0..=self.size / 2 {
            let sided = if bin == 0 || bin == self.size / 2 {
                1.0
            } else {
                2.0
            };
            let side_power = self
                .buffers
                .each_ref()
                .map(|buffer| buffer[bin].norm_sqr() * sided * self.normalization);
            self.stereo_power[bin] = side_power;
            // This average is energy/display information only. Separate side
            // powers remain available to the onset features below.
            let power = (side_power[0] + side_power[1]) * 0.5;
            self.power[bin] = power;
            self.broadband_power += power;
        }
    }

    fn reset(&mut self) {
        self.ring.fill([0.0; 2]);
        self.invalid.fill(false);
        self.invalid_count = 0;
        self.position = 0;
        self.filled = 0;
        self.stereo_power.fill([0.0; 2]);
        self.power.fill(0.0);
        self.broadband_power = 0.0;
    }
}

/// Rate scaling preserves the physical window to the nearest power-of-two FFT.
/// 44.1 kHz uses the 48 kHz size, while 96 kHz doubles it.
fn scaled_size(size_at_48k: usize, sample_rate: usize) -> usize {
    let target = (size_at_48k as f64 * sample_rate as f64 / 48_000.0).round() as usize;
    let upper = target.max(64).next_power_of_two();
    if target.abs_diff(upper / 2) < target.abs_diff(upper) {
        (upper / 2).max(64)
    } else {
        upper
    }
}

/// Integral of the *power* mask over a bin's physical frequency support.
/// Dividing by the actual support width keeps DC/Nyquist endpoint bins honest.
fn mask_weight(bin_low: f32, bin_high: f32, low: f32, high: f32, taper: f32) -> f32 {
    let width = bin_high - bin_low;
    if width <= 0.0 || high <= low || bin_high <= low || bin_low >= taper.max(high) {
        return 0.0;
    }
    let mut integral = (bin_high.min(high) - bin_low.max(low)).max(0.0);
    if taper > high {
        let a = bin_low.max(high);
        let b = bin_high.min(taper);
        if b > a {
            let d = taper - high;
            integral += 0.5 * (b - a)
                + 0.5 * d / std::f32::consts::PI
                    * ((std::f32::consts::PI * (b - high) / d).sin()
                        - (std::f32::consts::PI * (a - high) / d).sin());
        }
    }
    (integral / width).clamp(0.0, 1.0)
}

pub(super) struct PercussionAnalyzer {
    sample_rate: usize,
    hop_frames: usize,
    since_hop: usize,
    consumed_frames: u64,
    fast: Transform,
    kick: Transform,
    settings: PercussionSettings,
    gains: PercussionBinGains,
    weights: [Vec<f32>; 3],
    meters: [Vec<OnsetBinMeter>; 3],
    previous_power: [Vec<[f32; 2]>; 3],
    revisions: [u64; 3],
}

impl PercussionAnalyzer {
    pub fn new(sample_rate: usize) -> Self {
        assert!(sample_rate > 0);
        let settings = PercussionSettings::default();
        let fast = Transform::new(scaled_size(settings.fft_size, sample_rate));
        let kick = Transform::new(scaled_size(2048, sample_rate));
        let mut analyzer = Self {
            sample_rate,
            hop_frames: ((sample_rate as f64 * f64::from(settings.hop_ms) / 1000.0).round()
                as usize)
                .max(1),
            since_hop: 0,
            consumed_frames: 0,
            fast,
            kick,
            settings,
            gains: PercussionBinGains::default(),
            weights: std::array::from_fn(|_| Vec::new()),
            meters: std::array::from_fn(|_| Vec::new()),
            previous_power: std::array::from_fn(|_| Vec::new()),
            revisions: [0; 3],
        };
        for channel in 0..3 {
            analyzer.rebuild(channel);
        }
        analyzer
    }

    fn rebuild(&mut self, channel: usize) {
        let grid = if channel == 0 { &self.kick } else { &self.fast };
        let bin_hz = self.sample_rate as f32 / grid.size as f32;
        let nyquist = self.sample_rate as f32 * 0.5;
        let config = &self.settings.channels[channel];
        self.weights[channel].clear();
        self.meters[channel].clear();
        self.previous_power[channel] = vec![[0.0; 2]; grid.power.len()];
        for bin in 0..grid.power.len() {
            let center = bin as f32 * bin_hz;
            let low = (center - bin_hz * 0.5).max(0.0);
            let high = (center + bin_hz * 0.5).min(nyquist);
            // No support above Nyquist means unavailable; never move a high
            // region down to the top remaining bin at a low sample rate.
            let selected = mask_weight(low, high, config.low_hz, config.high_hz, config.taper_hz);
            let gain = self.gains.profile(channel).gain_at(center);
            self.weights[channel].push(selected * gain * gain);
            self.meters[channel].push(OnsetBinMeter {
                address: BinAddress::new(center, bin_hz),
                frequency_hz: center,
                low_hz: low,
                high_hz: high,
                selected,
                gain,
                ..OnsetBinMeter::default()
            });
        }
    }

    pub fn set_settings(&mut self, settings: &PercussionSettings) {
        let fast_changed = settings.fft_size != self.settings.fft_size;
        let hop_changed = settings.hop_ms != self.settings.hop_ms;
        let changed: [bool; 3] = std::array::from_fn(|channel| {
            let a = &settings.channels[channel];
            let b = &self.settings.channels[channel];
            a.low_hz != b.low_hz
                || a.high_hz != b.high_hz
                || a.taper_hz != b.taper_hz
                || hop_changed
                || (channel != 0 && fast_changed)
        });
        self.settings = *settings;
        if fast_changed {
            self.fast = Transform::new(scaled_size(settings.fft_size, self.sample_rate));
        }
        if hop_changed {
            self.hop_frames = ((self.sample_rate as f64 * f64::from(settings.hop_ms) / 1000.0)
                .round() as usize)
                .max(1);
            self.since_hop = 0;
        }
        for (channel, changed) in changed.into_iter().enumerate() {
            if changed {
                self.revisions[channel] = self.revisions[channel].wrapping_add(1);
                self.rebuild(channel);
            }
        }
    }

    pub fn set_bin_gains(&mut self, gains: &PercussionBinGains) {
        let changed: [bool; 3] =
            std::array::from_fn(|channel| self.gains.profile(channel) != gains.profile(channel));
        let affected: [bool; 3] = std::array::from_fn(|channel| {
            self.meters[channel].iter().any(|meter| {
                meter.selected > 0.0
                    && meter.gain != gains.profile(channel).gain_at(meter.frequency_hz)
            })
        });
        self.gains = gains.clone();
        for (channel, changed) in changed.into_iter().enumerate() {
            if changed {
                if affected[channel] {
                    self.revisions[channel] = self.revisions[channel].wrapping_add(1);
                }
                self.rebuild(channel);
            }
        }
    }

    pub fn reset(&mut self) {
        self.reset_features();
        self.consumed_frames = 0;
    }

    /// Start a fresh feature epoch after a known input gain change, preserving
    /// consumed audio time and the independent arrangement stream.
    pub fn reset_features(&mut self) {
        self.fast.reset();
        self.kick.reset();
        self.since_hop = 0;
        for channel in 0..3 {
            self.revisions[channel] = self.revisions[channel].wrapping_add(1);
            self.previous_power[channel].fill([0.0; 2]);
            for meter in &mut self.meters[channel] {
                meter.magnitude = 0.0;
                meter.weighted_magnitude = 0.0;
                meter.onset = 0.0;
            }
        }
    }

    pub fn samplerate(&self) -> usize {
        self.sample_rate
    }
    pub fn audio_time(&self) -> f64 {
        self.consumed_frames as f64 / self.sample_rate as f64
    }
    pub fn hop_seconds(&self) -> f32 {
        self.hop_frames as f32 / self.sample_rate as f32
    }
    pub fn meters(&self, channel: usize) -> &[OnsetBinMeter] {
        &self.meters[channel]
    }

    pub fn push_stereo(&mut self, sample: [f32; 2]) -> Option<PercussionFrame> {
        self.fast.push(sample);
        self.kick.push(sample);
        self.consumed_frames = self.consumed_frames.saturating_add(1);
        self.since_hop += 1;
        if self.since_hop < self.hop_frames {
            return None;
        }
        self.since_hop = 0;
        if self.fast.ready() {
            self.fast.analyze();
        }
        if self.kick.ready() {
            self.kick.analyze();
        }
        let mut frame = PercussionFrame {
            dt: self.hop_seconds(),
            at: self.consumed_frames as f64 / self.sample_rate as f64,
            revisions: self.revisions,
            rms: if self.fast.ready() {
                self.fast.broadband_power.sqrt()
            } else {
                0.0
            },
            window_seconds: [
                self.kick.size as f32 / self.sample_rate as f32,
                self.fast.size as f32 / self.sample_rate as f32,
                self.fast.size as f32 / self.sample_rate as f32,
            ],
            bin_hz: [
                self.sample_rate as f32 / self.kick.size as f32,
                self.sample_rate as f32 / self.fast.size as f32,
                self.sample_rate as f32 / self.fast.size as f32,
            ],
            ..PercussionFrame::default()
        };
        let decay = (-frame.dt / 0.035).exp();
        for channel in 0..3 {
            let grid = if channel == 0 { &self.kick } else { &self.fast };
            let support = self.weights[channel].iter().any(|&weight| weight > 0.0);
            frame.available[channel] = grid.ready() && support;
            if !frame.available[channel] {
                continue;
            }
            let mut stereo_power = [0.0; 2];
            for (bin, meter) in self.meters[channel].iter_mut().enumerate() {
                let raw = grid.power[bin];
                let weighted =
                    grid.stereo_power[bin].map(|power| power * self.weights[channel][bin]);
                for (total, value) in stereo_power.iter_mut().zip(weighted) {
                    *total += value;
                }
                meter.magnitude = raw.sqrt();
                meter.weighted_magnitude = ((weighted[0] + weighted[1]) * 0.5).sqrt();
                let rise = stereo_positive_rise(weighted, self.previous_power[channel][bin]);
                meter.onset = rise.max(meter.onset * decay);
                self.previous_power[channel][bin] = weighted;
            }
            frame.stereo_power[channel] = stereo_power;
            frame.power[channel] = (stereo_power[0] + stereo_power[1]) * 0.5;
        }
        Some(frame)
    }
}

/// Combine movement only after rectifying each side's amplitude change. A fall
/// on one side cannot cancel a simultaneous rise on the other. RMS preserves
/// the established mono scale and the 3 dB energy difference of a hard pan.
fn stereo_positive_rise(current: [f32; 2], previous: [f32; 2]) -> f32 {
    let growth: [f32; 2] =
        std::array::from_fn(|side| (current[side].sqrt() - previous[side].sqrt()).max(0.0));
    ((growth[0] * growth[0] + growth[1] * growth[1]) * 0.5).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(
        sample_rate: usize,
        frequency: f32,
        amplitude: f32,
        phase: f32,
        anti: bool,
        fast_size: usize,
    ) -> Vec<PercussionFrame> {
        let mut analyzer = PercussionAnalyzer::new(sample_rate);
        let settings = PercussionSettings {
            fft_size: fast_size,
            ..PercussionSettings::default()
        };
        analyzer.set_settings(&settings);
        (0..sample_rate / 5)
            .filter_map(|n| {
                let value = amplitude
                    * (std::f32::consts::TAU * frequency * n as f32 / sample_rate as f32 + phase)
                        .sin();
                analyzer.push_stereo([value, if anti { -value } else { value }])
            })
            .filter(|frame| frame.at > 0.1)
            .collect()
    }

    #[test]
    fn power_mask_is_integrated_and_taper_is_a_power_weight() {
        assert_eq!(mask_weight(100.0, 150.0, 60.0, 200.0, 200.0), 1.0);
        assert_eq!(mask_weight(200.0, 250.0, 60.0, 200.0, 200.0), 0.0);
        assert!((mask_weight(190.0, 210.0, 60.0, 200.0, 200.0) - 0.5).abs() < 1e-6);
        assert!((mask_weight(5000.0, 10000.0, 2000.0, 5000.0, 10000.0) - 0.5).abs() < 1e-6);
        assert_eq!(mask_weight(3500.0, 4000.0, 5000.0, 16000.0, 16000.0), 0.0);
    }

    #[test]
    fn stereo_parseval_normalization_matches_windowed_time_power() {
        for size in [256, 512, 1024, 2048] {
            let mut transform = Transform::new(size);
            for n in 0..size {
                transform.push([
                    (n as f32 * 0.317).sin() * 0.3,
                    (n as f32 * 0.183).cos() * 0.6,
                ]);
            }
            transform.analyze();
            let expected = transform
                .ring
                .iter()
                .zip(&transform.window)
                .map(|(v, h)| (v[0] * v[0] + v[1] * v[1]) * 0.5 * h * h)
                .sum::<f32>()
                / transform.window.iter().map(|h| h * h).sum::<f32>();
            assert!(
                (transform.broadband_power - expected).abs() < 2e-6,
                "size={size}"
            );
            for side in 0..2 {
                let expected = transform
                    .ring
                    .iter()
                    .zip(&transform.window)
                    .map(|(sample, h)| sample[side] * sample[side] * h * h)
                    .sum::<f32>()
                    / transform.window.iter().map(|h| h * h).sum::<f32>();
                let measured = transform
                    .stereo_power
                    .iter()
                    .map(|power| power[side])
                    .sum::<f32>();
                assert!(
                    (measured - expected).abs() < 2e-6,
                    "size={size}, side={side}"
                );
            }
        }
    }

    #[test]
    fn explicit_channel_transforms_match_two_real_dfts_from_the_old_packed_transform() {
        for size in [256, 512, 2048] {
            let mut transform = Transform::new(size);
            let mut random = 0x4517_9823_u32;
            for n in 0..size {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                let noise = random as f32 / u32::MAX as f32 - 0.5;
                transform.push([
                    (n as f32 * 0.317).sin() * 0.3 + noise * 0.1,
                    (n as f32 * 0.183).cos() * 0.6 - noise * 0.05,
                ]);
            }
            let mut packed = (0..size)
                .map(|index| {
                    let sample = transform.ring[(transform.position + index) % size];
                    Complex32::new(
                        sample[0] * transform.window[index],
                        sample[1] * transform.window[index],
                    )
                })
                .collect::<Vec<_>>();
            transform
                .fft
                .process_with_scratch(&mut packed, &mut transform.scratch);
            transform.analyze();
            for bin in 0..=size / 2 {
                let direct = packed[bin];
                let mirror = packed[(size - bin) % size].conj();
                let channels = [
                    (direct + mirror) * 0.5,
                    (direct - mirror) * Complex32::new(0.0, -0.5),
                ];
                let sided = if bin == 0 || bin == size / 2 {
                    1.0
                } else {
                    2.0
                };
                for (side, expected) in channels
                    .map(|value| value.norm_sqr() * sided * transform.normalization)
                    .into_iter()
                    .enumerate()
                {
                    assert!(
                        (transform.stereo_power[bin][side] - expected).abs() < 2e-7,
                        "size={size}, bin={bin}, side={side}"
                    );
                }
            }
        }
    }

    #[test]
    fn channel_power_is_independent_of_phase_and_has_no_pan_or_width_bonus() {
        let rate = 48000;
        for frequency in [120.0, 3000.0, 12000.0] {
            let run = |pan: [f32; 2], invert: bool| {
                let mut analyzer = PercussionAnalyzer::new(rate);
                (0..rate / 5)
                    .filter_map(|n| {
                        let value = 0.4
                            * (std::f32::consts::TAU * frequency * n as f32 / rate as f32).sin();
                        analyzer.push_stereo([
                            value * pan[0],
                            value * pan[1] * if invert { -1.0 } else { 1.0 },
                        ])
                    })
                    .last()
                    .unwrap()
            };
            let mono = run([1.0, 1.0], false);
            let anti = run([1.0, 1.0], true);
            let left = run([1.0, 0.0], false);
            let right = run([0.0, 1.0], false);
            let equal_energy_pan = run([std::f32::consts::SQRT_2, 0.0], false);
            for channel in 0..3 {
                let tolerance = 2e-7;
                assert!((mono.power[channel] - anti.power[channel]).abs() < tolerance);
                assert!((left.power[channel] - mono.power[channel] * 0.5).abs() < tolerance);
                assert_eq!(left.stereo_power[channel][1], 0.0);
                assert_eq!(right.stereo_power[channel][0], 0.0);
                assert!(
                    (left.stereo_power[channel][0] - right.stereo_power[channel][1]).abs()
                        < tolerance
                );
                assert!((mono.power[channel] - equal_energy_pan.power[channel]).abs() < tolerance);
            }
        }
    }

    #[test]
    fn positive_channel_movement_is_combined_only_after_rectification() {
        let previous = [0.09, 0.01];
        let current = [0.01, 0.09];
        let previous_mix = ((previous[0] + previous[1]) * 0.5_f32).sqrt();
        let current_mix = ((current[0] + current[1]) * 0.5_f32).sqrt();
        assert_eq!(
            current_mix - previous_mix,
            0.0,
            "early power mixing loses the new right attack"
        );
        let movement = stereo_positive_rise(current, previous);
        assert!((movement - 0.2 / std::f32::consts::SQRT_2).abs() < 1e-7);
        assert_eq!(
            movement,
            stereo_positive_rise([current[1], current[0]], [previous[1], previous[0]])
        );
        assert!((stereo_positive_rise([0.09; 2], [0.01; 2]) - 0.2).abs() < 1e-7);
        assert_eq!(stereo_positive_rise([0.01; 2], [0.09; 2]), 0.0);
    }

    #[test]
    fn real_fft_side_transfer_retains_movement_when_mixed_bin_power_is_unchanged() {
        let size = 512;
        let mut transform = Transform::new(size);
        let mut sides = [[0.0; 2]; 2];
        for (index, side) in [0, 1].into_iter().enumerate() {
            for n in 0..size {
                let mut sample = [0.0; 2];
                sample[side] = 0.4 * (std::f32::consts::TAU * 32.0 * n as f32 / size as f32).sin();
                transform.push(sample);
            }
            transform.analyze();
            sides[index] = transform.stereo_power[32];
        }
        assert_eq!(
            (sides[0][0] + sides[0][1]) * 0.5,
            (sides[1][0] + sides[1][1]) * 0.5
        );
        assert!(stereo_positive_rise(sides[1], sides[0]) > 0.1);
    }

    #[test]
    fn stationary_power_gain_phase_and_sample_rate_contract() {
        for sample_rate in [44100, 48000, 96000] {
            for fast_size in [256, 512] {
                for (channel, frequency) in [(0, 120.0), (1, 3000.0), (2, 12000.0)] {
                    let unity = tone(sample_rate, frequency, 0.4, 0.0, false, fast_size);
                    let phased = tone(sample_rate, frequency, 0.4, 1.7, true, fast_size);
                    let quieter = tone(sample_rate, frequency, 0.2, 0.0, false, fast_size);
                    for ((a, b), c) in unity.iter().zip(&phased).zip(&quieter) {
                        assert!(a.available[channel]);
                        assert!(
                            (a.power[channel] - 0.08).abs() < 0.006,
                            "sr={sample_rate} N={fast_size} ch={channel} P={}",
                            a.power[channel]
                        );
                        assert!((a.power[channel] - b.power[channel]).abs() < 0.002);
                        assert!((a.power[channel] * 0.25 - c.power[channel]).abs() < 1e-6);
                    }
                }
            }
        }
    }

    #[test]
    fn low_rate_masks_truncate_without_moving_down() {
        for sample_rate in [8000, 16000] {
            let mut analyzer = PercussionAnalyzer::new(sample_rate);
            let last = (0..sample_rate / 5)
                .filter_map(|n| analyzer.push_stereo([(n as f32 * 0.73).sin() * 0.2; 2]))
                .last()
                .unwrap();
            assert_eq!(last.available[2], sample_rate == 16000);
            if sample_rate == 8000 {
                assert_eq!(last.power[2], 0.0);
                assert!(analyzer.meters(2).iter().all(|m| m.selected == 0.0));
            }
            assert!(last.available[1]);
            assert!(analyzer
                .meters(1)
                .iter()
                .all(|m| m.high_hz <= sample_rate as f32 * 0.5));
        }
    }

    #[test]
    fn kick_selectivity_does_not_claim_short_fft_resolution() {
        for phase in [0.0, 0.4, 1.3, 2.7] {
            for frequency in [60.0, 80.0, 120.0, 180.0] {
                let frames = tone(48000, frequency, 0.4, phase, false, 256);
                let min = frames
                    .iter()
                    .map(|f| f.power[0])
                    .fold(f32::INFINITY, f32::min);
                let max = frames.iter().map(|f| f.power[0]).fold(0.0, f32::max);
                assert!(
                    (max - min) / max < 0.07,
                    "f={frequency} phase={phase} {min}..{max}"
                );
                assert_eq!(frames[0].bin_hz[0], 23.4375);
            }
        }
        for frequency in [15.0, 350.0, 500.0, 1000.0] {
            let frames = tone(48000, frequency, 0.4, 0.5, false, 512);
            assert!(frames.iter().all(|f| f.power[0] < 0.003), "f={frequency}");
        }
    }

    #[test]
    fn known_energy_integrals_use_hop_duration_once() {
        for sample_rate in [44100, 48000, 96000] {
            let frames = tone(sample_rate, 3000.0, 0.4, 0.0, false, 512);
            let energy: f32 = frames.iter().map(|f| f.power[1] * f.dt).sum();
            let elapsed: f32 = frames.iter().map(|f| f.dt).sum();
            assert!((energy / elapsed - 0.08).abs() < 0.001);
        }
    }

    #[test]
    fn faders_change_power_by_gain_squared_and_only_their_channel_epoch() {
        let mut analyzer = PercussionAnalyzer::new(48000);
        let mut gains = PercussionBinGains::default();
        for (channel, frequency) in [117.1875, 3000.0, 12000.0].into_iter().enumerate() {
            let before = analyzer.revisions;
            gains.set_bin(
                channel,
                BinAddress::new(frequency, if channel == 0 { 23.4375 } else { 93.75 }),
                0.5,
            );
            analyzer.set_bin_gains(&gains);
            for (other, before) in before.into_iter().enumerate() {
                assert_eq!(
                    analyzer.revisions[other],
                    before + u64::from(channel == other)
                );
            }
        }
        let bin = analyzer
            .meters(1)
            .iter()
            .position(|m| m.frequency_hz == 3000.0)
            .unwrap();
        assert_eq!(analyzer.weights[1][bin], 0.25);
        let before = analyzer.revisions;
        gains.set_bin(0, BinAddress::new(3000.0, 23.4375), 0.0);
        analyzer.set_bin_gains(&gains);
        assert_eq!(
            analyzer.revisions, before,
            "inactive faders cannot cancel a pending candidate or erase its reference"
        );
    }

    #[test]
    fn sample_clock_includes_fill_and_nonfinite_windows_are_unavailable() {
        let mut analyzer = PercussionAnalyzer::new(48000);
        let mut first_available = [None; 3];
        for n in 0..3000 {
            if let Some(frame) = analyzer.push_stereo([0.1; 2]) {
                assert_eq!(frame.at, (n + 1) as f64 / 48000.0);
                for (channel, first) in first_available.iter_mut().enumerate() {
                    if frame.available[channel] && first.is_none() {
                        *first = Some(frame.at);
                    }
                }
            }
        }
        assert!(first_available[0].unwrap() >= 2048.0 / 48000.0);
        assert!(first_available[1].unwrap() >= 512.0 / 48000.0);
        analyzer.push_stereo([f32::NAN, 0.0]);
        let next = (0..48)
            .find_map(|_| analyzer.push_stereo([0.1; 2]))
            .unwrap();
        assert_eq!(next.available, [false; 3]);
    }

    #[test]
    fn kick_phase_experiment_compares_real_window_support() {
        let mut relative_spread = [0.0; 4];
        for (index, size) in [256, 512, 1024, 2048].into_iter().enumerate() {
            let mut levels = Vec::new();
            for phase in 0..32 {
                let mut transform = Transform::new(size);
                for n in 0..size {
                    let value = 0.4
                        * (std::f32::consts::TAU
                            * (60.0 * n as f32 / 48000.0 + phase as f32 / 32.0))
                            .sin();
                    transform.push([value; 2]);
                }
                transform.analyze();
                let bin_hz = 48000.0 / size as f32;
                levels.push(
                    transform
                        .power
                        .iter()
                        .enumerate()
                        .map(|(bin, power)| {
                            let frequency = bin as f32 * bin_hz;
                            power
                                * mask_weight(
                                    (frequency - bin_hz * 0.5).max(0.0),
                                    (frequency + bin_hz * 0.5).min(24000.0),
                                    60.0,
                                    200.0,
                                    200.0,
                                )
                        })
                        .sum::<f32>(),
                );
            }
            let lo = levels.iter().copied().fold(f32::INFINITY, f32::min);
            let hi = levels.iter().copied().fold(0.0, f32::max);
            relative_spread[index] = (hi - lo) / hi;
        }
        eprintln!("60 Hz regional power phase spread for N=256/512/1024/2048: {relative_spread:?}");
        assert!(relative_spread[0] > 0.1);
        assert!(relative_spread[1] > relative_spread[3] * 10.0);
        assert!(relative_spread[3] < 0.01);
    }

    #[test]
    #[ignore = "full window comparison; run explicitly in release mode"]
    fn kick_resolution_experiment_reports_leakage_and_feature_availability() {
        for size in [1024, 2048] {
            let mut leakage = Vec::new();
            for frequency in [
                20.0, 40.0, 50.0, 60.0, 80.0, 120.0, 180.0, 200.0, 220.0, 250.0, 300.0,
            ] {
                let mut power = 0.0;
                for phase in 0..8 {
                    let mut transform = Transform::new(size);
                    for n in 0..size {
                        let value = 0.4
                            * (std::f32::consts::TAU
                                * (frequency * n as f32 / 48000.0 + phase as f32 / 8.0))
                                .sin();
                        transform.push([value; 2]);
                    }
                    transform.analyze();
                    let hz = 48000.0 / size as f32;
                    power += transform
                        .power
                        .iter()
                        .enumerate()
                        .map(|(bin, power)| {
                            let f = bin as f32 * hz;
                            power
                                * mask_weight(
                                    (f - hz * 0.5).max(0.0),
                                    (f + hz * 0.5).min(24000.0),
                                    60.0,
                                    200.0,
                                    200.0,
                                )
                        })
                        .sum::<f32>()
                        / 8.0;
                }
                leakage.push((frequency, power / 0.08));
            }
            eprintln!("Kick N={size} selected/total stationary power by Hz: {leakage:?}");
            let mut feature_available = Vec::new();
            let mut concentration = Vec::new();
            for sample_rate in [44100, 48000, 96000] {
                for frequency in [60.0, 80.0, 120.0, 180.0] {
                    for phase in [0.0, 0.7, 1.7, 2.8] {
                        let mut analyzer = PercussionAnalyzer::new(sample_rate);
                        analyzer.kick = Transform::new(scaled_size(size, sample_rate));
                        analyzer.rebuild(0);
                        let mut previous = 0.0_f32;
                        let mut envelope = 0.0_f32;
                        let mut start = None;
                        let mut increments = Vec::new();
                        for n in 0..sample_rate / 5 {
                            let t = n as f64 / sample_rate as f64 - 0.1;
                            let value = if t >= 0.0 {
                                (0.4 * (t / 0.001).min(1.0)
                                    * (-t / 0.08).exp()
                                    * (std::f64::consts::TAU * frequency * t + phase).sin())
                                    as f32
                            } else {
                                0.0
                            };
                            if let Some(frame) = analyzer.push_stereo([value; 2]) {
                                envelope += (1.0 - (-frame.dt / 0.001).exp())
                                    * (frame.power[0].sqrt() - envelope);
                                let growth = (envelope - previous).max(0.0);
                                if start.is_none() && growth > 0.00001 && growth > previous * 0.002
                                {
                                    start = Some(frame.at);
                                }
                                if let Some(started) = start {
                                    increments.push(growth);
                                    if frame.at - started >= 0.024 {
                                        feature_available.push(frame.at - 0.1);
                                        let total = increments.iter().sum::<f32>();
                                        let peak = increments
                                            .windows(5)
                                            .map(|window| window.iter().sum::<f32>())
                                            .fold(0.0, f32::max);
                                        concentration.push(peak / total.max(1e-9));
                                        break;
                                    }
                                }
                                previous = envelope;
                            }
                        }
                    }
                }
            }
            let min_ms = feature_available
                .iter()
                .copied()
                .fold(f64::INFINITY, f64::min)
                * 1000.0;
            let max_ms = feature_available.iter().copied().fold(0.0, f64::max) * 1000.0;
            let min_s = concentration.iter().copied().fold(f32::INFINITY, f32::min);
            let max_s = concentration.iter().copied().fold(0.0, f32::max);
            eprintln!("Kick N={size} prospective24ms front-end availability {min_ms:.2}..{max_ms:.2}ms; observed concentration {min_s:.3}..{max_s:.3} (fixture only, classifier emission tested separately)");
            assert!(max_ms <= 50.0);
        }
    }

    #[test]
    fn broadband_impulse_integral_is_independent_of_window_and_sample_rate() {
        for sample_rate in [44100, 48000, 96000] {
            for fast_size in [256, 512] {
                for offset in [0, 7, 29] {
                    let mut analyzer = PercussionAnalyzer::new(sample_rate);
                    let settings = PercussionSettings {
                        fft_size: fast_size,
                        ..PercussionSettings::default()
                    };
                    analyzer.set_settings(&settings);
                    let mut measured = 0.0;
                    for n in 0..sample_rate / 5 {
                        let value = if n == sample_rate / 10 + offset {
                            0.8
                        } else {
                            0.0
                        };
                        if let Some(frame) = analyzer.push_stereo([value; 2]) {
                            measured += frame.rms * frame.rms * frame.dt;
                        }
                    }
                    let expected = 0.64 / sample_rate as f32;
                    assert!(
                        (measured / expected - 1.0).abs() < 0.001,
                        "sr={sample_rate} N={fast_size} offset={offset}: {measured} vs {expected}"
                    );
                }
            }
        }
    }

    #[test]
    fn kick_sweep_tracks_physical_region_without_high_frequency_leakage() {
        let sample_rate = 48000;
        let mut analyzer = PercussionAnalyzer::new(sample_rate);
        let mut phase = 0.0_f64;
        let mut inside = Vec::new();
        let mut outside = Vec::new();
        for n in 0..sample_rate {
            let frequency = 20.0 + 480.0 * n as f64 / sample_rate as f64;
            phase += std::f64::consts::TAU * frequency / sample_rate as f64;
            if let Some(frame) = analyzer.push_stereo([(0.4 * phase.sin()) as f32; 2]) {
                if !frame.available[0] {
                    continue;
                }
                // The front end remains causal; this center-frequency lookup
                // is only a fixture's frequency label, never an event time.
                let center_frequency =
                    20.0 + 480.0 * (frame.at - f64::from(frame.window_seconds[0]) * 0.5);
                if (90.0..=165.0).contains(&center_frequency) {
                    inside.push(frame.power[0]);
                }
                if center_frequency >= 300.0 {
                    outside.push(frame.power[0]);
                }
            }
        }
        assert!(inside.iter().all(|&power| power > 0.06));
        assert!(outside.iter().all(|&power| power < 0.0001));
    }

    #[test]
    fn feature_reset_preserves_clock_and_long_session_hop_precision() {
        let mut analyzer = PercussionAnalyzer::new(48000);
        analyzer.consumed_frames = 48000_u64 * 86400 * 30;
        let before = analyzer.audio_time();
        analyzer.reset_features();
        assert_eq!(analyzer.audio_time(), before);
        let frame = (0..48)
            .find_map(|_| analyzer.push_stereo([0.0; 2]))
            .unwrap();
        assert!((frame.at - before - 0.001).abs() < 1e-8);
        assert_eq!(frame.available, [false; 3]);
        analyzer.reset();
        assert_eq!(analyzer.audio_time(), 0.0);
    }

    #[test]
    #[ignore = "rejected 24 ms window comparison; run explicitly in release mode"]
    fn production_kick_window_experiment_compares_fast_and_smooth_attacks() {
        use super::super::transients::SharpOnsets;
        for size in [1024, 2048] {
            for rise_ms in [1.0, 30.0, 60.0, 120.0] {
                let mut concentrations = Vec::new();
                let mut delays = Vec::new();
                let mut eligible = 0;
                for sample_rate in [44100, 48000, 96000] {
                    for frequency in [60.0, 80.0, 120.0, 180.0] {
                        for phase in [0.0, 0.7, 1.7, 2.8] {
                            let mut analyzer = PercussionAnalyzer::new(sample_rate);
                            analyzer.kick = Transform::new(scaled_size(size, sample_rate));
                            analyzer.rebuild(0);
                            let mut detector = SharpOnsets::default();
                            let mut settings = PercussionSettings::default();
                            settings.channels[0].feature_ms = 24.0;
                            settings.channels[0].min_sharpness = 0.4;
                            for n in 0..sample_rate / 3 {
                                let t = n as f64 / sample_rate as f64 - 0.1;
                                let value = if t >= 0.0 {
                                    (0.4 * (t / (rise_ms * 0.001)).min(1.0)
                                        * (-t / 0.15).exp()
                                        * (std::f64::consts::TAU * frequency * t + phase).sin())
                                        as f32
                                } else {
                                    0.0
                                };
                                if let Some(frame) = analyzer.push_stereo([value; 2]) {
                                    detector.observe_percussion(&frame, frame.at, &settings);
                                }
                            }
                            if let Some(decision) =
                                detector.decisions().iter().find(|d| d.channel == 0)
                            {
                                concentrations.push(decision.sharpness);
                                delays.push((decision.emission_time - 0.1) * 1000.0);
                                eligible += usize::from(decision.eligible);
                            }
                        }
                    }
                }
                let min_s = concentrations.iter().copied().fold(f32::INFINITY, f32::min);
                let max_s = concentrations.iter().copied().fold(0.0, f32::max);
                let min_delay = delays.iter().copied().fold(f64::INFINITY, f64::min);
                let max_delay = delays.iter().copied().fold(0.0, f64::max);
                eprintln!("Production kick N={size} rise={rise_ms}ms: {}/48 candidates; observed S={min_s:.4}..{max_s:.4}; decision delay={min_delay:.2}..{max_delay:.2}ms; eligible={eligible}", concentrations.len());
                assert!(!concentrations.is_empty());
                assert!(max_delay <= 50.0);
            }
        }
    }

    /// Final selected front end: this compact regression runs in the normal
    /// suite; the exhaustive frequency/phase/gain/decay experiment is below.
    #[test]
    fn selected_kick_front_end_separates_canonical_fast_and_smooth_attacks() {
        use super::super::transients::SharpOnsets;
        let settings = PercussionSettings::default();
        for sample_rate in [44100, 48000, 96000] {
            for frequency in [60.0, 120.0, 180.0] {
                for rise_ms in [1.0, 30.0, 120.0] {
                    let mut analyzer = PercussionAnalyzer::new(sample_rate);
                    let mut detector = SharpOnsets::default();
                    for n in 0..sample_rate / 6 {
                        let t = n as f64 / sample_rate as f64 - 0.06;
                        let value = if t >= 0.0 {
                            (0.4 * (t / (rise_ms * 0.001)).min(1.0)
                                * (-t / 0.06).exp()
                                * (std::f64::consts::TAU * frequency * t + 0.7).sin())
                                as f32
                        } else {
                            0.0
                        };
                        if let Some(frame) = analyzer.push_stereo([value; 2]) {
                            detector.observe_percussion(&frame, frame.at, &settings);
                        }
                    }
                    let records: Vec<_> = detector
                        .decisions()
                        .iter()
                        .filter(|d| d.channel == 0)
                        .collect();
                    assert_eq!(
                        records.len(),
                        1,
                        "sr={sample_rate} f={frequency} rise={rise_ms}"
                    );
                    let decision = records[0];
                    assert_eq!(
                        decision.eligible,
                        rise_ms == 1.0,
                        "sr={sample_rate} f={frequency} rise={rise_ms} S={} reason={:?}",
                        decision.sharpness,
                        decision.reason
                    );
                    if decision.eligible {
                        assert!(decision.emission_time - 0.06 <= 0.050);
                    }
                    // Quiet smooth prefixes may cross the numerical discovery
                    // floor later (up to 52.65 ms signal-to-completion in the
                    // exhaustive matrix). Their fixed feature horizon remains
                    // bounded and they produce no percussion event.
                    assert!(decision.completion_time - decision.onset_time <= 0.0461);
                    assert!((decision.feature_seconds - 0.044).abs() < 1e-6);
                }
            }
        }
    }

    #[test]
    #[ignore = "exhaustive phase/gain/decay matrix; run explicitly in release mode"]
    fn production_kick_longer_horizon_experiment() {
        use super::super::transients::SharpOnsets;
        for decay in [0.06, 0.15] {
            for rise_ms in [1.0, 30.0, 60.0, 120.0] {
                let mut concentrations = Vec::new();
                let mut delays = Vec::new();
                for sample_rate in [44100, 48000, 96000] {
                    for amplitude in [0.2, 0.4, 0.8] {
                        for frequency in [60.0, 80.0, 120.0, 180.0] {
                            for phase in [0.0, 0.7, 1.7, 2.8] {
                                let mut analyzer = PercussionAnalyzer::new(sample_rate);
                                let mut detector = SharpOnsets::default();
                                let settings = PercussionSettings::default();
                                for n in 0..sample_rate / 4 {
                                    let t = n as f64 / sample_rate as f64 - 0.1;
                                    let value = if t >= 0.0 {
                                        (amplitude
                                            * (t / (rise_ms * 0.001)).min(1.0)
                                            * (-t / decay).exp()
                                            * (std::f64::consts::TAU * frequency * t + phase).sin())
                                            as f32
                                    } else {
                                        0.0
                                    };
                                    if let Some(frame) = analyzer.push_stereo([value; 2]) {
                                        detector.observe_percussion(&frame, frame.at, &settings);
                                    }
                                }
                                let decision = detector
                                    .decisions()
                                    .iter()
                                    .find(|d| d.channel == 0)
                                    .expect("audible attack yields a candidate");
                                concentrations.push(decision.sharpness);
                                delays.push((decision.emission_time - 0.1) * 1000.0);
                                assert_eq!(decision.eligible,rise_ms==1.0,"sr={sample_rate} f={frequency} phase={phase} amplitude={amplitude} decay={decay} rise={rise_ms} S={} reason={:?}",decision.sharpness,decision.reason);
                            }
                        }
                    }
                }
                let min_s = concentrations.iter().copied().fold(f32::INFINITY, f32::min);
                let max_s = concentrations.iter().copied().fold(0.0, f32::max);
                let max_delay = delays.iter().copied().fold(0.0, f64::max);
                eprintln!("Production kick N=2048 feature44ms decay={decay}s rise={rise_ms}ms: observed S={min_s:.4}..{max_s:.4}; max decision delay={max_delay:.2}ms");
                if rise_ms == 1.0 {
                    assert!(
                        max_delay <= 50.0,
                        "canonical audible percussion must meet the signal-to-emission target"
                    );
                }
            }
        }
    }
}
