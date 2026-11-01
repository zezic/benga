//! Causal arrangement tracking. Shape is centered log magnitude: a change of
//! playback gain translates the log spectrum and disappears on centering.
//! Sustained changes across adjacent spectral trends identify a quiet section;
//! returning bass measured by fast and slow LF envelopes confirms an audible drop.

use super::{
    detection::{DetectorSettings, DropStatus, OnsetFluxStatus},
    spectrum::Spectrum,
    BAND_COUNT,
};

const FLOOR_DB: f32 = -80.0;
const AMPLITUDE_FLOOR: f32 = 1e-4;
const BASS_WEIGHTS: [f32; BAND_COUNT] = [
    0.25, 0.5, 1.0, 1.0, 0.85, 0.4, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
];

fn db(amplitude: f32) -> f32 {
    (20.0 * amplitude.max(AMPLITUDE_FLOOR).log10()).max(FLOOR_DB)
}

fn follow(current: f32, target: f32, dt: f32, tau: f32) -> f32 {
    current + (target - current) * (1.0 - (-dt / tau).exp())
}

/// The displayed shape uses ±24 dB relative to the spectrum's geometric mean.
/// A relative floor keeps silence and tiny isolated FFT bins finite without
/// introducing a playback-level dependency.
fn shape(magnitudes: &[f32; BAND_COUNT]) -> [f32; BAND_COUNT] {
    let peak = magnitudes.iter().copied().fold(0.0, f32::max);
    if peak < 1e-7 {
        return [0.0; BAND_COUNT];
    }
    let floor = peak * 0.004;
    let logs = magnitudes.map(|value| 20.0 * (value.max(floor) / peak).log10());
    let center = logs.iter().sum::<f32>() / BAND_COUNT as f32;
    logs.map(|value| ((value - center) / 24.0).clamp(-1.0, 1.0))
}

/// Compare both relative band levels and the slope of every neighboring pair.
/// Broad movement matters more than an isolated narrow resonant bin.
fn difference(a: &[f32; BAND_COUNT], b: &[f32; BAND_COUNT]) -> (f32, f32, f32) {
    let mut distance = 0.0;
    let mut trend = 0.0;
    let mut changed = 0.0;
    let mut total = 0.0;
    for i in 0..BAND_COUNT {
        let weight = if i < 6 { 2.0 } else { 1.0 };
        distance += (a[i] - b[i]).abs() * weight;
        total += weight;
        if i + 1 < BAND_COUNT {
            let sa = a[i + 1] - a[i];
            let sb = b[i + 1] - b[i];
            trend += (sa - sb).abs().min(1.0);
            if (sa - sb).abs() > 0.10 {
                changed += 1.0;
            }
        }
    }
    (
        (distance / total).clamp(0.0, 1.0),
        (trend / (BAND_COUNT - 1) as f32).clamp(0.0, 1.0),
        changed / (BAND_COUNT - 1) as f32,
    )
}

#[derive(Debug)]
pub(crate) struct ArrangementTracker {
    pub(crate) status: DropStatus,
    pub(crate) eligible: bool,
    pub(crate) gap_entry: bool,
    pub(crate) flash: f32,
    pub(crate) last_power: f32,
    ready: bool,
    active_time: f32,
    envelope: f32,
    reference_db: f32,
    bass_fast: f32,
    bass_slow: f32,
    bass_rising: bool,
    bass_onset_after_gap: bool,
    reference_shape: [f32; BAND_COUNT],
    fast_shape: [f32; BAND_COUNT],
    quiet_time: f32,
    rms_quiet_time: f32,
    mix_loss_time: f32,
    last_drop: f64,
}

impl Default for ArrangementTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ArrangementTracker {
    pub(crate) fn new() -> Self {
        Self {
            status: DropStatus::default(),
            eligible: false,
            gap_entry: false,
            flash: 0.0,
            last_power: 0.0,
            ready: false,
            active_time: 0.0,
            envelope: 0.0,
            reference_db: FLOOR_DB,
            bass_fast: 0.0,
            bass_slow: 0.0,
            bass_rising: false,
            bass_onset_after_gap: false,
            reference_shape: [0.0; BAND_COUNT],
            fast_shape: [0.0; BAND_COUNT],
            quiet_time: 0.0,
            rms_quiet_time: 0.0,
            mix_loss_time: 0.0,
            last_drop: f64::NEG_INFINITY,
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    pub(crate) fn observe(
        &mut self,
        spectrum: &Spectrum,
        now: f64,
        dt: f32,
        settings: &DetectorSettings,
    ) -> Option<f32> {
        let tuning = settings.arrangement;
        self.gap_entry = false;
        self.flash *= (-dt * 8.0).exp();
        let rms = spectrum.rms.max(0.0);
        let rms_db = db(rms);
        let current_shape = shape(&spectrum.linear_bands);
        let spectrum_present = spectrum
            .linear_bands
            .iter()
            .any(|&magnitude| magnitude > 1e-7);
        let bass = spectrum
            .linear_bands
            .iter()
            .zip(BASS_WEIGHTS)
            .map(|(v, w)| v * v * w)
            .sum::<f32>()
            .sqrt();
        // Compare the same LF area on two time scales. Initializing both from
        // the first observation avoids inventing an onset on capture/reset.
        if self.ready {
            self.bass_fast = follow(self.bass_fast, bass, dt, tuning.bass_fast_time);
            self.bass_slow = follow(self.bass_slow, bass, dt, tuning.bass_slow_time);
        } else {
            self.bass_fast = bass;
            self.bass_slow = bass;
        }
        // Use the same measurement floor as the graph. Numerically tiny LF
        // leakage must not become a large normalized attack below -80 dB.
        let bass_fast = self.bass_fast.max(AMPLITUDE_FLOOR);
        let bass_slow = self.bass_slow.max(AMPLITUDE_FLOOR);
        let raw_hit = ((bass_fast - bass_slow).max(0.0) / bass_fast).clamp(0.0, 1.0);
        let hit_threshold = tuning.bass_hit_threshold;
        let bass_fast_db = db(bass_fast);
        let bass_slow_db = db(bass_slow);
        // (fast - slow) / fast >= k iff fast >= slow / (1 - k).
        let bass_onset_threshold_db = db(bass_slow / (1.0 - hit_threshold));
        let bass_rising = raw_hit >= hit_threshold;
        if !bass_rising {
            self.bass_onset_after_gap = false;
        } else if !self.bass_rising {
            self.bass_onset_after_gap = self.eligible;
        }
        self.bass_rising = bass_rising;

        if !self.ready {
            self.envelope = rms;
            self.reference_db = rms_db;
            self.fast_shape = current_shape;
            self.reference_shape = current_shape;
            self.ready = true;
        }
        self.envelope = follow(
            self.envelope,
            rms,
            dt,
            if rms > self.envelope { 0.06 } else { 0.24 },
        );
        let envelope_db = db(self.envelope);
        // Follow measurable shape at any playback level. Only numerical silence
        // lacks a usable shape; output energy validation belongs to the drop.
        if spectrum_present {
            for (value, target) in self.fast_shape.iter_mut().zip(current_shape) {
                *value = follow(*value, target, dt, 0.20);
            }
        }
        let (distance, trend, fraction) = difference(&self.fast_shape, &self.reference_shape);
        let energy_delta = envelope_db - self.reference_db;
        let has_history = self.active_time >= 1.5;
        let changed_shape = distance > tuning.shape_distance
            && (trend > tuning.trend_change || fraction >= tuning.changed_fraction);
        let energy_loss = energy_delta < -tuning.mix_loss_db;
        // The original shape path combines spectral movement with relative
        // mix loss. The two level-only paths below each have their own hold.
        let quiet = has_history
            && energy_loss
            && (changed_shape || (!spectrum_present && energy_delta < -20.0));
        // Absolute RMS has its own hold and needs no spectral evidence or
        // reference warmup. Use the measured window level, not the slower mix
        // envelope, so the configured duration is the entire requested delay.
        let rms_quiet = rms_db < tuning.gap_rms_db;
        if !self.eligible {
            self.quiet_time = if quiet {
                (self.quiet_time + dt).min(tuning.gap_hold)
            } else {
                0.0
            };
            self.rms_quiet_time = if rms_quiet {
                (self.rms_quiet_time + dt).min(tuning.gap_rms_hold)
            } else {
                0.0
            };
            self.mix_loss_time = if energy_loss {
                (self.mix_loss_time + dt).min(tuning.mix_loss_hold)
            } else {
                0.0
            };
            let relative_ready = self.quiet_time >= tuning.gap_hold
                && now - self.last_drop >= f64::from(tuning.drop_refractory);
            let rms_ready = self.rms_quiet_time >= tuning.gap_rms_hold;
            let loss_ready = self.mix_loss_time >= tuning.mix_loss_hold;
            if relative_ready || rms_ready || loss_ready {
                self.eligible = true;
                self.gap_entry = true;
                // A bass rise already underway when a gap becomes confirmed
                // cannot also be that section's subsequent bass return.
                self.bass_onset_after_gap = false;
            }
        }

        let mut onset = None;
        let drop_rms_threshold_db = tuning.drop_min_rms_db;
        if self.eligible {
            // Confirmation latches the gap until a valid drop consumes it,
            // even when the original quiet conditions no longer hold.
            let audible = rms_db >= drop_rms_threshold_db;
            let hit = bass_rising && self.bass_onset_after_gap;
            if audible && hit && now - self.last_drop >= f64::from(tuning.drop_refractory) {
                let power = (raw_hit / hit_threshold).clamp(0.5, 2.0);
                self.eligible = false;
                self.quiet_time = 0.0;
                self.rms_quiet_time = 0.0;
                self.mix_loss_time = 0.0;
                self.last_drop = now;
                self.last_power = power;
                self.flash = 1.0;
                self.bass_onset_after_gap = false;
                self.reference_db = rms_db;
                self.reference_shape = current_shape;
                onset = Some(power);
            }
        }

        // Candidate valleys still learn slowly so a repeating sparse rhythm
        // cannot pin the reference to an old loud passage forever. Confirmed
        // quiet sections freeze it until a drop consumes their armed state.
        if !self.eligible && spectrum_present {
            if !has_history {
                self.reference_db = envelope_db;
                self.reference_shape = self.fast_shape;
            } else {
                let learning =
                    tuning.reference_time * if self.quiet_time > 0.0 { 3.2 } else { 1.0 };
                self.reference_db = follow(self.reference_db, envelope_db, dt, learning);
                for (value, target) in self.reference_shape.iter_mut().zip(self.fast_shape) {
                    *value = follow(*value, target, dt, learning);
                }
            }
            self.active_time = (self.active_time + dt).min(2.0);
        }
        self.status = DropStatus {
            rms_db,
            envelope_db,
            shape: self.fast_shape,
            reference_shape: self.reference_shape,
            shape_distance: distance,
            trend_change: trend,
            changed_fraction: fraction,
            shape_distance_threshold: tuning.shape_distance,
            trend_change_threshold: tuning.trend_change,
            changed_fraction_threshold: tuning.changed_fraction,
            energy_delta_db: energy_delta,
            mix_loss_threshold_db: tuning.mix_loss_db,
            mix_loss_hold_seconds: tuning.mix_loss_hold,
            mix_loss_held_seconds: self.mix_loss_time,
            mix_loss_condition_met: energy_loss,
            gap_conditions_met: quiet,
            gap_hold_seconds: tuning.gap_hold,
            gap_held_seconds: self.quiet_time,
            gap_history_ready: has_history,
            gap_rms_threshold_db: tuning.gap_rms_db,
            gap_rms_hold_seconds: tuning.gap_rms_hold,
            gap_rms_held_seconds: self.rms_quiet_time,
            gap_rms_condition_met: rms_quiet,
            gap_cooldown_remaining: (f64::from(tuning.drop_refractory) - (now - self.last_drop))
                .max(0.0) as f32,
            drop_rms_threshold_db,
            bass_fast_db,
            bass_slow_db,
            bass_fast_seconds: tuning.bass_fast_time,
            bass_slow_seconds: tuning.bass_slow_time,
            bass_onset_threshold_db,
            bass_onset_ready: self.bass_onset_after_gap,
            bass_hit: raw_hit,
            hit_threshold,
            quiet_progress: if self.eligible {
                1.0
            } else {
                (self.quiet_time / tuning.gap_hold).clamp(0.0, 1.0)
            },
            onset: OnsetFluxStatus {
                flux: raw_hit,
                smoothed: raw_hit,
                reference: None,
                threshold: hit_threshold,
                ..OnsetFluxStatus::default()
            },
        };
        onset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pin the acoustic regression recipe independently of factory tuning.
    fn arrangement_test_settings() -> DetectorSettings {
        DetectorSettings {
            arrangement: super::super::ArrangementSettings {
                mix_loss_db: 2.0,
                mix_loss_hold: 1.2,
                shape_distance: 0.12,
                trend_change: 0.055,
                changed_fraction: 0.25,
                gap_hold: 1.2,
                gap_rms_db: -40.0,
                gap_rms_hold: 0.5,
                reference_time: 5.0,
                bass_fast_time: 0.020,
                bass_slow_time: 0.250,
                bass_hit_threshold: 0.12,
                drop_min_rms_db: -26.0,
                drop_refractory: 8.0,
            },
            ..DetectorSettings::default()
        }
    }

    fn section(rms: f32, bass: f32, mid: f32, high: f32) -> Spectrum {
        Spectrum {
            rms,
            linear_bands: std::array::from_fn(|i| {
                if i < 6 {
                    bass
                } else if i < 11 {
                    mid
                } else {
                    high
                }
            }),
            ..Spectrum::default()
        }
    }

    fn feed(
        tracker: &mut ArrangementTracker,
        spectrum: Spectrum,
        seconds: f64,
        now: &mut f64,
    ) -> usize {
        feed_settings(
            tracker,
            spectrum,
            seconds,
            now,
            &arrangement_test_settings(),
        )
    }

    fn feed_settings(
        tracker: &mut ArrangementTracker,
        spectrum: Spectrum,
        seconds: f64,
        now: &mut f64,
        settings: &DetectorSettings,
    ) -> usize {
        let mut hits = 0;
        for _ in 0..(seconds / 0.005).round() as usize {
            *now += 0.005;
            hits += usize::from(tracker.observe(&spectrum, *now, 0.005, settings).is_some());
        }
        hits
    }

    fn loss_only_settings() -> DetectorSettings {
        let mut settings = arrangement_test_settings();
        settings.arrangement.gap_rms_db = -80.0;
        settings.arrangement.gap_hold = 4.0;
        settings.arrangement.shape_distance = 0.6;
        settings.arrangement.mix_loss_hold = 0.1;
        settings.arrangement.reference_time = 20.0;
        settings
    }

    #[test]
    fn loss_gap_arms_on_uniform_attenuation_without_shape_or_absolute_rms() {
        let settings = loss_only_settings();
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = section(0.25, 0.15, 0.05, 0.025);
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
        let lower = Spectrum {
            rms: active.rms * 0.4,
            linear_bands: active.linear_bands.map(|value| value * 0.4),
            ..Spectrum::default()
        };
        for _ in 0..100 {
            feed_settings(&mut tracker, lower, 0.005, &mut now, &settings);
            assert!(!tracker.status.gap_conditions_met);
            assert!(!tracker.status.gap_rms_condition_met);
            assert_eq!(tracker.status.gap_held_seconds, 0.0);
            if tracker.status.mix_loss_held_seconds < settings.arrangement.mix_loss_hold {
                assert!(
                    !tracker.eligible,
                    "only a completed loss hold can arm this fixture"
                );
            }
            if tracker.eligible {
                break;
            }
        }
        assert!(tracker.status.mix_loss_condition_met);
        assert!(
            tracker.eligible,
            "sustained energy loss alone must arm the gap"
        );
        assert_eq!(tracker.status.mix_loss_hold_seconds, 0.1);
        assert_eq!(tracker.status.mix_loss_held_seconds, 0.1);
        assert_eq!(tracker.status.quiet_progress, 1.0);
    }

    #[test]
    fn loss_gap_requires_continuous_loss_and_equality_resets_its_own_hold() {
        let settings = loss_only_settings();
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = section(0.25, 0.15, 0.05, 0.025);
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
        let below = Spectrum {
            rms: 0.075,
            ..active
        };
        // Pin the envelope/reference at exact test levels to exercise the
        // comparison boundary without a smoothing transition obscuring it.
        tracker.envelope = below.rms;
        tracker.reference_db = -18.0;
        feed_settings(&mut tracker, below, 0.07, &mut now, &settings);
        assert!(tracker.status.mix_loss_held_seconds > 0.06);
        assert!(!tracker.eligible);
        let boundary = Spectrum { rms: 0.1, ..active };
        tracker.envelope = boundary.rms;
        tracker.reference_db = db(boundary.rms) + settings.arrangement.mix_loss_db;
        feed_settings(&mut tracker, boundary, 0.005, &mut now, &settings);
        assert_eq!(
            tracker.status.energy_delta_db,
            -settings.arrangement.mix_loss_db
        );
        assert!(!tracker.status.mix_loss_condition_met);
        assert_eq!(tracker.status.mix_loss_held_seconds, 0.0);
        tracker.envelope = below.rms;
        tracker.reference_db = -18.0;
        feed_settings(&mut tracker, below, 0.07, &mut now, &settings);
        assert!(
            !tracker.eligible,
            "separate loss episodes cannot pool their durations"
        );
        feed_settings(&mut tracker, below, 0.04, &mut now, &settings);
        assert!(tracker.eligible);
    }

    #[test]
    fn loss_gap_rms_gap_and_shape_gap_keep_three_independent_timers() {
        let bell = std::array::from_fn(|i| 0.01 + (-(i as f32 - 7.5).powi(2) / 12.0).exp());
        let active = Spectrum {
            rms: 0.02,
            linear_bands: bell.map(|value| value * 0.08),
            ..Spectrum::default()
        };
        let mut settings = loss_only_settings();
        settings.arrangement.mix_loss_db = 0.25;
        settings.arrangement.mix_loss_hold = 0.2;
        settings.arrangement.gap_rms_hold = 0.1;
        settings.arrangement.gap_hold = 0.1;
        settings.arrangement.shape_distance = 0.01;
        settings.arrangement.trend_change = 0.005;
        settings.arrangement.changed_fraction = 0.05;
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
        settings.arrangement.gap_rms_db = -30.0;
        feed_settings(&mut tracker, active, 0.06, &mut now, &settings);
        assert!(tracker.status.gap_rms_held_seconds > 0.05);
        assert_eq!(tracker.status.mix_loss_held_seconds, 0.0);
        assert_eq!(tracker.status.gap_held_seconds, 0.0);
        // Above the absolute RMS limit, but below a remembered louder mix.
        let loss_only = Spectrum {
            rms: 0.04,
            ..active
        };
        tracker.envelope = loss_only.rms;
        tracker.reference_db = db(0.25);
        feed_settings(&mut tracker, loss_only, 0.06, &mut now, &settings);
        assert_eq!(tracker.status.gap_rms_held_seconds, 0.0);
        assert!(tracker.status.mix_loss_held_seconds > 0.05);
        assert_eq!(tracker.status.gap_held_seconds, 0.0);
        assert!(
            !tracker.eligible,
            "partial RMS and loss holds cannot combine"
        );
        let loss_with_shape = Spectrum {
            linear_bands: bell.map(|value| (1.1 - value) * 0.05),
            ..loss_only
        };
        feed_settings(&mut tracker, loss_with_shape, 0.06, &mut now, &settings);
        assert!(tracker.status.gap_conditions_met);
        assert!(tracker.status.gap_held_seconds > 0.0);
        assert!(tracker.status.gap_held_seconds < settings.arrangement.gap_hold);
        assert!(tracker.status.mix_loss_held_seconds < settings.arrangement.mix_loss_hold);
        assert!(
            !tracker.eligible,
            "partial loss and shape holds cannot combine"
        );
    }

    #[test]
    fn loss_gap_live_edits_preserve_the_latch_and_a_drop_resets_all_timers() {
        let mut settings = loss_only_settings();
        settings.arrangement.mix_loss_hold = 0.5;
        settings.arrangement.gap_rms_db = -30.0;
        settings.arrangement.gap_rms_hold = 1.0;
        settings.arrangement.shape_distance = 0.12;
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = section(0.25, 0.15, 0.05, 0.025);
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
        let quiet = section(0.02, 0.001, 0.01, 0.006);
        feed_settings(&mut tracker, quiet, 0.35, &mut now, &settings);
        let saved = tracker.status;
        assert!(!tracker.eligible);
        assert!(saved.mix_loss_held_seconds > 0.1);
        assert!(saved.gap_rms_held_seconds > 0.3);
        assert!(saved.gap_held_seconds > 0.0);
        settings.arrangement.mix_loss_hold = 0.1;
        feed_settings(&mut tracker, quiet, 0.005, &mut now, &settings);
        assert!(
            tracker.eligible,
            "lowering the hold must use accumulated loss evidence"
        );
        settings.arrangement.drop_min_rms_db = 0.0;
        settings.arrangement.mix_loss_db = 12.0;
        settings.arrangement.mix_loss_hold = 10.0;
        settings.arrangement.gap_rms_db = -80.0;
        assert_eq!(
            feed_settings(&mut tracker, active, 2.0, &mut now, &settings),
            0
        );
        assert!(!tracker.status.mix_loss_condition_met);
        assert!(
            tracker.eligible,
            "lost conditions and live edits cannot clear a latched gap"
        );
        assert_eq!(tracker.status.quiet_progress, 1.0);
        assert_eq!(saved.mix_loss_hold_seconds, 0.5);
        assert_eq!(saved.mix_loss_threshold_db, 2.0);
        settings.arrangement.drop_min_rms_db = -26.0;
        assert_eq!(
            feed_settings(&mut tracker, active, 0.5, &mut now, &settings),
            0
        );
        feed_settings(
            &mut tracker,
            section(0.25, 0.05, 0.05, 0.025),
            0.5,
            &mut now,
            &settings,
        );
        assert_eq!(
            feed_settings(&mut tracker, active, 0.02, &mut now, &settings),
            1
        );
        assert!(!tracker.eligible);
        assert_eq!(tracker.status.mix_loss_held_seconds, 0.0);
        assert_eq!(tracker.status.gap_rms_held_seconds, 0.0);
        assert_eq!(tracker.status.gap_held_seconds, 0.0);
    }

    #[test]
    fn loss_gap_arms_during_refractory_but_drops_still_obey_the_interval() {
        let mut settings = loss_only_settings();
        settings.arrangement.mix_loss_hold = 0.05;
        settings.arrangement.drop_refractory = 8.0;
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = section(0.25, 0.15, 0.05, 0.025);
        let quiet = Spectrum {
            rms: active.rms * 0.2,
            linear_bands: active.linear_bands.map(|value| value * 0.2),
            ..Spectrum::default()
        };
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
        feed_settings(&mut tracker, quiet, 0.5, &mut now, &settings);
        assert!(tracker.eligible);
        assert_eq!(
            feed_settings(&mut tracker, active, 0.05, &mut now, &settings),
            1
        );
        feed_settings(&mut tracker, quiet, 0.5, &mut now, &settings);
        assert!(
            tracker.eligible,
            "completed loss hold alone must rearm during the drop interval"
        );
        assert!(tracker.status.gap_cooldown_remaining > 7.0);
        assert_eq!(
            feed_settings(&mut tracker, active, 0.05, &mut now, &settings),
            0
        );
        assert!(tracker.eligible);
        assert_eq!(
            feed_settings(&mut tracker, active, 8.0, &mut now, &settings),
            0
        );
        assert!(
            tracker.eligible,
            "waiting for the interval cannot expire the gap"
        );
        feed_settings(&mut tracker, quiet, 0.5, &mut now, &settings);
        assert_eq!(
            feed_settings(&mut tracker, active, 0.02, &mut now, &settings),
            1
        );
        assert!(!tracker.eligible);
    }

    #[test]
    fn loss_gap_needs_no_extra_warmup_when_the_required_loss_is_registered() {
        let settings = loss_only_settings();
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        feed_settings(
            &mut tracker,
            section(0.25, 0.15, 0.05, 0.025),
            0.2,
            &mut now,
            &settings,
        );
        feed_settings(&mut tracker, Spectrum::default(), 0.4, &mut now, &settings);
        assert!(tracker.status.mix_loss_condition_met);
        assert!(!tracker.status.gap_history_ready);
        assert!(!tracker.status.gap_conditions_met);
        assert!(!tracker.status.gap_rms_condition_met);
        assert!(
            tracker.eligible,
            "a registered loss needs only its own completed hold"
        );
    }

    #[test]
    fn rms_gap_can_arm_at_startup_without_relative_evidence_or_history() {
        let mut settings = arrangement_test_settings();
        settings.arrangement.gap_rms_db = -30.0;
        settings.arrangement.gap_rms_hold = 0.1;
        settings.arrangement.gap_hold = 4.0;
        settings.arrangement.mix_loss_db = 12.0;
        settings.arrangement.shape_distance = 0.6;
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let low = section(0.01, 0.002, 0.003, 0.001);
        feed_settings(&mut tracker, low, 0.09, &mut now, &settings);
        assert!(!tracker.eligible);
        assert!(tracker.status.gap_rms_condition_met);
        assert!(!tracker.status.gap_history_ready);
        assert!(!tracker.status.gap_conditions_met);
        assert_eq!(tracker.status.gap_held_seconds, 0.0);
        feed_settings(&mut tracker, low, 0.02, &mut now, &settings);
        assert!(tracker.eligible, "absolute RMS alone must confirm the gap");
        assert!(!tracker.status.gap_history_ready);
        assert_eq!(tracker.status.quiet_progress, 1.0);
        assert_eq!(tracker.status.gap_rms_threshold_db, -30.0);
        assert_eq!(tracker.status.gap_rms_hold_seconds, 0.1);
    }

    #[test]
    fn rms_gap_uses_raw_window_rms_and_accepts_uniform_attenuation() {
        let mut settings = arrangement_test_settings();
        settings.arrangement.gap_rms_db = -30.0;
        settings.arrangement.gap_rms_hold = 0.05;
        settings.arrangement.shape_distance = 0.6;
        settings.arrangement.gap_hold = 4.0;
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = section(0.25, 0.15, 0.05, 0.025);
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
        let low = Spectrum {
            rms: active.rms * 0.02,
            linear_bands: active.linear_bands.map(|v| v * 0.02),
            ..Spectrum::default()
        };
        feed_settings(&mut tracker, low, 0.06, &mut now, &settings);
        assert!(tracker.status.rms_db < settings.arrangement.gap_rms_db);
        assert!(
            tracker.status.envelope_db > settings.arrangement.gap_rms_db,
            "fixture must still be above the threshold after smoothing"
        );
        assert!(!tracker.status.gap_conditions_met);
        assert!(
            tracker.eligible,
            "no shape change or envelope lag may block absolute RMS"
        );
    }

    #[test]
    fn rms_gap_requires_one_continuous_hold_and_equality_resets_it() {
        let mut settings = arrangement_test_settings();
        settings.arrangement.gap_rms_db = -40.0;
        settings.arrangement.gap_rms_hold = 0.1;
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let low = section(0.005, 0.001, 0.002, 0.001);
        feed_settings(&mut tracker, low, 0.07, &mut now, &settings);
        assert!(!tracker.eligible);
        assert!(tracker.status.gap_rms_held_seconds > 0.06);
        let boundary = Spectrum { rms: 0.01, ..low };
        assert_eq!(db(boundary.rms), settings.arrangement.gap_rms_db);
        feed_settings(&mut tracker, boundary, 0.005, &mut now, &settings);
        assert!(!tracker.status.gap_rms_condition_met);
        assert_eq!(tracker.status.gap_rms_held_seconds, 0.0);
        feed_settings(&mut tracker, low, 0.07, &mut now, &settings);
        assert!(
            !tracker.eligible,
            "separate dips cannot accumulate one hold"
        );
        feed_settings(&mut tracker, low, 0.04, &mut now, &settings);
        assert!(tracker.eligible);
    }

    #[test]
    fn rms_gap_and_relative_gap_keep_independent_hold_timers() {
        let bell = std::array::from_fn(|i| 0.01 + (-(i as f32 - 7.5).powi(2) / 12.0).exp());
        let active = Spectrum {
            rms: 0.25,
            linear_bands: bell.map(|v| v * 0.08),
            ..Spectrum::default()
        };
        let mut settings = arrangement_test_settings();
        settings.arrangement.gap_rms_db = -30.0;
        settings.arrangement.gap_rms_hold = 0.1;
        settings.arrangement.gap_hold = 0.1;
        settings.arrangement.mix_loss_db = 0.25;
        settings.arrangement.shape_distance = 0.01;
        settings.arrangement.trend_change = 0.005;
        settings.arrangement.changed_fraction = 0.05;
        settings.arrangement.reference_time = 20.0;
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
        let absolute_only = Spectrum {
            rms: 0.02,
            linear_bands: active.linear_bands.map(|v| v * 0.08),
            ..Spectrum::default()
        };
        feed_settings(&mut tracker, absolute_only, 0.06, &mut now, &settings);
        assert!(tracker.status.gap_rms_held_seconds > 0.05);
        assert_eq!(tracker.status.gap_held_seconds, 0.0);
        let relative_only = Spectrum {
            rms: 0.035,
            linear_bands: bell.map(|v| (1.1 - v) * 0.05),
            ..Spectrum::default()
        };
        feed_settings(&mut tracker, relative_only, 0.06, &mut now, &settings);
        assert!(tracker.status.gap_conditions_met);
        assert!(tracker.status.gap_held_seconds > 0.0);
        assert!(tracker.status.gap_held_seconds < settings.arrangement.gap_hold);
        assert_eq!(tracker.status.gap_rms_held_seconds, 0.0);
        assert!(
            !tracker.eligible,
            "neither independent path completed its hold"
        );
    }

    #[test]
    fn rms_gap_live_controls_keep_the_latch_and_a_drop_resets_both_timers() {
        let mut settings = arrangement_test_settings();
        settings.arrangement.gap_rms_db = -30.0;
        settings.arrangement.gap_rms_hold = 0.5;
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let low = section(0.01, 0.002, 0.003, 0.001);
        feed_settings(&mut tracker, low, 0.15, &mut now, &settings);
        let saved = tracker.status;
        assert!(!tracker.eligible);
        settings.arrangement.gap_rms_hold = 0.1;
        feed_settings(&mut tracker, low, 0.005, &mut now, &settings);
        assert!(
            tracker.eligible,
            "lowering the hold applies to accumulated evidence"
        );
        settings.arrangement.gap_rms_db = -80.0;
        settings.arrangement.gap_rms_hold = 10.0;
        feed_settings(&mut tracker, low, 0.2, &mut now, &settings);
        assert!(
            tracker.eligible,
            "live changes cannot clear a confirmed gap"
        );
        assert_eq!(tracker.status.quiet_progress, 1.0);
        assert_eq!(saved.gap_rms_threshold_db, -30.0);
        assert_eq!(saved.gap_rms_hold_seconds, 0.5);
        assert_eq!(tracker.status.gap_rms_threshold_db, -80.0);
        let active = section(0.25, 0.15, 0.05, 0.025);
        assert_eq!(
            feed_settings(&mut tracker, active, 0.05, &mut now, &settings),
            1
        );
        assert!(!tracker.eligible);
        assert_eq!(tracker.status.gap_rms_held_seconds, 0.0);
        assert_eq!(tracker.status.gap_held_seconds, 0.0);
    }

    #[test]
    fn rms_gap_can_arm_during_drop_refractory_but_cannot_bypass_drop_interval() {
        let mut settings = arrangement_test_settings();
        settings.arrangement.gap_rms_db = -30.0;
        settings.arrangement.gap_rms_hold = 0.05;
        settings.arrangement.drop_refractory = 8.0;
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let low = section(0.01, 0.002, 0.003, 0.001);
        let active = section(0.25, 0.15, 0.05, 0.025);
        feed_settings(&mut tracker, low, 0.06, &mut now, &settings);
        assert!(tracker.eligible);
        assert_eq!(
            feed_settings(&mut tracker, active, 0.05, &mut now, &settings),
            1
        );
        feed_settings(&mut tracker, low, 0.5, &mut now, &settings);
        assert!(
            tracker.eligible,
            "absolute RMS alone must be enough to rearm"
        );
        assert!(tracker.status.gap_cooldown_remaining > 7.0);
        assert_eq!(
            feed_settings(&mut tracker, active, 0.05, &mut now, &settings),
            0
        );
        assert!(tracker.eligible, "only a valid drop consumes the flag");
    }

    #[test]
    fn envelope_time_controls_change_lf_attack_latency_without_clearing_the_gap() {
        let mut first_crossings = Vec::new();
        for fast in [0.005, 0.100] {
            let mut settings = arrangement_test_settings();
            settings.arrangement.gap_rms_db = -80.0;
            settings.arrangement.bass_hit_threshold = 0.5;
            let mut tracker = ArrangementTracker::new();
            let mut now = 0.0;
            let active = section(0.25, 0.15, 0.05, 0.025);
            feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
            feed_settings(
                &mut tracker,
                section(0.035, 0.002, 0.02, 0.012),
                3.0,
                &mut now,
                &settings,
            );
            assert!(tracker.eligible);
            settings.arrangement.drop_min_rms_db = 0.0;
            settings.arrangement.bass_fast_time = fast;
            let mut crossing = None;
            for hop in 1..=20 {
                assert_eq!(
                    feed_settings(&mut tracker, active, 0.005, &mut now, &settings),
                    0
                );
                if tracker.status.bass_hit >= tracker.status.hit_threshold {
                    crossing.get_or_insert(hop);
                }
            }
            assert!(
                tracker.eligible,
                "envelope edits cannot clear a confirmed gap"
            );
            assert_eq!(tracker.status.quiet_progress, 1.0);
            assert_eq!(tracker.status.bass_fast_seconds, fast);
            first_crossings.push(crossing.expect("both attack speeds must reach this threshold"));
        }
        assert!(
            first_crossings[0] < first_crossings[1],
            "faster following must reach the onset threshold sooner: {first_crossings:?}"
        );
    }

    #[test]
    fn envelope_time_controls_change_lf_evidence_duration_without_clearing_the_gap() {
        let mut scores = Vec::new();
        for slow in [0.05, 0.5] {
            let mut settings = arrangement_test_settings();
            settings.arrangement.gap_rms_db = -80.0;
            let mut tracker = ArrangementTracker::new();
            let mut now = 0.0;
            let active = section(0.25, 0.15, 0.05, 0.025);
            feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
            feed_settings(
                &mut tracker,
                section(0.035, 0.002, 0.02, 0.012),
                3.0,
                &mut now,
                &settings,
            );
            assert!(tracker.eligible);
            settings.arrangement.drop_min_rms_db = 0.0;
            settings.arrangement.bass_slow_time = slow;
            assert_eq!(
                feed_settings(&mut tracker, active, 0.3, &mut now, &settings),
                0
            );
            assert!(tracker.eligible);
            assert_eq!(tracker.status.bass_slow_seconds, slow);
            scores.push(tracker.status.bass_hit);
        }
        assert!(
            scores[0] < 0.05 && scores[1] > 0.2,
            "a slower background must retain the LF rise longer: {scores:?}"
        );
    }

    #[test]
    fn actual_stream_high_only_return_after_silence_does_not_confirm_a_drop() {
        use super::super::spectrum::SpectrumAnalyzer;
        let rate = 48_000;
        let mut analyzer = SpectrumAnalyzer::new(rate);
        let mut tracker = ArrangementTracker::new();
        let mut armed = false;
        let mut drops = 0;
        for sample in 0..rate * 8 {
            let time = sample as f64 / rate as f64;
            let signal = if time < 4.0 {
                0.2 * (std::f64::consts::TAU * 90.0 * time).sin()
                    + 0.05 * (std::f64::consts::TAU * 880.0 * time).sin()
            } else if time < 7.0 {
                0.0
            } else {
                0.2 * ((time - 7.0) / 0.02).min(1.0) * (std::f64::consts::TAU * 6000.0 * time).sin()
            } as f32;
            if let Some(spectrum) = analyzer.push_stereo([signal, -signal]) {
                drops += usize::from(
                    tracker
                        .observe(
                            &spectrum,
                            time,
                            analyzer.hop_seconds(),
                            &arrangement_test_settings(),
                        )
                        .is_some(),
                );
                if (6.0..7.0).contains(&time) {
                    armed |= tracker.eligible;
                }
            }
        }
        assert!(
            armed,
            "real silence must establish a gap before the high-only return"
        );
        assert_eq!(drops, 0, "a loud HF return contains no LF onset");
        assert!(tracker.eligible);
    }

    #[test]
    fn lf_envelopes_ignore_numerical_bass_when_high_frequencies_return_after_silence() {
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        feed(
            &mut tracker,
            section(0.25, 0.15, 0.05, 0.025),
            4.0,
            &mut now,
        );
        feed(&mut tracker, Spectrum::default(), 4.0, &mut now);
        assert!(tracker.eligible);
        let high_return = Spectrum {
            rms: 0.2,
            linear_bands: std::array::from_fn(|band| if band < 7 { 1e-7 } else { 0.15 }),
            ..Spectrum::default()
        };
        assert_eq!(feed(&mut tracker, high_return, 0.2, &mut now), 0);
        assert!(tracker.eligible, "numerical LF cannot consume the gap");
        assert!(tracker.status.bass_hit < tracker.status.hit_threshold);
    }

    #[test]
    fn lf_envelopes_require_a_fresh_rise_after_the_gap_confirms() {
        let mut settings = arrangement_test_settings();
        settings.arrangement.shape_distance = 0.01;
        settings.arrangement.trend_change = 0.005;
        settings.arrangement.changed_fraction = 0.05;
        settings.arrangement.gap_hold = 0.05;
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = Spectrum {
            rms: 0.25,
            linear_bands: std::array::from_fn(|band| if band < 7 { 0.01 } else { 0.08 }),
            ..Spectrum::default()
        };
        let gap_with_existing_rise = Spectrum {
            rms: 0.1,
            linear_bands: std::array::from_fn(|band| if band < 7 { 0.02 } else { 0.005 }),
            ..Spectrum::default()
        };
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
        assert_eq!(
            feed_settings(
                &mut tracker,
                gap_with_existing_rise,
                0.2,
                &mut now,
                &settings
            ),
            0,
            "the rise that preceded gap entry must not immediately consume it"
        );
        assert!(tracker.eligible);
        assert_eq!(
            feed_settings(
                &mut tracker,
                gap_with_existing_rise,
                1.0,
                &mut now,
                &settings
            ),
            0
        );
        let fresh_return = Spectrum {
            rms: 0.25,
            linear_bands: std::array::from_fn(|band| if band < 7 { 0.08 } else { 0.08 }),
            ..Spectrum::default()
        };
        assert_eq!(
            feed_settings(&mut tracker, fresh_return, 0.1, &mut now, &settings),
            1
        );
    }

    #[test]
    fn lf_envelopes_confirm_a_bass_return_from_lf_levels_alone() {
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = section(0.25, 0.15, 0.05, 0.025);
        feed(&mut tracker, active, 4.0, &mut now);
        feed(
            &mut tracker,
            section(0.035, 0.002, 0.02, 0.012),
            3.0,
            &mut now,
        );
        assert!(tracker.eligible);
        assert_eq!(feed(&mut tracker, active, 0.1, &mut now), 1);
        assert_eq!(feed(&mut tracker, active, 1.0, &mut now), 0);
        assert!(tracker.status.bass_hit < tracker.status.hit_threshold);
    }

    #[test]
    fn lf_envelopes_accept_a_120ms_bass_ramp_at_equal_playback_gains() {
        for gain in [1.0, 0.5] {
            let mut tracker = ArrangementTracker::new();
            let mut now = 0.0;
            let active = section(0.25 * gain, 0.15 * gain, 0.05 * gain, 0.025 * gain);
            let quiet = section(0.035 * gain, 0.002 * gain, 0.02 * gain, 0.012 * gain);
            feed(&mut tracker, active, 4.0, &mut now);
            feed(&mut tracker, quiet, 3.0, &mut now);
            assert!(tracker.eligible);
            let mut drops = 0;
            for step in 1..=24 {
                let amount = step as f32 / 24.0;
                let ramp = Spectrum {
                    rms: quiet.rms + (active.rms - quiet.rms) * amount,
                    linear_bands: std::array::from_fn(|band| {
                        quiet.linear_bands[band]
                            + (active.linear_bands[band] - quiet.linear_bands[band]) * amount
                    }),
                    ..Spectrum::default()
                };
                drops += feed(&mut tracker, ramp, 0.005, &mut now);
            }
            drops += feed(&mut tracker, active, 0.2, &mut now);
            assert_eq!(drops, 1, "one gradual LF return at gain {gain}");
        }
    }

    #[test]
    fn lf_envelopes_apply_the_configured_rms_floor_to_the_current_attack() {
        let mut settings = arrangement_test_settings();
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = section(0.25, 0.15, 0.05, 0.025);
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
        feed_settings(
            &mut tracker,
            section(0.035, 0.002, 0.02, 0.012),
            3.0,
            &mut now,
            &settings,
        );
        assert!(tracker.eligible);
        settings.arrangement.drop_min_rms_db = -6.0;
        assert_eq!(
            feed_settings(&mut tracker, active, 0.04, &mut now, &settings),
            0
        );
        assert!(tracker.eligible);
        assert!(tracker.status.bass_hit >= tracker.status.hit_threshold);
        settings.arrangement.drop_min_rms_db = -26.0;
        assert_eq!(
            feed_settings(&mut tracker, active, 0.01, &mut now, &settings),
            1
        );
        assert_eq!(tracker.status.drop_rms_threshold_db, -26.0);
    }

    #[test]
    fn lf_envelopes_initialize_and_reset_without_inventing_an_attack() {
        let mut tracker = ArrangementTracker::new();
        let active = section(0.25, 0.15, 0.05, 0.025);
        let settings = arrangement_test_settings();
        tracker.observe(&active, 0.005, 0.005, &settings);
        assert_eq!(tracker.status.bass_hit, 0.0);
        let mut now = 0.005;
        feed_settings(&mut tracker, active, 1.0, &mut now, &settings);
        assert!(tracker.status.bass_hit < 1e-5);
        tracker.reset();
        tracker.observe(&active, 2.0, 0.005, &settings);
        assert_eq!(tracker.status.bass_hit, 0.0);
    }

    #[test]
    fn energy_diagnostics_keep_the_threshold_and_reference_used_at_each_hop() {
        let mut settings = arrangement_test_settings();
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = section(0.25, 0.15, 0.05, 0.025);
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
        let saved = tracker.status;
        let decision_reference = tracker.reference_db;
        settings.arrangement.mix_loss_db = 0.25;
        settings.arrangement.shape_distance = 0.01;
        settings.arrangement.trend_change = 0.005;
        settings.arrangement.changed_fraction = 0.05;
        settings.arrangement.gap_hold = 0.05;
        // Uniform attenuation has no new shape, so this remains a candidate
        // measurement and the reference adapts after the decision is made.
        let quieter = Spectrum {
            rms: active.rms * 0.5,
            linear_bands: active.linear_bands.map(|v| v * 0.5),
            ..Spectrum::default()
        };
        tracker.observe(&quieter, now + 0.05, 0.05, &settings);
        let measured = tracker.status;
        assert_eq!(saved.mix_loss_threshold_db, 2.0);
        assert_eq!(measured.mix_loss_threshold_db, 0.25);
        assert_eq!(saved.shape_distance_threshold, 0.12);
        assert_eq!(saved.trend_change_threshold, 0.055);
        assert_eq!(saved.changed_fraction_threshold, 0.25);
        assert_eq!(saved.gap_hold_seconds, 1.2);
        assert_eq!(measured.shape_distance_threshold, 0.01);
        assert_eq!(measured.trend_change_threshold, 0.005);
        assert_eq!(measured.changed_fraction_threshold, 0.05);
        assert_eq!(measured.gap_hold_seconds, 0.05);
        assert!(measured.gap_history_ready);
        assert!(!measured.gap_conditions_met);
        assert_eq!(measured.gap_held_seconds, 0.0);
        assert_eq!(measured.gap_cooldown_remaining, 0.0);
        assert!(measured.energy_delta_db < -measured.mix_loss_threshold_db);
        let plotted_reference = measured.envelope_db - measured.energy_delta_db;
        assert!((plotted_reference - decision_reference).abs() < 1e-5);
        assert!((plotted_reference - tracker.reference_db).abs() > 0.001);
        assert!(!tracker.eligible, "energy alone does not confirm a gap");
        tracker.observe(&active, now + 0.1, 0.05, &settings);
        assert_eq!(measured.mix_loss_threshold_db, 0.25);
        assert_eq!(saved.mix_loss_threshold_db, 2.0);
    }

    #[test]
    fn separated_shape_and_energy_changes_do_not_supply_a_shared_gap_hold() {
        let bell = std::array::from_fn(|i| 0.01 + (-(i as f32 - 7.5).powi(2) / 12.0).exp());
        let dip = bell.map(|v| 1.1 - v);
        let active = Spectrum {
            rms: 0.25,
            linear_bands: bell.map(|v| v * 0.08),
            ..Spectrum::default()
        };
        let changed = Spectrum {
            linear_bands: dip.map(|v| v * 0.05),
            ..active
        };
        let mut settings = arrangement_test_settings();
        settings.arrangement.reference_time = 0.5;
        settings.arrangement.gap_hold = 0.2;
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);

        let mut shape_only_hops = 0;
        for _ in 0..600 {
            now += 0.005;
            tracker.observe(&changed, now, 0.005, &settings);
            let status = tracker.status;
            let shape_passes = status.shape_distance > status.shape_distance_threshold
                && (status.trend_change > status.trend_change_threshold
                    || status.changed_fraction >= status.changed_fraction_threshold);
            shape_only_hops += usize::from(shape_passes);
            assert!(status.energy_delta_db >= -status.mix_loss_threshold_db);
            assert!(!status.gap_conditions_met);
            assert_eq!(status.gap_held_seconds, 0.0);
        }
        assert!(
            shape_only_hops > 20,
            "fixture must first supply shape evidence"
        );

        // Waited for the new shape to become the reference, then lower only
        // its gain. The earlier shape evidence cannot be reused at this time.
        let lower = Spectrum {
            rms: changed.rms * 0.5,
            linear_bands: changed.linear_bands.map(|v| v * 0.5),
            ..changed
        };
        let mut energy_only_hops = 0;
        for _ in 0..200 {
            now += 0.005;
            tracker.observe(&lower, now, 0.005, &settings);
            let status = tracker.status;
            energy_only_hops += usize::from(status.energy_delta_db < -status.mix_loss_threshold_db);
            assert!(status.shape_distance <= status.shape_distance_threshold);
            assert!(!status.gap_conditions_met);
            assert_eq!(status.gap_held_seconds, 0.0);
            assert!(!tracker.eligible);
        }
        assert!(
            energy_only_hops > 20,
            "fixture must later supply energy evidence"
        );

        let mut coincident = ArrangementTracker::new();
        let mut simultaneous_now = 0.0;
        feed_settings(
            &mut coincident,
            active,
            4.0,
            &mut simultaneous_now,
            &settings,
        );
        let quiet_changed = Spectrum {
            rms: 0.04,
            ..changed
        };
        let mut saw_partial_hold = false;
        for _ in 0..200 {
            simultaneous_now += 0.005;
            coincident.observe(&quiet_changed, simultaneous_now, 0.005, &settings);
            let status = coincident.status;
            if status.gap_held_seconds > 0.0 && !coincident.eligible {
                assert!(status.gap_conditions_met);
                assert!(status.gap_held_seconds < status.gap_hold_seconds);
                saw_partial_hold = true;
            }
            if coincident.eligible {
                assert!(status.gap_conditions_met);
                assert_eq!(status.gap_held_seconds, status.gap_hold_seconds);
                break;
            }
        }
        assert!(
            saw_partial_hold,
            "coincident evidence must fill the real hold timer"
        );
        assert!(
            coincident.eligible,
            "coincident shape and loss must confirm"
        );
    }

    #[test]
    fn drop_floor_diagnostic_reports_the_configured_gate_after_confirmation() {
        let settings = arrangement_test_settings();
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = section(0.25, 0.15, 0.05, 0.025);
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
        feed_settings(
            &mut tracker,
            section(0.035, 0.002, 0.02, 0.012),
            2.0,
            &mut now,
            &settings,
        );
        assert!(tracker.eligible);
        let required = settings.arrangement.drop_min_rms_db;
        let hit = Spectrum {
            rms: active.rms * 2.0,
            linear_bands: active.linear_bands.map(|v| v * 2.0),
            ..active
        };
        assert!(tracker
            .observe(&hit, now + 0.005, 0.005, &settings)
            .is_some());
        assert_eq!(tracker.status.drop_rms_threshold_db, required);
        assert!(tracker.reference_db - 10.0 > required + 5.0);
        assert_eq!(
            tracker.status.gap_cooldown_remaining,
            settings.arrangement.drop_refractory
        );
    }

    #[test]
    fn lowering_gap_hold_updates_an_in_progress_candidate_without_resetting_it() {
        let mut settings = arrangement_test_settings();
        settings.arrangement.gap_hold = 4.0;
        // Exercise the shape hold without the independent loss hold arming first.
        settings.arrangement.mix_loss_hold = 10.0;
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        feed_settings(
            &mut tracker,
            section(0.25, 0.15, 0.05, 0.025),
            4.0,
            &mut now,
            &settings,
        );
        let quiet = section(0.035, 0.002, 0.02, 0.012);
        feed_settings(&mut tracker, quiet, 2.0, &mut now, &settings);
        assert!(!tracker.eligible && tracker.quiet_time > 0.1);
        settings.arrangement.gap_hold = 0.1;
        feed_settings(&mut tracker, quiet, 0.02, &mut now, &settings);
        assert!(tracker.eligible);
        assert_eq!(tracker.status.quiet_progress, 1.0);
        settings.arrangement.gap_hold = 3.0;
        feed_settings(&mut tracker, quiet, 0.02, &mut now, &settings);
        assert!(tracker.eligible, "an already confirmed gap stays latched");
        assert_eq!(tracker.status.quiet_progress, 1.0);
    }

    #[test]
    fn mix_loss_control_changes_which_spectral_transition_can_arm() {
        let bell = std::array::from_fn(|i| 0.01 + (-(i as f32 - 7.5).powi(2) / 12.0).exp());
        let dip = bell.map(|v| 1.1 - v);
        let active = Spectrum {
            rms: 0.25,
            linear_bands: bell.map(|v| v * 0.08),
            ..Spectrum::default()
        };
        let transition = Spectrum {
            rms: 0.177,
            linear_bands: dip.map(|v| v * 0.05),
            ..Spectrum::default()
        };
        for (loss, expected) in [(2.0, true), (6.0, false)] {
            let mut settings = arrangement_test_settings();
            settings.arrangement.mix_loss_db = loss;
            let mut tracker = ArrangementTracker::new();
            let mut now = 0.0;
            feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
            feed_settings(&mut tracker, transition, 2.0, &mut now, &settings);
            assert_eq!(tracker.eligible, expected, "mix-loss requirement {loss}");
        }
    }

    #[test]
    fn live_drop_controls_validate_energy_and_bass_attack_without_clearing_the_gap() {
        for tune_bass in [false, true] {
            let mut settings = arrangement_test_settings();
            let active = section(0.25, 0.15, 0.05, 0.025);
            let mut tracker = ArrangementTracker::new();
            let mut now = 0.0;
            feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
            feed_settings(
                &mut tracker,
                section(0.035, 0.002, 0.02, 0.012),
                3.0,
                &mut now,
                &settings,
            );
            assert!(tracker.eligible);
            if tune_bass {
                settings.arrangement.bass_hit_threshold = 0.6;
            } else {
                settings.arrangement.drop_min_rms_db = -6.0;
            }
            let hit = if tune_bass {
                // A modest LF rise lies below the stricter envelope setting,
                // then clears the ordinary setting while that rise persists.
                section(active.rms, 0.003, 0.05, 0.025)
            } else {
                active
            };
            assert_eq!(
                feed_settings(&mut tracker, hit, 0.02, &mut now, &settings),
                0
            );
            assert!(tracker.eligible);
            if tune_bass {
                settings.arrangement.bass_hit_threshold = 0.12;
            } else {
                settings.arrangement.drop_min_rms_db = -26.0;
            }
            assert_eq!(
                feed_settings(&mut tracker, hit, 0.02, &mut now, &settings),
                1
            );
        }
    }

    #[test]
    fn reference_memory_control_changes_adaptation_to_a_uniform_level_change() {
        let active = section(0.25, 0.15, 0.05, 0.025);
        let lower = Spectrum {
            rms: active.rms * 0.5,
            linear_bands: active.linear_bands.map(|v| v * 0.5),
            ..Spectrum::default()
        };
        let mut deficits = Vec::new();
        for memory in [0.5, 20.0] {
            let mut settings = arrangement_test_settings();
            settings.arrangement.reference_time = memory;
            // Isolate adaptation; the new loss-only route intentionally arms
            // on a sustained uniform level decrease once its hold completes.
            settings.arrangement.mix_loss_hold = 10.0;
            let mut tracker = ArrangementTracker::new();
            let mut now = 0.0;
            feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
            feed_settings(&mut tracker, lower, 4.0, &mut now, &settings);
            assert!(
                !tracker.eligible,
                "a uniform level change does not alter shape"
            );
            deficits.push(tracker.status.energy_delta_db.abs());
        }
        assert!(
            deficits[0] < 0.1 && deficits[1] > 3.0,
            "reference deficits: {deficits:?}"
        );
    }

    #[test]
    fn gain_changes_do_not_change_shape() {
        let input = [
            0.02, 0.04, 0.1, 0.2, 0.3, 0.2, 0.12, 0.09, 0.08, 0.05, 0.04, 0.03, 0.025, 0.02, 0.01,
            0.005,
        ];
        let a = shape(&input);
        for gain in [0.125, 0.5, 2.0] {
            let b = shape(&input.map(|v| v * gain));
            assert!(difference(&a, &b).0 < 1e-6);
        }
    }

    #[test]
    fn inverted_bell_changes_many_spectral_trends() {
        let bell = std::array::from_fn(|i| 0.01 + (-(i as f32 - 7.5).powi(2) / 12.0).exp());
        let dip = bell.map(|v| 1.1 - v);
        let (distance, trend, fraction) = difference(&shape(&bell), &shape(&dip));
        assert!(
            distance > 0.3 && trend > 0.1 && fraction > 0.5,
            "{distance} {trend} {fraction}"
        );
    }

    #[test]
    fn quiet_shape_transition_then_audible_bass_hit_confirms_once() {
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        feed(
            &mut tracker,
            section(0.25, 0.15, 0.05, 0.025),
            4.0,
            &mut now,
        );
        feed(
            &mut tracker,
            section(0.045, 0.003, 0.025, 0.015),
            2.0,
            &mut now,
        );
        assert!(
            tracker.eligible,
            "quiet changed section must arm: {:?}",
            tracker.status
        );
        let hit = section(0.25, 0.15, 0.05, 0.025);
        assert_eq!(feed(&mut tracker, hit, 0.02, &mut now), 1);
        assert!(!tracker.eligible);
        assert_eq!(feed(&mut tracker, hit, 1.0, &mut now), 0);
    }

    #[test]
    fn quiet_crackles_and_high_only_returns_do_not_confirm() {
        for kind in 0..2 {
            let mut tracker = ArrangementTracker::new();
            let mut now = 0.0;
            feed(
                &mut tracker,
                section(0.25, 0.15, 0.05, 0.025),
                4.0,
                &mut now,
            );
            feed(
                &mut tracker,
                section(0.035, 0.002, 0.02, 0.012),
                2.0,
                &mut now,
            );
            assert!(tracker.eligible);
            let hit = match kind {
                0 => section(0.025, 0.012, 0.008, 0.005),
                1 => section(0.2, 0.001, 0.005, 0.15),
                _ => unreachable!(),
            };
            assert_eq!(
                feed(&mut tracker, hit, 0.5, &mut now),
                0,
                "false drop kind {kind}"
            );
        }
    }

    #[test]
    fn a_real_silent_break_can_return_to_the_same_spectrum() {
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = section(0.25, 0.15, 0.05, 0.025);
        feed(&mut tracker, active, 4.0, &mut now);
        feed(&mut tracker, Spectrum::default(), 4.0, &mut now);
        assert!(tracker.eligible);
        let hit = active;
        assert_eq!(feed(&mut tracker, hit, 0.02, &mut now), 1);
    }

    #[test]
    fn a_quiet_mix_can_arm_but_inaudible_hits_still_cannot_confirm_a_drop() {
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        feed(
            &mut tracker,
            section(0.0316, 0.018, 0.008, 0.004),
            6.0,
            &mut now,
        );
        let click = section(0.025, 0.014, 0.006, 0.003);
        feed(&mut tracker, click, 0.02, &mut now);
        feed(
            &mut tracker,
            section(0.009, 0.0004, 0.005, 0.003),
            3.0,
            &mut now,
        );
        assert!(
            tracker.eligible,
            "a quiet musical transition can start a gap"
        );
        assert_eq!(feed(&mut tracker, click, 0.02, &mut now), 0);
    }

    #[test]
    fn strong_trend_change_with_three_db_loss_arms_at_loud_and_quiet_levels() {
        let bell = std::array::from_fn(|i| 0.01 + (-(i as f32 - 7.5).powi(2) / 12.0).exp());
        let dip = bell.map(|v| 1.1 - v);
        for gain in [1.0, 0.1, 0.01, 0.001] {
            let mut tracker = ArrangementTracker::new();
            let mut now = 0.0;
            let active = Spectrum {
                rms: 0.25 * gain,
                linear_bands: bell.map(|v| v * 0.08 * gain),
                ..Spectrum::default()
            };
            let quieter_changed = Spectrum {
                rms: 0.177 * gain,
                linear_bands: dip.map(|v| v * 0.05 * gain),
                ..Spectrum::default()
            };
            feed(&mut tracker, active, 4.0, &mut now);
            feed(&mut tracker, quieter_changed, 2.0, &mut now);
            assert!(tracker.status.trend_change > 0.055);
            assert!(
                tracker.eligible,
                "relative transition must arm at gain {gain}: {:?}",
                tracker.status
            );
        }
    }

    #[test]
    fn shape_gap_requires_both_trend_change_and_mix_energy_loss() {
        let bell = std::array::from_fn(|i| 0.01 + (-(i as f32 - 7.5).powi(2) / 12.0).exp());
        let dip = bell.map(|v| 1.1 - v);
        let active = Spectrum {
            rms: 0.25,
            linear_bands: bell.map(|v| v * 0.08),
            ..Spectrum::default()
        };
        let mut settings = arrangement_test_settings();
        settings.arrangement.mix_loss_hold = 10.0;
        for changed in [
            Spectrum {
                linear_bands: dip.map(|v| v * 0.05),
                ..active
            },
            Spectrum {
                rms: active.rms * 0.708,
                linear_bands: active.linear_bands.map(|v| v * 0.708),
                ..active
            },
        ] {
            let mut tracker = ArrangementTracker::new();
            let mut now = 0.0;
            feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
            feed_settings(&mut tracker, changed, 3.0, &mut now, &settings);
            assert!(
                !tracker.eligible,
                "the shape-supported path needs both spectral change and mix loss"
            );
        }
    }

    #[test]
    fn sustained_bass_cannot_validate_tiny_new_rumble_during_a_bright_return() {
        let mut settings = arrangement_test_settings();
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = section(0.3, 0.15, 0.12, 0.06);
        let quiet = section(0.035, 0.002, 0.02, 0.012);
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
        feed_settings(&mut tracker, quiet, 3.0, &mut now, &settings);
        assert!(
            tracker.eligible,
            "fixture must establish a quiet section: {:?}",
            tracker.status
        );
        let mut steady_bass = quiet;
        steady_bass.rms = 0.08;
        steady_bass.linear_bands[4] = 0.1;
        // Let LF become established while the RMS gate is deliberately closed.
        // Reopening that gate later must not turn held bass into a fresh onset.
        settings.arrangement.drop_min_rms_db = -6.0;
        feed_settings(&mut tracker, steady_bass, 2.0, &mut now, &settings);
        assert!(tracker.eligible);
        settings.arrangement.drop_min_rms_db = -26.0;
        let mut return_hit = active;
        return_hit.linear_bands[..6].fill(0.002);
        return_hit.linear_bands[4] = 0.1;
        for i in 0..4 {
            return_hit.linear_bands[i] = 0.004;
        }
        assert_eq!(
            feed_settings(&mut tracker, return_hit, 0.02, &mut now, &settings),
            0
        );
        assert!(tracker.status.bass_hit < tracker.status.hit_threshold);
        return_hit.linear_bands[4] = 0.25;
        assert_eq!(
            feed_settings(&mut tracker, return_hit, 0.02, &mut now, &settings),
            1
        );
    }

    #[test]
    fn recurring_one_second_bass_valleys_do_not_accumulate_a_gap() {
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = section(0.25, 0.15, 0.05, 0.025);
        feed(&mut tracker, active, 4.0, &mut now);
        for _ in 0..16 {
            feed(
                &mut tracker,
                section(0.045, 0.003, 0.025, 0.015),
                1.0,
                &mut now,
            );
            assert!(
                !tracker.eligible,
                "an ordinary rhythmic valley must stay unarmed"
            );
            feed(&mut tracker, active, 0.4, &mut now);
        }
    }

    #[test]
    fn confirmed_gap_survives_a_long_nonqualifying_section_until_a_drop() {
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = section(0.25, 0.15, 0.05, 0.025);
        feed(&mut tracker, active, 4.0, &mut now);
        let quiet = section(0.035, 0.002, 0.02, 0.012);
        feed(&mut tracker, quiet, 3.0, &mut now);
        assert!(tracker.eligible);
        // Recover overall level without changing LF, so no bass return can
        // consume the gap and the old 40-second expiry is the only escape.
        assert_eq!(
            feed(
                &mut tracker,
                Spectrum { rms: 0.20, ..quiet },
                45.0,
                &mut now
            ),
            0
        );
        assert!(!tracker.status.gap_conditions_met);
        assert!(
            tracker.eligible,
            "elapsed time cannot clear a confirmed gap"
        );
        assert_eq!(tracker.status.quiet_progress, 1.0);
        assert_eq!(feed(&mut tracker, active, 0.02, &mut now), 1);
        assert!(!tracker.eligible, "the drop consumes the gap");
        assert_eq!(tracker.status.quiet_progress, 0.0);
        assert_eq!(feed(&mut tracker, active, 1.0, &mut now), 0);
    }

    #[test]
    fn confirmed_gap_survives_recovery_and_live_gate_changes_until_a_drop() {
        let mut settings = arrangement_test_settings();
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        let active = section(0.25, 0.15, 0.05, 0.025);
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
        feed_settings(
            &mut tracker,
            section(0.035, 0.002, 0.02, 0.012),
            3.0,
            &mut now,
            &settings,
        );
        assert!(tracker.eligible);
        settings.arrangement.drop_min_rms_db = 0.0;
        assert_eq!(
            feed_settings(&mut tracker, active, 3.0, &mut now, &settings),
            0
        );
        assert!(
            tracker.eligible,
            "recovery without a valid drop cannot clear the gap"
        );
        assert_eq!(tracker.status.quiet_progress, 1.0);
        settings.arrangement.drop_min_rms_db = -26.0;
        assert_eq!(
            feed_settings(&mut tracker, active, 0.5, &mut now, &settings),
            0
        );
        assert!(
            tracker.eligible,
            "opening the RMS gate cannot consume established bass"
        );
        feed_settings(
            &mut tracker,
            section(0.25, 0.05, 0.05, 0.025),
            0.5,
            &mut now,
            &settings,
        );
        assert_eq!(
            feed_settings(&mut tracker, active, 0.02, &mut now, &settings),
            1
        );
        assert!(!tracker.eligible);
    }

    #[test]
    fn a_long_breakdown_retains_valid_evidence_and_clocks_work_after_a_day() {
        for origin in [0.0, 86_400.0] {
            let mut tracker = ArrangementTracker::new();
            let mut now = origin;
            let active = section(0.25, 0.15, 0.05, 0.025);
            feed(&mut tracker, active, 4.0, &mut now);
            feed(
                &mut tracker,
                section(0.035, 0.002, 0.02, 0.012),
                120.0,
                &mut now,
            );
            assert!(tracker.eligible, "a valid long breakdown must remain armed");
            let hit = active;
            assert_eq!(feed(&mut tracker, hit, 0.02, &mut now), 1);
            assert!((tracker.last_drop - origin - 124.0).abs() < 0.02);
        }
    }

    #[test]
    fn relative_gap_ignores_startup_beat_valleys_and_uniform_gain_dips() {
        let mut settings = arrangement_test_settings();
        // Isolate the shape-supported path from both level-only alternatives.
        settings.arrangement.gap_rms_db = -80.0;
        settings.arrangement.mix_loss_hold = 10.0;
        let mut tracker = ArrangementTracker::new();
        let mut now = 0.0;
        feed_settings(
            &mut tracker,
            section(0.001, 0.001, 0.001, 0.001),
            2.0,
            &mut now,
            &settings,
        );
        assert!(!tracker.eligible);
        let active = section(0.25, 0.15, 0.05, 0.025);
        feed_settings(&mut tracker, active, 4.0, &mut now, &settings);
        for _ in 0..12 {
            feed_settings(
                &mut tracker,
                section(0.015, 0.001, 0.008, 0.006),
                0.10,
                &mut now,
                &settings,
            );
            feed_settings(&mut tracker, active, 0.25, &mut now, &settings);
            assert!(!tracker.eligible);
        }
        let attenuated = Spectrum {
            rms: active.rms * 0.1,
            linear_bands: active.linear_bands.map(|v| v * 0.1),
            ..Spectrum::default()
        };
        feed_settings(&mut tracker, attenuated, 4.0, &mut now, &settings);
        assert!(
            !tracker.eligible,
            "same shape at a different volume must not arm: {:?}",
            tracker.status
        );
        tracker.reset();
        assert!(!tracker.eligible && !tracker.ready);
    }

    #[test]
    fn actual_stream_confirms_the_bass_return_without_waiting_for_shape_smoothing() {
        use super::super::spectrum::SpectrumAnalyzer;
        for gain in [1.0, 0.5, 0.25] {
            let samplerate = 48_000;
            let mut analyzer = SpectrumAnalyzer::new(samplerate);
            let mut tracker = ArrangementTracker::new();
            let mut drops = Vec::new();
            let mut armed = false;
            let mut random = 1234_u32;
            for sample in 0..samplerate * 10 {
                let time = sample as f32 / samplerate as f32;
                let quiet = (4.0..7.0).contains(&time);
                let bass_gain = if quiet { 0.002 } else { 0.24 };
                let bass = bass_gain
                    * ((std::f32::consts::TAU * 90.0 * time).sin()
                        + 0.65 * (std::f32::consts::TAU * 150.0 * time).sin());
                random = random.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = (random as f32 / u32::MAX as f32 - 0.5) * 0.02;
                let signal = bass + 0.05 * (std::f32::consts::TAU * 2200.0 * time).sin() + noise;
                if let Some(spectrum) = analyzer.push_stereo([signal * gain, -signal * gain]) {
                    if tracker
                        .observe(
                            &spectrum,
                            f64::from(time),
                            analyzer.hop_seconds(),
                            &arrangement_test_settings(),
                        )
                        .is_some()
                    {
                        drops.push(time);
                    }
                    if (6.0..7.0).contains(&time) {
                        armed |= tracker.eligible;
                    }
                }
            }
            assert!(
                armed,
                "the actual FFT shape must recognize the bassless bridge at gain {gain}"
            );
            assert_eq!(drops.len(), 1, "one sharp return at gain {gain}: {drops:?}");
            assert!(
                (7.0..7.08).contains(&drops[0]),
                "drop latency must stay below 80 ms: {drops:?}"
            );
        }
    }
}
