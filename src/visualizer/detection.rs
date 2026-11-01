//! Detector settings, streaming orchestration, and public diagnostics.
//!
//! Short-window attacks feed independent percussion classifiers; normalized
//! spectral trends and energy contrast feed the arrangement tracker. Legacy
//! temporal polygons remain readable for saved-configuration compatibility.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use super::arrangement::ArrangementTracker as DropTracker;
use super::percussion::PercussionSettings;
use super::percussion_spectrum::PercussionFrame;
use super::spectrum::{RelativeExcessSettings, Spectrum};
use super::transients::SharpOnsets;
use super::{Curve, BAND_COUNT};

pub const ABL_GAIN_MIN: f32 = 0.5;
pub const ABL_GAIN_MAX: f32 = 8.0;
pub const ABL_AMOUNT_MIN: f32 = 0.0;
pub const EXCESS_ATTACK_MIN: f32 = 0.05;
pub const EXCESS_ATTACK_MAX: f32 = 2.0;
pub const EXCESS_RELEASE_MIN: f32 = 0.05;
pub const EXCESS_RELEASE_MAX: f32 = 2.0;
pub const EXCESS_FLOOR_MIN: f32 = 0.0;
pub const EXCESS_FLOOR_MAX: f32 = 0.2;
pub const EXCESS_CAP_MIN: f32 = 0.1;
pub const EXCESS_CAP_MAX: f32 = 4.0;

pub const FLUX_ATTACK_MIN: f32 = 0.001;
pub const FLUX_ATTACK_MAX: f32 = 0.1;
pub const FLUX_RELEASE_MIN: f32 = 0.005;
pub const FLUX_RELEASE_MAX: f32 = 0.5;
pub const FLUX_AVERAGE_MIN: f32 = 0.05;
pub const FLUX_AVERAGE_MAX: f32 = 4.0;
pub const FLUX_SENSITIVITY_MIN: f32 = 0.5;
pub const FLUX_SENSITIVITY_MAX: f32 = 5.0;
pub const FLUX_THRESHOLD_MIN: f32 = 0.0;
/// Upper bound of the dedicated composite-onset minimum threshold.
pub const ONSET_THRESHOLD_MAX: f32 = 0.25;
pub const FLUX_INTERVAL_MIN: f32 = 0.005;
pub const FLUX_INTERVAL_MAX: f32 = 0.2;
pub const FLUX_SUPPRESSION_MIN: f32 = 0.0;
pub const FLUX_SUPPRESSION_MAX: f32 = 2.0;
pub const FLUX_PRE_GAIN_MIN: f32 = 0.01;
pub const FLUX_PRE_GAIN_MAX: f32 = 4.0;
pub const KICK_FREQUENCY_MIN_HZ: f32 = 20.0;
pub const KICK_FREQUENCY_MAX_HZ: f32 = 2_000.0;
pub const KICK_FREQUENCY_MIN_WIDTH_HZ: f32 = 10.0;
pub const CHOKE_MIN: f32 = 0.0;
pub const CHOKE_MAX: f32 = 2.0;
pub const QUAKE_FACTOR_MIN: f32 = 0.0;
pub const QUAKE_FACTOR_MAX: f32 = 50.0;
pub const SEQUENCE_EVERY_MIN: u32 = 1;
pub const SEQUENCE_EVERY_MAX: u32 = 32;
/// Allowed snare output RMS gate in dBFS.
pub(super) const ONSET_MIX_GATE_DB: f32 = -27.0;
pub const SNARE_RMS_GATE_MIN_DB: f32 = -60.0;
pub const SNARE_RMS_GATE_MAX_DB: f32 = 0.0;
pub const SNARE_RMS_GATE_DEFAULT_DB: f32 = -27.0;
/// Allowed gap gate level in dBFS.
pub const DROP_GAP_THRESHOLD_MIN: f32 = -60.0;
pub const DROP_GAP_THRESHOLD_MAX: f32 = 0.0;
pub const DROP_KERNEL_MIN: f32 = 0.0;
pub const DROP_KERNEL_MAX: f32 = 1.0;
pub const SPRING_STIFFNESS_MIN: f32 = 1.0;
pub const SPRING_STIFFNESS_MAX: f32 = 400.0;
pub const SPRING_DAMPING_MIN: f32 = 0.0;
pub const SPRING_DAMPING_MAX: f32 = 60.0;

const SPRING_STEP: f32 = 1.0 / 240.0;
const KERNEL_EPSILON: f32 = 1e-6;

/// The drop detector's onset kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropKernel {
    Onset,
}

impl DropKernel {
    pub const ALL: [Self; 1] = [Self::Onset];

    pub fn label(self) -> &'static str {
        match self {
            Self::Onset => "ONSET",
        }
    }
}

/// Per-band weights for the drop detector's onset projection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DropKernels {
    pub onset: [f32; BAND_COUNT],
}

impl Default for DropKernels {
    fn default() -> Self {
        Self {
            onset: [
                0.44955054, 0.6159004, 0.7489803, 0.8598802, 0.89315015, 0.8709702, 0.77116024,
                0.6269904, 0.5050005, 0.4051906, 0.31647065, 0.28320068, 0.2721107, 0.28320068,
                0.3608306, 0.67135036,
            ],
        }
    }
}

impl DropKernels {
    pub fn weights(self, kernel: DropKernel) -> [f32; BAND_COUNT] {
        match kernel {
            DropKernel::Onset => self.onset,
        }
    }

    pub fn set(&mut self, kernel: DropKernel, band: usize, value: f32) {
        assert!(
            band < BAND_COUNT,
            "drop kernel bands stay inside the spectrum"
        );
        let value = value.clamp(DROP_KERNEL_MIN, DROP_KERNEL_MAX);
        match kernel {
            DropKernel::Onset => self.onset[band] = value,
        }
    }

    fn validate(self) -> anyhow::Result<()> {
        for kernel in DropKernel::ALL {
            validate_kernel(kernel.label(), &self.weights(kernel))?;
        }
        Ok(())
    }
}

fn validate_kernel(name: &str, weights: &[f32; BAND_COUNT]) -> anyhow::Result<()> {
    for (band, weight) in weights.iter().enumerate() {
        anyhow::ensure!(
            weight.is_finite() && (DROP_KERNEL_MIN..=DROP_KERNEL_MAX).contains(weight),
            "{name} kernel band {band} must be finite and between \
             {DROP_KERNEL_MIN} and {DROP_KERNEL_MAX}"
        );
    }
    Ok(())
}

/// Per-band absolute-loudness shaping. The tracked high reference is scaled by
/// the gain and then passed through the curve.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BandOnsetSettings {
    pub abl_gain: f32,
    pub abl_curve: Curve,
    pub abl_amount: f32,
}

impl Default for BandOnsetSettings {
    fn default() -> Self {
        Self {
            abl_gain: 1.0,
            abl_curve: Curve::SCurve,
            abl_amount: 2.5,
        }
    }
}

impl BandOnsetSettings {
    pub fn loudness(self, high: f32) -> f32 {
        let scaled = (high * self.abl_gain).clamp(0.0, 1.0);
        self.abl_curve.apply(scaled, self.abl_amount)
    }
}

/// Largest number of analysis steps a half-second template can span.
pub const PATTERN_MAX_STEPS: usize = 192;

/// Which of the two temporal template polygons a vertex belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatternLayer {
    Inner,
    Outer,
}

/// Which of the two temporal templates a point belongs to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PatternKernel {
    #[default]
    Main,
    Suppression,
}

/// Normalized time-by-band polygon vertices. Time runs left to right and band
/// runs top to bottom, both in 0..1.
pub type PatternPolygon = Vec<[f32; 2]>;

/// A time-by-band onset template. The template is always half a second long;
/// two polygons define its field: the inner polygon is the maximum, the outer
/// polygon is the zero boundary, and the region between them is interpolated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalPattern {
    pub inner: PatternPolygon,
    pub outer: PatternPolygon,
    pub suppression_inner: PatternPolygon,
    pub suppression_outer: PatternPolygon,
    /// Fraction of the half-second template used for matching, 0.1..1.0.
    #[serde(default = "default_pattern_area")]
    pub effective: f32,
}

fn default_pattern_area() -> f32 {
    1.0
}

impl Default for TemporalPattern {
    fn default() -> Self {
        Self::kick_initial()
    }
}

impl TemporalPattern {
    pub fn kick_initial() -> Self {
        Self {
            inner: vec![
                [0.0, 0.33803162],
                [0.032534927, 0.15787412],
                [0.10334638, 0.04483413],
                [0.16458872, 0.0],
                [0.3042978, 0.0],
                [0.26984897, 0.10135413],
                [0.16267489, 0.16140662],
                [0.029481063, 0.35151303],
                [0.0, 0.5645693],
                [0.0038275793, 0.7619316],
                [0.034291036, 0.878736],
                [0.004243819, 0.9872462],
                [0.0, 1.0],
            ],
            outer: vec![
                [0.0, 0.23205662],
                [0.028707279, 0.08369163],
                [0.12248461, 0.0],
                [0.33491898, 0.0],
                [0.27559045, 0.16140662],
                [0.16458872, 0.19673163],
                [0.09760491, 0.42987663],
                [0.049759332, 0.5853066],
                [0.049759332, 0.7407366],
                [0.06124227, 1.0],
                [0.0, 1.0],
            ],
            suppression_inner: vec![
                [0.0, 0.0],
                [0.075829335, 0.0],
                [0.019845905, 0.08775653],
                [0.0, 0.26498532],
            ],
            suppression_outer: vec![
                [0.0, 0.0],
                [0.1090047, 0.0],
                [0.040580507, 0.102450065],
                [0.0021032926, 0.31054506],
            ],
            effective: 0.1,
        }
    }

    pub fn snare_initial() -> Self {
        Self {
            inner: vec![
                [0.07615377, 0.31207043],
                [0.0842307, 0.43143383],
                [0.07615377, 0.5110094],
                [0.07076915, 0.5574285],
                [0.06538454, 0.6403197],
                [0.041830614, 0.73819804],
                [0.0049211713, 0.82108927],
                [0.011552064, 0.9736742],
                [0.0, 0.98968726],
                [0.0061537432, 0.268967],
            ],
            outer: vec![
                [0.0, 0.5762838],
                [0.0034614343, 0.22586356],
                [0.09230763, 0.20596966],
                [0.2270109, 0.43893924],
                [0.17328197, 0.5024324],
                [0.16706158, 0.7609291],
                [0.0470679, 0.90050524],
                [0.019845905, 1.0],
                [0.0, 1.0],
                [0.0, 0.89132124],
            ],
            suppression_inner: vec![
                [0.0, 0.053319808],
                [0.11022358, 0.042432208],
                [0.10742805, 0.23840904],
                [0.0, 0.23115064],
            ],
            suppression_outer: vec![
                [0.0, 0.0],
                [0.1325878, 0.0],
                [0.12979227, 0.30010545],
                [0.0, 0.26744264],
            ],
            effective: 0.13,
        }
    }

    /// A short, high-band transient that follows a hi-hat rather than a snare.
    pub fn hat_initial() -> Self {
        Self {
            inner: vec![
                [0.0, 0.92790276],
                [0.0, 0.8149811],
                [0.048392296, 0.8399954],
                [0.082431, 0.91789705],
                [0.07956027, 0.96292275],
                [0.05, 1.0],
                [0.0, 1.0],
            ],
            outer: vec![
                [0.0, 0.7052756],
                [0.022145579, 0.7302899],
                [0.050852925, 0.7503013],
                [0.09391394, 0.82784563],
                [0.12235066, 0.9754299],
                [0.09, 1.0],
                [0.0, 1.0],
            ],
            suppression_inner: vec![
                [0.0, 0.35471818],
                [0.088582575, 0.41475248],
                [0.085711844, 0.50980675],
                [0.0, 0.5623368],
            ],
            suppression_outer: vec![
                [0.0, 0.29968673],
                [0.11154845, 0.38973817],
                [0.120160654, 0.52731675],
                [0.0, 0.6173682],
            ],
            effective: 0.1,
        }
    }

    /// Approximates a previous step-grid pattern with a band-region rectangle.
    pub fn from_legacy_steps(steps: &[[f32; BAND_COUNT]]) -> Self {
        let mut minimum = BAND_COUNT;
        let mut maximum = 0;
        for step in steps {
            for (band, value) in step.iter().enumerate() {
                if *value > 0.0 {
                    minimum = minimum.min(band);
                    maximum = maximum.max(band);
                }
            }
        }
        if minimum >= BAND_COUNT {
            return Self::kick_initial();
        }
        let low = minimum as f32 / BAND_COUNT as f32;
        let high = (maximum + 1) as f32 / BAND_COUNT as f32;
        let start = if steps.len() == 1 { 0.8 } else { 0.0 };
        Self {
            inner: rectangle(start, 1.0, low, high),
            outer: rectangle(start, 1.0, (low - 0.06).max(0.0), (high + 0.06).min(1.0)),
            suppression_inner: Vec::new(),
            suppression_outer: Vec::new(),
            effective: 1.0,
        }
    }

    pub fn contains_band(&self, band: usize) -> bool {
        let y = (band as f32 + 0.5) / BAND_COUNT as f32;
        (0..16).any(|step| self.field_value((step as f32 + 0.5) / 16.0, y) > 0.01)
    }

    fn validate(&self) -> anyhow::Result<()> {
        validate_polygon("inner", &self.inner)?;
        validate_polygon("outer", &self.outer)?;
        validate_polygon("suppression inner", &self.suppression_inner)?;
        validate_polygon("suppression outer", &self.suppression_outer)?;
        anyhow::ensure!(
            self.effective.is_finite() && (0.1..=1.0).contains(&self.effective),
            "temporal pattern effective area must be between 10% and 100%"
        );
        Ok(())
    }

    pub fn polygons(&self, kernel: PatternKernel) -> (&PatternPolygon, &PatternPolygon) {
        match kernel {
            PatternKernel::Main => (&self.inner, &self.outer),
            PatternKernel::Suppression => (&self.suppression_inner, &self.suppression_outer),
        }
    }

    /// Field value at normalized time and band.
    pub fn field_value(&self, x: f32, y: f32) -> f32 {
        field_value_for(&self.inner, &self.outer, x, y)
    }

    pub fn suppression_field_value(&self, x: f32, y: f32) -> f32 {
        field_value_for(&self.suppression_inner, &self.suppression_outer, x, y)
    }

    /// Effective field at normalized time and band. The retained part starts
    /// at the template start; the end of the effective area is faded over its
    /// last 10%.
    pub fn effective_field_value(&self, x: f32, y: f32) -> f32 {
        self.effective_field_value_for(PatternKernel::Main, x, y)
    }

    pub fn suppression_effective_field_value(&self, x: f32, y: f32) -> f32 {
        self.effective_field_value_for(PatternKernel::Suppression, x, y)
    }

    pub fn effective_field_value_for(&self, kernel: PatternKernel, x: f32, y: f32) -> f32 {
        let effective = self.effective.clamp(0.1, 1.0);
        if x > effective {
            return 0.0;
        }
        let within = (x / effective).clamp(0.0, 1.0);
        let fade = if effective < 1.0 {
            ((1.0 - within) / 0.1).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let (inner, outer) = self.polygons(kernel);
        field_value_for(inner, outer, x, y) * fade
    }

    #[cfg(test)]
    fn sample_steps(&self, kernel: PatternKernel, steps: usize) -> Vec<[f32; BAND_COUNT]> {
        let effective = self.effective.clamp(0.1, 1.0);
        (0..steps)
            .map(|step| {
                let within = (step as f32 + 0.5) / steps as f32;
                let x = within * effective;
                std::array::from_fn(|band| {
                    self.effective_field_value_for(
                        kernel,
                        x,
                        (band as f32 + 0.5) / BAND_COUNT as f32,
                    )
                })
            })
            .collect()
    }

    /// Matches the template against the newest part of the onset history and
    /// returns the main-kernel excitation, the suppression-kernel inhibition,
    /// and their residual after suppression.
    #[cfg(test)]
    fn project(
        &self,
        history: &VecDeque<[f32; BAND_COUNT]>,
        suppression: f32,
        pre_gain: f32,
        steps: usize,
    ) -> OnsetProjection {
        let take = steps.clamp(1, PATTERN_MAX_STEPS).min(history.len());
        if take == 0 {
            return OnsetProjection::default();
        }
        let effective_steps =
            ((take as f32 * self.effective.clamp(0.1, 1.0)).ceil() as usize).clamp(1, take);
        let offset = history.len() - effective_steps;
        let pattern = self.sample_steps(PatternKernel::Main, effective_steps);
        let suppressor_weights = self.sample_steps(PatternKernel::Suppression, effective_steps);
        let mut excited = 0.0;
        let mut excitation_weight = 0.0;
        let mut inhibited = 0.0;
        let mut inhibition_weight = 0.0;
        for (step_index, weights) in pattern.iter().enumerate() {
            let flow = &history[offset + step_index];
            for band in 0..BAND_COUNT {
                let weight = weights[band].clamp(0.0, 1.0);
                excited += weight * flow[band];
                excitation_weight += weight;
                let suppressor = suppressor_weights[step_index][band].clamp(0.0, 1.0);
                inhibited += suppressor * flow[band];
                inhibition_weight += suppressor;
            }
        }
        let excitation = if excitation_weight > KERNEL_EPSILON {
            excited / excitation_weight
        } else {
            0.0
        } * pre_gain;
        let inhibition = if inhibition_weight > KERNEL_EPSILON {
            inhibited / inhibition_weight
        } else {
            0.0
        } * pre_gain;
        let suppressed = suppression * inhibition;
        OnsetProjection {
            inhibition,
            suppressed,
            residual: (excitation - suppressed).max(0.0),
        }
    }
}

fn rectangle(x0: f32, x1: f32, y0: f32, y1: f32) -> PatternPolygon {
    vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
}

fn field_value_for(inner: &[[f32; 2]], outer: &[[f32; 2]], x: f32, y: f32) -> f32 {
    if inner.len() < 3 || outer.len() < 3 {
        return 0.0;
    }
    if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return 0.0;
    }
    if point_in_polygon(inner, x, y) {
        return 1.0;
    }
    if !point_in_polygon(outer, x, y) {
        return 0.0;
    }
    let inner_distance = distance_to_polygon(inner, x, y).max(KERNEL_EPSILON);
    let outer_distance = distance_to_polygon(outer, x, y).max(KERNEL_EPSILON);
    (outer_distance / (inner_distance + outer_distance)).clamp(0.0, 1.0)
}

fn validate_polygon(name: &str, points: &[[f32; 2]]) -> anyhow::Result<()> {
    anyhow::ensure!(
        (3..=32).contains(&points.len()),
        "temporal {name} polygon must have between 3 and 32 vertices"
    );
    for (index, point) in points.iter().enumerate() {
        anyhow::ensure!(
            point[0].is_finite()
                && point[1].is_finite()
                && (0.0..=1.0).contains(&point[0])
                && (0.0..=1.0).contains(&point[1]),
            "temporal {name} vertex {index} must be inside the normalized grid"
        );
    }
    Ok(())
}

fn point_in_polygon(points: &[[f32; 2]], x: f32, y: f32) -> bool {
    if points.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut previous = points.len() - 1;
    for current in 0..points.len() {
        let a = points[current];
        let b = points[previous];
        let crosses = (a[1] > y) != (b[1] > y)
            && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1] + f32::EPSILON) + a[0];
        if crosses {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

fn distance_to_polygon(points: &[[f32; 2]], x: f32, y: f32) -> f32 {
    let mut distance = f32::INFINITY;
    if points.len() < 2 {
        return distance;
    }
    let mut previous = points.len() - 1;
    for current in 0..points.len() {
        distance = distance.min(distance_to_segment(points[previous], points[current], x, y));
        previous = current;
    }
    distance
}

fn distance_to_segment(a: [f32; 2], b: [f32; 2], x: f32, y: f32) -> f32 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let length_squared = dx * dx + dy * dy;
    if length_squared <= f32::EPSILON {
        return ((x - a[0]).powi(2) + (y - a[1]).powi(2)).sqrt();
    }
    let t = (((x - a[0]) * dx + (y - a[1]) * dy) / length_squared).clamp(0.0, 1.0);
    let px = a[0] + t * dx;
    let py = a[1] + t * dy;
    ((x - px).powi(2) + (y - py).powi(2)).sqrt()
}

/// One live control of the spectral arrangement detector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArrangementSetting {
    MixLossDb,
    MixLossHold,
    ShapeDistance,
    TrendChange,
    ChangedFraction,
    GapHold,
    GapRmsDb,
    GapRmsHold,
    ReferenceTime,
    BassFastTime,
    BassSlowTime,
    BassHitThreshold,
    DropMinRmsDb,
    DropRefractory,
}

impl ArrangementSetting {
    pub const ALL: [Self; 14] = [
        Self::MixLossDb,
        Self::MixLossHold,
        Self::ShapeDistance,
        Self::TrendChange,
        Self::ChangedFraction,
        Self::GapHold,
        Self::GapRmsDb,
        Self::GapRmsHold,
        Self::ReferenceTime,
        Self::BassFastTime,
        Self::BassSlowTime,
        Self::BassHitThreshold,
        Self::DropMinRmsDb,
        Self::DropRefractory,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::MixLossDb => "Mix loss",
            Self::MixLossHold => "Mix loss hold",
            Self::ShapeDistance => "Shape change",
            Self::TrendChange => "Trend change",
            Self::ChangedFraction => "Changed area",
            Self::GapHold => "Gap hold",
            Self::GapRmsDb => "RMS gap below",
            Self::GapRmsHold => "RMS gap hold",
            Self::ReferenceTime => "Reference",
            Self::BassFastTime => "Fast LF envelope",
            Self::BassSlowTime => "Slow LF envelope",
            Self::BassHitThreshold => "Bass attack",
            Self::DropMinRmsDb => "Drop RMS",
            Self::DropRefractory => "Drop interval",
        }
    }

    pub fn range(self) -> (f32, f32) {
        match self {
            Self::MixLossDb => (0.25, 12.0),
            Self::MixLossHold => (0.05, 10.0),
            Self::ShapeDistance => (0.01, 0.6),
            Self::TrendChange => (0.005, 0.3),
            Self::ChangedFraction => (0.05, 1.0),
            Self::GapHold => (0.05, 4.0),
            Self::GapRmsDb => (-80.0, 0.0),
            Self::GapRmsHold => (0.05, 10.0),
            Self::ReferenceTime => (0.5, 20.0),
            Self::BassFastTime => (0.002, 0.200),
            Self::BassSlowTime => (0.020, 2.0),
            Self::BassHitThreshold => (0.02, 0.6),
            Self::DropMinRmsDb => (-60.0, 0.0),
            Self::DropRefractory => (0.25, 30.0),
        }
    }

    pub fn step(self) -> f32 {
        match self {
            Self::MixLossDb | Self::ReferenceTime | Self::DropRefractory => 0.25,
            Self::ShapeDistance | Self::BassHitThreshold => 0.01,
            Self::TrendChange => 0.005,
            Self::BassFastTime => 0.001,
            Self::BassSlowTime => 0.005,
            Self::MixLossHold | Self::ChangedFraction | Self::GapHold | Self::GapRmsHold => 0.05,
            Self::DropMinRmsDb | Self::GapRmsDb => 1.0,
        }
    }

    pub fn is_time(self) -> bool {
        matches!(
            self,
            Self::MixLossHold
                | Self::GapHold
                | Self::GapRmsHold
                | Self::ReferenceTime
                | Self::BassFastTime
                | Self::BassSlowTime
                | Self::DropRefractory
        )
    }

    pub fn format(self, value: f32) -> String {
        match self {
            Self::MixLossDb => format!("{value:.2} dB"),
            Self::DropMinRmsDb | Self::GapRmsDb => format!("{value:.1} dB"),
            Self::MixLossHold
            | Self::GapHold
            | Self::GapRmsHold
            | Self::ReferenceTime
            | Self::BassFastTime
            | Self::BassSlowTime
            | Self::DropRefractory => {
                if value < 1.0 {
                    format!("{:.0} ms", value * 1000.0)
                } else {
                    format!("{value:.2} s")
                }
            }
            _ => format!("{:.1}%", value * 100.0),
        }
    }
}

/// Persisted controls used directly by gap arming and drop confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArrangementSettings {
    pub mix_loss_db: f32,
    /// Independent gap confirmation after this continuous time above the mix-loss requirement.
    pub mix_loss_hold: f32,
    pub shape_distance: f32,
    pub trend_change: f32,
    pub changed_fraction: f32,
    pub gap_hold: f32,
    /// Independent gap confirmation when analysis-window RMS stays below this level.
    pub gap_rms_db: f32,
    /// Required continuous time below the absolute RMS threshold, in seconds.
    pub gap_rms_hold: f32,
    pub reference_time: f32,
    /// LF onset envelope time constants in seconds; slow must exceed fast by 5 ms.
    pub bass_fast_time: f32,
    pub bass_slow_time: f32,
    pub bass_hit_threshold: f32,
    pub drop_min_rms_db: f32,
    pub drop_refractory: f32,
}

impl Default for ArrangementSettings {
    fn default() -> Self {
        Self {
            mix_loss_db: 1.0,
            mix_loss_hold: 0.906454,
            shape_distance: 0.2,
            trend_change: 0.075,
            changed_fraction: 0.25,
            gap_hold: 0.05,
            gap_rms_db: -24.0,
            gap_rms_hold: 0.5,
            reference_time: 0.5,
            bass_fast_time: 0.05005731,
            bass_slow_time: 0.5005731,
            bass_hit_threshold: 0.35,
            drop_min_rms_db: -24.0,
            drop_refractory: 1.0164245,
        }
    }
}

impl ArrangementSettings {
    pub fn value(self, setting: ArrangementSetting) -> f32 {
        match setting {
            ArrangementSetting::MixLossDb => self.mix_loss_db,
            ArrangementSetting::MixLossHold => self.mix_loss_hold,
            ArrangementSetting::ShapeDistance => self.shape_distance,
            ArrangementSetting::TrendChange => self.trend_change,
            ArrangementSetting::ChangedFraction => self.changed_fraction,
            ArrangementSetting::GapHold => self.gap_hold,
            ArrangementSetting::GapRmsDb => self.gap_rms_db,
            ArrangementSetting::GapRmsHold => self.gap_rms_hold,
            ArrangementSetting::ReferenceTime => self.reference_time,
            ArrangementSetting::BassFastTime => self.bass_fast_time,
            ArrangementSetting::BassSlowTime => self.bass_slow_time,
            ArrangementSetting::BassHitThreshold => self.bass_hit_threshold,
            ArrangementSetting::DropMinRmsDb => self.drop_min_rms_db,
            ArrangementSetting::DropRefractory => self.drop_refractory,
        }
    }

    pub fn set(&mut self, setting: ArrangementSetting, value: f32) {
        if !value.is_finite() {
            return;
        }
        let (low, high) = setting.range();
        let value = value.clamp(low, high);
        let value = match setting {
            ArrangementSetting::BassFastTime => value.min(self.bass_slow_time - 0.005),
            ArrangementSetting::BassSlowTime => value.max(self.bass_fast_time + 0.005),
            _ => value,
        };
        *match setting {
            ArrangementSetting::MixLossDb => &mut self.mix_loss_db,
            ArrangementSetting::MixLossHold => &mut self.mix_loss_hold,
            ArrangementSetting::ShapeDistance => &mut self.shape_distance,
            ArrangementSetting::TrendChange => &mut self.trend_change,
            ArrangementSetting::ChangedFraction => &mut self.changed_fraction,
            ArrangementSetting::GapHold => &mut self.gap_hold,
            ArrangementSetting::GapRmsDb => &mut self.gap_rms_db,
            ArrangementSetting::GapRmsHold => &mut self.gap_rms_hold,
            ArrangementSetting::ReferenceTime => &mut self.reference_time,
            ArrangementSetting::BassFastTime => &mut self.bass_fast_time,
            ArrangementSetting::BassSlowTime => &mut self.bass_slow_time,
            ArrangementSetting::BassHitThreshold => &mut self.bass_hit_threshold,
            ArrangementSetting::DropMinRmsDb => &mut self.drop_min_rms_db,
            ArrangementSetting::DropRefractory => &mut self.drop_refractory,
        } = value;
    }

    pub fn validate(self) -> anyhow::Result<Self> {
        for setting in ArrangementSetting::ALL {
            let value = self.value(setting);
            let (low, high) = setting.range();
            anyhow::ensure!(
                value.is_finite() && (low..=high).contains(&value),
                "arrangement {} must be finite and between {low} and {high}",
                setting.label()
            );
        }
        anyhow::ensure!(
            self.bass_slow_time + f32::EPSILON >= self.bass_fast_time + 0.005,
            "arrangement slow LF envelope must be at least 5 ms slower than the fast LF envelope"
        );
        Ok(self)
    }
}

const BIN_GAIN_MAX_HZ: f32 = 192_000.0;
const BIN_GAIN_MAX_POINTS: usize = 8192;

/// A physical FFT bin, addressed in frequency so saved edits survive a new grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BinAddress {
    pub center_millihz: u32,
    pub width_millihz: u32,
}

impl Default for BinAddress {
    fn default() -> Self {
        Self {
            center_millihz: 0,
            width_millihz: 1,
        }
    }
}

impl BinAddress {
    pub fn new(center_hz: f32, bin_width_hz: f32) -> Self {
        let center = if center_hz.is_finite() {
            center_hz.clamp(0.0, BIN_GAIN_MAX_HZ)
        } else {
            0.0
        };
        let width = if bin_width_hz.is_finite() {
            bin_width_hz.clamp(0.001, BIN_GAIN_MAX_HZ)
        } else {
            0.001
        };
        Self {
            center_millihz: (center * 1000.0).round() as u32,
            width_millihz: ((width * 1000.0).round() as u32).max(1),
        }
    }

    pub fn center_hz(self) -> f32 {
        self.center_millihz as f32 / 1000.0
    }
    pub fn width_hz(self) -> f32 {
        self.width_millihz as f32 / 1000.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinGainPoint {
    pub hz: f32,
    pub gain: f32,
}

/// Piecewise-linear attenuation. Empty curves and frequencies outside a curve
/// have unity gain; neighboring anchors make an edit local to the selected bin.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FrequencyGainProfile {
    pub points: Vec<BinGainPoint>,
}

impl FrequencyGainProfile {
    pub fn gain_at(&self, hz: f32) -> f32 {
        if !hz.is_finite() {
            return 1.0;
        }
        match self
            .points
            .binary_search_by(|point| point.hz.total_cmp(&hz))
        {
            Ok(index) => self.points[index].gain,
            Err(0) => 1.0,
            Err(index) if index == self.points.len() => 1.0,
            Err(index) => {
                let left = self.points[index - 1];
                let right = self.points[index];
                let fraction = (hz - left.hz) / (right.hz - left.hz);
                left.gain + (right.gain - left.gain) * fraction
            }
        }
    }

    pub fn set_bin(&mut self, address: BinAddress, gain: f32) {
        if !gain.is_finite() {
            return;
        }
        let address = BinAddress::new(address.center_hz(), address.width_hz());
        let maximum = (BIN_GAIN_MAX_HZ * 1000.0) as u32;
        let coordinates = [
            address.center_millihz.saturating_sub(address.width_millihz),
            address.center_millihz,
            address
                .center_millihz
                .saturating_add(address.width_millihz)
                .min(maximum),
        ];
        let missing = coordinates
            .iter()
            .enumerate()
            .filter(|(index, coordinate)| {
                !coordinates[..*index].contains(coordinate)
                    && self
                        .points
                        .binary_search_by(|point| {
                            point.hz.total_cmp(&(**coordinate as f32 / 1000.0))
                        })
                        .is_err()
            })
            .count();
        if self.points.len() + missing > BIN_GAIN_MAX_POINTS {
            return;
        }
        let low = coordinates[0] as f32 / 1000.0;
        let high = coordinates[2] as f32 / 1000.0;
        let low_gain = self.gain_at(low);
        let high_gain = self.gain_at(high);
        self.insert(BinGainPoint {
            hz: low,
            gain: low_gain,
        });
        self.insert(BinGainPoint {
            hz: high,
            gain: high_gain,
        });
        self.insert(BinGainPoint {
            hz: address.center_hz(),
            gain: gain.clamp(0.0, 1.0),
        });
    }

    fn insert(&mut self, point: BinGainPoint) {
        match self
            .points
            .binary_search_by(|existing| existing.hz.total_cmp(&point.hz))
        {
            Ok(index) => self.points[index] = point,
            Err(index) => self.points.insert(index, point),
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.points.len() <= BIN_GAIN_MAX_POINTS,
            "bin attenuation profile has too many points"
        );
        let mut previous = None;
        for point in &self.points {
            anyhow::ensure!(
                point.hz.is_finite() && (0.0..=BIN_GAIN_MAX_HZ).contains(&point.hz),
                "bin attenuation frequency must be finite and inside the supported spectrum"
            );
            anyhow::ensure!(
                point.gain.is_finite() && (0.0..=1.0).contains(&point.gain),
                "bin attenuation gain must be finite and between zero and one"
            );
            anyhow::ensure!(
                previous.is_none_or(|hz| point.hz > hz),
                "bin attenuation frequencies must be unique and increasing"
            );
            previous = Some(point.hz);
        }
        Ok(())
    }
}

/// Independent detector attenuation curves. Channels are kick, snare, and hat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PercussionBinGains {
    pub kick: FrequencyGainProfile,
    pub snare: FrequencyGainProfile,
    pub hat: FrequencyGainProfile,
}

impl Default for PercussionBinGains {
    fn default() -> Self {
        Self {
            kick: FrequencyGainProfile {
                points: [
                    (0.0, 1.0),
                    (23.437, 0.06374729),
                    (23.438, 0.0637074),
                    (46.875, 0.22908208),
                    (46.876, 0.9999574),
                    (70.312, 0.37381178),
                    (70.313, 0.3737849),
                    (93.75, 0.6631906),
                    (93.751, 1.0),
                    (117.188, 1.0),
                ]
                .into_iter()
                .map(|(hz, gain)| BinGainPoint { hz, gain })
                .collect(),
            },
            snare: FrequencyGainProfile::default(),
            hat: FrequencyGainProfile::default(),
        }
    }
}

impl PercussionBinGains {
    pub fn profile(&self, channel: usize) -> &FrequencyGainProfile {
        match channel {
            0 => &self.kick,
            1 => &self.snare,
            2 => &self.hat,
            _ => panic!("unknown percussion channel"),
        }
    }

    fn profile_mut(&mut self, channel: usize) -> &mut FrequencyGainProfile {
        match channel {
            0 => &mut self.kick,
            1 => &mut self.snare,
            2 => &mut self.hat,
            _ => panic!("unknown percussion channel"),
        }
    }

    pub fn value(&self, channel: usize, address: BinAddress) -> f32 {
        self.profile(channel).gain_at(address.center_hz())
    }

    pub fn set_bin(&mut self, channel: usize, address: BinAddress, gain: f32) {
        self.profile_mut(channel).set_bin(address, gain);
    }

    pub fn reset(&mut self, channel: usize) {
        self.profile_mut(channel).points.clear();
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        for channel in 0..3 {
            self.profile(channel).validate()?;
        }
        Ok(())
    }
}

/// Detector configuration. Legacy temporal fields remain readable and roundtrip
/// for compatibility; the inspector exposes only controls used by the new DSP.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "DetectorSettingsRaw", into = "DetectorSettingsRaw")]
pub struct DetectorSettings {
    /// Revision-three causal event features and paired onset populations.
    pub percussion: PercussionSettings,
    pub arrangement: ArrangementSettings,
    pub bin_gains: PercussionBinGains,
    /// Per-band absolute-loudness shaping used by the onset flow.
    pub band_onset: [BandOnsetSettings; BAND_COUNT],
    /// Per-bin relative-excess background detector.
    pub excess: RelativeExcessSettings,
    /// Bounds of the kick body's raw-FFT attack region, in Hz.
    pub kick_low_hz: f32,
    pub kick_high_hz: f32,
    /// Legacy temporal template, retained for saved-configuration compatibility.
    pub kick_pattern: TemporalPattern,
    /// Legacy temporal projection gain; unused by sharp-attack detection.
    pub kick_onset_pre_gain: f32,
    /// Legacy envelope time constants. Live controls begin at `average` below.
    pub kick_onset_attack: f32,
    pub kick_onset_release: f32,
    pub kick_onset_average: f32,
    pub kick_onset_sensitivity: f32,
    pub kick_onset_min_threshold: f32,
    pub kick_onset_min_interval: f32,
    /// Legacy suppression weight; the new classifiers validate regions directly.
    pub kick_onset_suppression: f32,
    /// Legacy choke settings. Coincident validated percussion now stays independent.
    pub kick_snare_choke: f32,
    pub kick_drop_choke: f32,
    pub kick_quake_factor: f32,
    /// A new quake direction is taken every this many kicks.
    pub kick_sequence_every: u32,
    /// Legacy temporal template.
    pub snare_pattern: TemporalPattern,
    /// Legacy projection gain.
    pub snare_onset_pre_gain: f32,
    /// Legacy envelope time constants. Live controls begin at `average` below.
    pub snare_onset_attack: f32,
    pub snare_onset_release: f32,
    pub snare_onset_average: f32,
    pub snare_onset_sensitivity: f32,
    pub snare_onset_min_threshold: f32,
    pub snare_onset_min_interval: f32,
    /// Legacy temporal suppression weight.
    pub snare_onset_suppression: f32,
    /// Minimum broadband RMS in dBFS required to emit a confirmed snare.
    pub snare_min_rms_db: f32,
    pub snare_flip_every: u32,
    pub snare_poi_switches: u32,
    pub snare_choke: f32,
    pub flip_stiffness: f32,
    pub flip_damping: f32,
    /// Legacy temporal template.
    pub hat_pattern: TemporalPattern,
    /// Legacy projection gain.
    pub hat_onset_pre_gain: f32,
    pub hat_onset_attack: f32,
    pub hat_onset_release: f32,
    pub hat_onset_average: f32,
    pub hat_onset_sensitivity: f32,
    pub hat_onset_min_threshold: f32,
    pub hat_onset_min_interval: f32,
    /// Legacy temporal suppression weight.
    pub hat_onset_suppression: f32,
    /// Legacy drop projection weights; bass confirmation now uses actual attack energy.
    pub drop_kernels: DropKernels,
    /// Legacy absolute quiet ceiling, retained only for config compatibility.
    /// Gap starts now depend on spectral trends and relative mix energy loss.
    pub drop_gap_threshold: f32,
    /// Legacy drop flux settings; the arrangement tracker maintains its own state.
    pub drop_onset_pre_gain: f32,
    pub drop_onset_attack: f32,
    pub drop_onset_release: f32,
    pub drop_onset_average: f32,
    pub drop_onset_sensitivity: f32,
    pub drop_onset_min_threshold: f32,
    pub drop_onset_min_interval: f32,
    /// Weight of the inverted-kernel flow subtracted from the drop onset flow.
    pub drop_onset_suppression: f32,
    pub drop_quake_factor: f32,
    pub quake_stiffness: f32,
    pub quake_damping: f32,
}

impl Default for DetectorSettings {
    fn default() -> Self {
        Self {
            percussion: PercussionSettings::default(),
            arrangement: ArrangementSettings::default(),
            bin_gains: PercussionBinGains::default(),
            band_onset: [BandOnsetSettings::default(); BAND_COUNT],
            excess: RelativeExcessSettings::default(),
            kick_low_hz: 20.0,
            kick_high_hz: 302.0,
            kick_pattern: TemporalPattern::kick_initial(),
            kick_onset_pre_gain: 0.2977303,
            kick_onset_attack: 0.004782859,
            kick_onset_release: 0.010084573,
            kick_onset_average: 0.7738249,
            kick_onset_sensitivity: 5.0,
            kick_onset_min_threshold: 0.25,
            kick_onset_min_interval: 0.09,
            kick_onset_suppression: 0.0,
            kick_snare_choke: 0.099999994,
            kick_drop_choke: 0.099999994,
            kick_quake_factor: 26.09,
            kick_sequence_every: 1,
            snare_pattern: TemporalPattern::snare_initial(),
            snare_onset_pre_gain: 1.0054638,
            snare_onset_attack: 0.001,
            snare_onset_release: 0.005,
            snare_onset_average: 0.6,
            snare_onset_sensitivity: 5.0,
            snare_onset_min_threshold: 0.06,
            snare_onset_min_interval: 0.10,
            snare_onset_suppression: 0.099999994,
            snare_min_rms_db: SNARE_RMS_GATE_DEFAULT_DB,
            snare_flip_every: 8,
            snare_poi_switches: 16,
            snare_choke: 0.099999994,
            flip_stiffness: 349.5,
            flip_damping: 18.5,
            hat_pattern: TemporalPattern::hat_initial(),
            hat_onset_pre_gain: 4.0,
            hat_onset_attack: 0.001,
            hat_onset_release: 0.005,
            hat_onset_average: 0.29895896,
            hat_onset_sensitivity: 5.0,
            hat_onset_min_threshold: 0.25,
            hat_onset_min_interval: 0.03,
            hat_onset_suppression: 0.0,
            drop_kernels: DropKernels::default(),
            drop_gap_threshold: -26.0,
            drop_onset_pre_gain: 1.0054638,
            drop_onset_attack: 0.024582442,
            drop_onset_release: 0.05,
            drop_onset_average: 1.2924675,
            drop_onset_sensitivity: 2.0,
            drop_onset_min_threshold: 0.05,
            drop_onset_min_interval: 0.14,
            drop_onset_suppression: 0.5,
            drop_quake_factor: 22.13,
            quake_stiffness: 190.5,
            quake_damping: 4.5,
        }
    }
}

impl DetectorSettings {
    pub fn validate(self) -> anyhow::Result<Self> {
        self.percussion.validate()?;
        self.arrangement.validate()?;
        self.bin_gains.validate()?;
        for (name, frequency) in [
            ("kick_low_hz", self.kick_low_hz),
            ("kick_high_hz", self.kick_high_hz),
        ] {
            anyhow::ensure!(
                frequency.is_finite()
                    && (KICK_FREQUENCY_MIN_HZ..=KICK_FREQUENCY_MAX_HZ).contains(&frequency),
                "{name} must be finite and between {KICK_FREQUENCY_MIN_HZ} and {KICK_FREQUENCY_MAX_HZ} Hz"
            );
        }
        anyhow::ensure!(
            self.kick_high_hz >= self.kick_low_hz + KICK_FREQUENCY_MIN_WIDTH_HZ,
            "kick frequency range must span at least {KICK_FREQUENCY_MIN_WIDTH_HZ} Hz"
        );
        for (band, settings) in self.band_onset.iter().enumerate() {
            anyhow::ensure!(
                settings.abl_gain.is_finite()
                    && (ABL_GAIN_MIN..=ABL_GAIN_MAX).contains(&settings.abl_gain),
                "band {band} ABL gain must be between {ABL_GAIN_MIN} and {ABL_GAIN_MAX}"
            );
            anyhow::ensure!(
                settings.abl_amount.is_finite()
                    && (ABL_AMOUNT_MIN..=Curve::MAX_AMOUNT).contains(&settings.abl_amount),
                "band {band} ABL amount must be between {ABL_AMOUNT_MIN} and {}",
                Curve::MAX_AMOUNT
            );
        }
        for (name, value, minimum, maximum) in [
            (
                "excess background_attack",
                self.excess.background_attack,
                EXCESS_ATTACK_MIN,
                EXCESS_ATTACK_MAX,
            ),
            (
                "excess background_release",
                self.excess.background_release,
                EXCESS_RELEASE_MIN,
                EXCESS_RELEASE_MAX,
            ),
            (
                "excess floor",
                self.excess.floor,
                EXCESS_FLOOR_MIN,
                EXCESS_FLOOR_MAX,
            ),
            (
                "excess cap",
                self.excess.cap,
                EXCESS_CAP_MIN,
                EXCESS_CAP_MAX,
            ),
        ] {
            anyhow::ensure!(
                value.is_finite() && (minimum..=maximum).contains(&value),
                "{name} must be finite and between {minimum} and {maximum}"
            );
        }
        for (name, value, minimum, maximum) in [
            (
                "drop_onset_pre_gain",
                self.drop_onset_pre_gain,
                FLUX_PRE_GAIN_MIN,
                FLUX_PRE_GAIN_MAX,
            ),
            (
                "drop_onset_attack",
                self.drop_onset_attack,
                FLUX_ATTACK_MIN,
                FLUX_ATTACK_MAX,
            ),
            (
                "drop_onset_release",
                self.drop_onset_release,
                FLUX_RELEASE_MIN,
                FLUX_RELEASE_MAX,
            ),
            (
                "drop_onset_average",
                self.drop_onset_average,
                FLUX_AVERAGE_MIN,
                FLUX_AVERAGE_MAX,
            ),
            (
                "drop_onset_sensitivity",
                self.drop_onset_sensitivity,
                FLUX_SENSITIVITY_MIN,
                FLUX_SENSITIVITY_MAX,
            ),
            (
                "drop_onset_min_threshold",
                self.drop_onset_min_threshold,
                FLUX_THRESHOLD_MIN,
                ONSET_THRESHOLD_MAX,
            ),
            (
                "drop_onset_min_interval",
                self.drop_onset_min_interval,
                FLUX_INTERVAL_MIN,
                FLUX_INTERVAL_MAX,
            ),
            (
                "drop_onset_suppression",
                self.drop_onset_suppression,
                FLUX_SUPPRESSION_MIN,
                FLUX_SUPPRESSION_MAX,
            ),
        ] {
            anyhow::ensure!(
                value.is_finite() && (minimum..=maximum).contains(&value),
                "{name} must be finite and between {minimum} and {maximum}"
            );
        }
        for (name, value, minimum, maximum) in [
            (
                "kick_onset_pre_gain",
                self.kick_onset_pre_gain,
                FLUX_PRE_GAIN_MIN,
                FLUX_PRE_GAIN_MAX,
            ),
            (
                "kick_onset_attack",
                self.kick_onset_attack,
                FLUX_ATTACK_MIN,
                FLUX_ATTACK_MAX,
            ),
            (
                "kick_onset_release",
                self.kick_onset_release,
                FLUX_RELEASE_MIN,
                FLUX_RELEASE_MAX,
            ),
            (
                "kick_onset_average",
                self.kick_onset_average,
                FLUX_AVERAGE_MIN,
                FLUX_AVERAGE_MAX,
            ),
            (
                "kick_onset_sensitivity",
                self.kick_onset_sensitivity,
                FLUX_SENSITIVITY_MIN,
                FLUX_SENSITIVITY_MAX,
            ),
            (
                "kick_onset_min_threshold",
                self.kick_onset_min_threshold,
                FLUX_THRESHOLD_MIN,
                ONSET_THRESHOLD_MAX,
            ),
            (
                "kick_onset_min_interval",
                self.kick_onset_min_interval,
                FLUX_INTERVAL_MIN,
                FLUX_INTERVAL_MAX,
            ),
            (
                "kick_onset_suppression",
                self.kick_onset_suppression,
                FLUX_SUPPRESSION_MIN,
                FLUX_SUPPRESSION_MAX,
            ),
            (
                "snare_onset_pre_gain",
                self.snare_onset_pre_gain,
                FLUX_PRE_GAIN_MIN,
                FLUX_PRE_GAIN_MAX,
            ),
            (
                "snare_onset_attack",
                self.snare_onset_attack,
                FLUX_ATTACK_MIN,
                FLUX_ATTACK_MAX,
            ),
            (
                "snare_onset_release",
                self.snare_onset_release,
                FLUX_RELEASE_MIN,
                FLUX_RELEASE_MAX,
            ),
            (
                "snare_onset_average",
                self.snare_onset_average,
                FLUX_AVERAGE_MIN,
                FLUX_AVERAGE_MAX,
            ),
            (
                "snare_onset_sensitivity",
                self.snare_onset_sensitivity,
                FLUX_SENSITIVITY_MIN,
                FLUX_SENSITIVITY_MAX,
            ),
            (
                "snare_onset_min_threshold",
                self.snare_onset_min_threshold,
                FLUX_THRESHOLD_MIN,
                ONSET_THRESHOLD_MAX,
            ),
            (
                "snare_onset_min_interval",
                self.snare_onset_min_interval,
                FLUX_INTERVAL_MIN,
                FLUX_INTERVAL_MAX,
            ),
            (
                "snare_onset_suppression",
                self.snare_onset_suppression,
                FLUX_SUPPRESSION_MIN,
                FLUX_SUPPRESSION_MAX,
            ),
            (
                "snare_min_rms_db",
                self.snare_min_rms_db,
                SNARE_RMS_GATE_MIN_DB,
                SNARE_RMS_GATE_MAX_DB,
            ),
            (
                "hat_onset_pre_gain",
                self.hat_onset_pre_gain,
                FLUX_PRE_GAIN_MIN,
                FLUX_PRE_GAIN_MAX,
            ),
            (
                "hat_onset_attack",
                self.hat_onset_attack,
                FLUX_ATTACK_MIN,
                FLUX_ATTACK_MAX,
            ),
            (
                "hat_onset_release",
                self.hat_onset_release,
                FLUX_RELEASE_MIN,
                FLUX_RELEASE_MAX,
            ),
            (
                "hat_onset_average",
                self.hat_onset_average,
                FLUX_AVERAGE_MIN,
                FLUX_AVERAGE_MAX,
            ),
            (
                "hat_onset_sensitivity",
                self.hat_onset_sensitivity,
                FLUX_SENSITIVITY_MIN,
                FLUX_SENSITIVITY_MAX,
            ),
            (
                "hat_onset_min_threshold",
                self.hat_onset_min_threshold,
                FLUX_THRESHOLD_MIN,
                ONSET_THRESHOLD_MAX,
            ),
            (
                "hat_onset_min_interval",
                self.hat_onset_min_interval,
                FLUX_INTERVAL_MIN,
                FLUX_INTERVAL_MAX,
            ),
            (
                "hat_onset_suppression",
                self.hat_onset_suppression,
                FLUX_SUPPRESSION_MIN,
                FLUX_SUPPRESSION_MAX,
            ),
        ] {
            anyhow::ensure!(
                value.is_finite() && (minimum..=maximum).contains(&value),
                "{name} must be finite and between {minimum} and {maximum}"
            );
        }
        self.kick_pattern.validate()?;
        self.snare_pattern.validate()?;
        self.hat_pattern.validate()?;
        for (name, choke) in [
            ("kick_snare_choke", self.kick_snare_choke),
            ("kick_drop_choke", self.kick_drop_choke),
            ("snare_choke", self.snare_choke),
        ] {
            anyhow::ensure!(
                choke.is_finite() && (CHOKE_MIN..=CHOKE_MAX).contains(&choke),
                "{name} must be finite and between {CHOKE_MIN} and {CHOKE_MAX}"
            );
        }
        for (name, factor) in [
            ("kick_quake_factor", self.kick_quake_factor),
            ("drop_quake_factor", self.drop_quake_factor),
        ] {
            anyhow::ensure!(
                factor.is_finite() && (QUAKE_FACTOR_MIN..=QUAKE_FACTOR_MAX).contains(&factor),
                "{name} must be finite and between {QUAKE_FACTOR_MIN} and {QUAKE_FACTOR_MAX}"
            );
        }
        anyhow::ensure!(
            (SEQUENCE_EVERY_MIN..=SEQUENCE_EVERY_MAX).contains(&self.kick_sequence_every),
            "the kick quake sequence length must be between {SEQUENCE_EVERY_MIN} and {SEQUENCE_EVERY_MAX}"
        );
        anyhow::ensure!(
            (SEQUENCE_EVERY_MIN..=SEQUENCE_EVERY_MAX).contains(&self.snare_flip_every),
            "the snare flip count must be between {SEQUENCE_EVERY_MIN} and {SEQUENCE_EVERY_MAX}"
        );
        anyhow::ensure!(
            (SEQUENCE_EVERY_MIN..=SEQUENCE_EVERY_MAX).contains(&self.snare_poi_switches),
            "the POI switch count must be between {SEQUENCE_EVERY_MIN} and {SEQUENCE_EVERY_MAX}"
        );
        self.drop_kernels.validate()?;
        anyhow::ensure!(
            self.drop_gap_threshold.is_finite()
                && (DROP_GAP_THRESHOLD_MIN..=DROP_GAP_THRESHOLD_MAX)
                    .contains(&self.drop_gap_threshold),
            "the drop gap threshold must be finite and between \
             {DROP_GAP_THRESHOLD_MIN} and {DROP_GAP_THRESHOLD_MAX} dBFS"
        );
        for (name, stiffness) in [
            ("flip_stiffness", self.flip_stiffness),
            ("quake_stiffness", self.quake_stiffness),
        ] {
            anyhow::ensure!(
                stiffness.is_finite()
                    && (SPRING_STIFFNESS_MIN..=SPRING_STIFFNESS_MAX).contains(&stiffness),
                "{name} must be finite and between {SPRING_STIFFNESS_MIN} and {SPRING_STIFFNESS_MAX}"
            );
        }
        for (name, damping) in [
            ("flip_damping", self.flip_damping),
            ("quake_damping", self.quake_damping),
        ] {
            anyhow::ensure!(
                damping.is_finite() && (SPRING_DAMPING_MIN..=SPRING_DAMPING_MAX).contains(&damping),
                "{name} must be finite and between {SPRING_DAMPING_MIN} and {SPRING_DAMPING_MAX}"
            );
        }
        Ok(self)
    }

    pub fn kick_contains(&self, band: usize) -> bool {
        if band >= BAND_COUNT {
            return false;
        }
        // Routing diagnostics use nominal display bands. The actual detector
        // integrates fractional raw-bin overlap with these configured bounds.
        let low = 32.0 * 500.0_f32.powf(band as f32 / BAND_COUNT as f32);
        let high = 32.0 * 500.0_f32.powf((band + 1) as f32 / BAND_COUNT as f32);
        high.min(self.kick_high_hz) - low.max(self.kick_low_hz) > 0.001
    }

    pub fn snare_contains(&self, band: usize) -> bool {
        (6..BAND_COUNT).contains(&band)
    }

    pub fn hat_contains(&self, band: usize) -> bool {
        (12..BAND_COUNT).contains(&band)
    }

    pub fn drop_kernel_weights(&self, kernel: DropKernel) -> [f32; BAND_COUNT] {
        self.drop_kernels.weights(kernel)
    }

    pub fn set_drop_kernel_band(&mut self, kernel: DropKernel, band: usize, value: f32) {
        assert!(band < BAND_COUNT, "kernel bands stay inside the spectrum");
        self.drop_kernels.set(kernel, band, value);
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BandFluxSettingsRaw {
    attack: f32,
    release: f32,
    average: f32,
    sensitivity: f32,
    min_threshold: f32,
    min_interval: f32,
    #[serde(default = "default_flux_gain")]
    gain: f32,
}

fn default_flux_gain() -> f32 {
    1.0
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OptionalKernel {
    Bare([f32; BAND_COUNT]),
    Wrapped(Option<[f32; BAND_COUNT]>),
}

fn deserialize_optional_kernel<'de, D>(
    deserializer: D,
) -> Result<Option<[f32; BAND_COUNT]>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match OptionalKernel::deserialize(deserializer)? {
        OptionalKernel::Bare(kernel) => Some(kernel),
        OptionalKernel::Wrapped(kernel) => kernel,
    })
}

fn deserialize_optional_f32<'de, D>(deserializer: D) -> Result<Option<f32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    f32::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TemporalPatternRaw {
    inner: PatternPolygon,
    outer: PatternPolygon,
    #[serde(default)]
    suppression_inner: PatternPolygon,
    #[serde(default)]
    suppression_outer: PatternPolygon,
    #[serde(skip_serializing)]
    steps: Vec<[f32; BAND_COUNT]>,
    #[serde(default = "default_pattern_area")]
    effective: f32,
}

impl Default for TemporalPatternRaw {
    fn default() -> Self {
        Self {
            inner: Vec::new(),
            outer: Vec::new(),
            suppression_inner: Vec::new(),
            suppression_outer: Vec::new(),
            steps: Vec::new(),
            effective: 1.0,
        }
    }
}

impl TemporalPatternRaw {
    fn validate_legacy(&self) -> anyhow::Result<()> {
        for (name, polygon) in [
            ("inner", &self.inner),
            ("outer", &self.outer),
            ("suppression inner", &self.suppression_inner),
            ("suppression outer", &self.suppression_outer),
        ] {
            if !polygon.is_empty() {
                validate_polygon(name, polygon)?;
            }
        }
        anyhow::ensure!(
            self.effective.is_finite() && (0.1..=1.0).contains(&self.effective),
            "legacy pattern area must be finite in 0.1..=1"
        );
        anyhow::ensure!(
            self.steps.len() <= PATTERN_MAX_STEPS,
            "legacy pattern exceeds bounded history"
        );
        for step in &self.steps {
            validate_kernel("legacy pattern", step)?;
        }
        Ok(())
    }

    fn into_pattern(self) -> TemporalPattern {
        if self.inner.len() >= 3 && self.outer.len() >= 3 {
            TemporalPattern {
                inner: self.inner,
                outer: self.outer,
                suppression_inner: self.suppression_inner,
                suppression_outer: self.suppression_outer,
                effective: self.effective,
            }
        } else if !self.steps.is_empty() {
            TemporalPattern::from_legacy_steps(&self.steps)
        } else {
            TemporalPattern::kick_initial()
        }
    }
}

impl From<TemporalPattern> for TemporalPatternRaw {
    fn from(pattern: TemporalPattern) -> Self {
        Self {
            inner: pattern.inner,
            outer: pattern.outer,
            suppression_inner: pattern.suppression_inner,
            suppression_outer: pattern.suppression_outer,
            steps: Vec::new(),
            effective: pattern.effective,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DetectorSettingsRaw {
    percussion: PercussionSettings,
    arrangement: ArrangementSettings,
    bin_gains: PercussionBinGains,
    /// New score units require new thresholds when reading temporal recipes.
    #[serde(default)]
    onset_revision: u32,
    /// New per-band ABL settings. Missing means all bands use defaults.
    band_onset: Vec<BandOnsetSettings>,
    excess: RelativeExcessSettings,
    kick_low_hz: f32,
    kick_high_hz: f32,
    /// Settings from the previous per-band flux tracker; accepted but unused.
    #[serde(rename = "band_flux", skip_serializing)]
    legacy_band_flux: Vec<BandFluxSettingsRaw>,
    #[serde(rename = "flux_attack", skip_serializing)]
    legacy_flux_attack: f32,
    #[serde(rename = "flux_release", skip_serializing)]
    legacy_flux_release: f32,
    #[serde(rename = "flux_average", skip_serializing)]
    legacy_flux_average: f32,
    #[serde(rename = "flux_sensitivity", skip_serializing)]
    legacy_flux_sensitivity: f32,
    #[serde(rename = "flux_min_threshold", skip_serializing)]
    legacy_flux_min_threshold: f32,
    #[serde(rename = "flux_min_interval", skip_serializing)]
    legacy_flux_min_interval: f32,
    kick_pattern: Option<TemporalPatternRaw>,
    #[serde(
        rename = "kick_kernel",
        skip_serializing,
        default,
        deserialize_with = "deserialize_optional_kernel"
    )]
    legacy_kick_kernel: Option<[f32; BAND_COUNT]>,
    kick_onset_pre_gain: f32,
    kick_onset_attack: f32,
    kick_onset_release: f32,
    kick_onset_average: f32,
    kick_onset_sensitivity: f32,
    kick_onset_min_threshold: f32,
    kick_onset_min_interval: f32,
    kick_onset_suppression: f32,
    kick_snare_choke: f32,
    kick_drop_choke: f32,
    kick_quake_factor: f32,
    kick_sequence_every: u32,
    snare_pattern: Option<TemporalPatternRaw>,
    #[serde(
        rename = "snare_kernel",
        skip_serializing,
        default,
        deserialize_with = "deserialize_optional_kernel"
    )]
    legacy_snare_kernel: Option<[f32; BAND_COUNT]>,
    snare_onset_pre_gain: f32,
    snare_onset_attack: f32,
    snare_onset_release: f32,
    snare_onset_average: f32,
    snare_onset_sensitivity: f32,
    snare_onset_min_threshold: f32,
    snare_onset_min_interval: f32,
    snare_onset_suppression: f32,
    snare_min_rms_db: f32,
    snare_flip_every: u32,
    snare_poi_switches: u32,
    snare_choke: f32,
    flip_stiffness: f32,
    flip_damping: f32,
    hat_pattern: Option<TemporalPatternRaw>,
    hat_onset_pre_gain: f32,
    hat_onset_attack: f32,
    hat_onset_release: f32,
    hat_onset_average: f32,
    hat_onset_sensitivity: f32,
    hat_onset_min_threshold: f32,
    hat_onset_min_interval: f32,
    hat_onset_suppression: f32,
    drop_gap_threshold: f32,
    drop_onset_pre_gain: f32,
    drop_onset_attack: f32,
    drop_onset_release: f32,
    drop_onset_average: f32,
    drop_onset_sensitivity: f32,
    drop_onset_min_threshold: f32,
    drop_onset_min_interval: f32,
    drop_onset_suppression: f32,
    drop_onset_kernel: [f32; BAND_COUNT],
    drop_quake_factor: f32,
    quake_stiffness: f32,
    quake_damping: f32,
    #[serde(rename = "drop_split", skip_serializing)]
    legacy_drop_split: usize,
    #[serde(rename = "drop_onsets", skip_serializing)]
    legacy_drop_onsets: usize,
    #[serde(rename = "drop_window", skip_serializing)]
    legacy_drop_window: f32,
    #[serde(
        rename = "drop_gap_window",
        skip_serializing,
        default,
        deserialize_with = "deserialize_optional_f32"
    )]
    legacy_drop_gap_window: Option<f32>,
    #[serde(rename = "drop_gap_gain", skip_serializing)]
    legacy_drop_gap_gain: f32,
    #[serde(rename = "drop_low_lp", skip_serializing)]
    legacy_drop_low_lp: f32,
    #[serde(rename = "drop_high_lp", skip_serializing)]
    legacy_drop_high_lp: f32,
    #[serde(rename = "drop_low_kernel", skip_serializing)]
    legacy_drop_low_kernel: [f32; BAND_COUNT],
    #[serde(rename = "drop_high_kernel", skip_serializing)]
    legacy_drop_high_kernel: [f32; BAND_COUNT],
    #[serde(rename = "drop_gap_level", skip_serializing)]
    legacy_drop_gap_level: f32,
    #[serde(rename = "drop_gap_time", skip_serializing)]
    legacy_drop_gap_time: f32,
    #[serde(rename = "drop_quiet_level", skip_serializing)]
    legacy_drop_quiet_level: f32,
    #[serde(rename = "drop_quiet_time", skip_serializing)]
    legacy_drop_quiet_time: f32,
    #[serde(rename = "kick_low", skip_serializing)]
    legacy_kick_low: usize,
    #[serde(rename = "kick_high", skip_serializing)]
    legacy_kick_high: usize,
    #[serde(rename = "kick_window", skip_serializing)]
    legacy_kick_window: f32,
    #[serde(rename = "snare_bands", skip_serializing)]
    legacy_snare_bands: [Option<usize>; 8],
    #[serde(rename = "snare_window", skip_serializing)]
    legacy_snare_window: f32,
    #[serde(rename = "snare_min_onsets", skip_serializing)]
    legacy_snare_min_onsets: usize,
    #[serde(rename = "snare_min_power", skip_serializing)]
    legacy_snare_min_power: f32,
}

impl Default for DetectorSettingsRaw {
    fn default() -> Self {
        let mut raw: Self = DetectorSettings::default().into();
        raw.band_onset.clear();
        raw.legacy_band_flux.clear();
        raw.kick_pattern = None;
        raw.snare_pattern = None;
        raw.hat_pattern = None;
        raw.legacy_kick_kernel = None;
        raw.legacy_snare_kernel = None;
        raw
    }
}

impl TryFrom<DetectorSettingsRaw> for DetectorSettings {
    type Error = anyhow::Error;

    fn try_from(raw: DetectorSettingsRaw) -> Result<Self, Self::Error> {
        anyhow::ensure!(
            raw.onset_revision <= 3,
            "unsupported onset_revision {}",
            raw.onset_revision
        );
        for pattern in [&raw.kick_pattern, &raw.snare_pattern, &raw.hat_pattern]
            .into_iter()
            .flatten()
        {
            pattern.validate_legacy()?;
        }
        for kernel in [&raw.legacy_kick_kernel, &raw.legacy_snare_kernel]
            .into_iter()
            .flatten()
        {
            validate_kernel("legacy", kernel)?;
        }
        for settings in &raw.legacy_band_flux {
            for value in [
                settings.attack,
                settings.release,
                settings.average,
                settings.sensitivity,
                settings.min_threshold,
                settings.min_interval,
                settings.gain,
            ] {
                anyhow::ensure!(
                    value.is_finite() && value >= 0.0,
                    "legacy band flux values must be finite and nonnegative"
                );
            }
        }
        for value in [
            raw.legacy_flux_attack,
            raw.legacy_flux_release,
            raw.legacy_flux_average,
            raw.legacy_flux_sensitivity,
            raw.legacy_flux_min_threshold,
            raw.legacy_flux_min_interval,
        ] {
            anyhow::ensure!(
                value.is_finite() && value >= 0.0,
                "legacy flux values must be finite and nonnegative"
            );
        }
        let histogram_migration = raw.onset_revision < 3;
        // Accepted compatibility fields must be checked even when their old
        // algorithm is no longer active, and before any migration overwrites them.
        for (name, value) in [
            ("drop_window", raw.legacy_drop_window),
            ("drop_gap_window", raw.legacy_drop_gap_window.unwrap_or(0.0)),
            ("drop_gap_gain", raw.legacy_drop_gap_gain),
            ("drop_low_lp", raw.legacy_drop_low_lp),
            ("drop_high_lp", raw.legacy_drop_high_lp),
            ("drop_gap_time", raw.legacy_drop_gap_time),
            ("drop_quiet_time", raw.legacy_drop_quiet_time),
            ("kick_window", raw.legacy_kick_window),
            ("snare_window", raw.legacy_snare_window),
            ("snare_min_power", raw.legacy_snare_min_power),
        ] {
            anyhow::ensure!(
                value.is_finite() && value >= 0.0,
                "legacy {name} must be finite and nonnegative"
            );
        }
        for value in [raw.legacy_drop_gap_level, raw.legacy_drop_quiet_level] {
            anyhow::ensure!(value.is_finite(), "legacy drop levels must be finite");
        }
        anyhow::ensure!(
            raw.drop_gap_threshold.is_finite()
                && (DROP_GAP_THRESHOLD_MIN..=DROP_GAP_THRESHOLD_MAX)
                    .contains(&raw.drop_gap_threshold),
            "drop_gap_threshold must be finite and in its saved range"
        );
        validate_kernel("legacy drop low", &raw.legacy_drop_low_kernel)?;
        validate_kernel("legacy drop high", &raw.legacy_drop_high_kernel)?;
        anyhow::ensure!(
            raw.legacy_drop_split <= BAND_COUNT
                && raw.legacy_kick_low <= BAND_COUNT
                && raw.legacy_kick_high <= BAND_COUNT,
            "legacy band boundaries exceed spectrum"
        );
        anyhow::ensure!(
            raw.legacy_snare_bands
                .iter()
                .flatten()
                .all(|band| *band < BAND_COUNT),
            "legacy snare band exceeds spectrum"
        );
        let needs_migration = raw.onset_revision < 2;
        let band_onset: [BandOnsetSettings; BAND_COUNT] = if raw.band_onset.is_empty() {
            [BandOnsetSettings::default(); BAND_COUNT]
        } else {
            anyhow::ensure!(
                raw.band_onset.len() == BAND_COUNT,
                "band_onset must describe all {BAND_COUNT} bands"
            );
            raw.band_onset
                .try_into()
                .expect("the band count was checked above")
        };
        let kick_pattern = if let Some(pattern) = raw.kick_pattern {
            pattern.into_pattern()
        } else if let Some(kernel) = raw.legacy_kick_kernel {
            validate_kernel("kick", &kernel)?;
            TemporalPattern::from_legacy_steps(&[kernel])
        } else {
            TemporalPattern::kick_initial()
        };
        let snare_pattern = if let Some(pattern) = raw.snare_pattern {
            pattern.into_pattern()
        } else if let Some(kernel) = raw.legacy_snare_kernel {
            validate_kernel("snare", &kernel)?;
            TemporalPattern::from_legacy_steps(&[kernel])
        } else {
            TemporalPattern::snare_initial()
        };
        let mut kick_pattern = kick_pattern;
        if kick_pattern.suppression_inner.len() < 3 || kick_pattern.suppression_outer.len() < 3 {
            let initial = TemporalPattern::kick_initial();
            kick_pattern.suppression_inner = initial.suppression_inner;
            kick_pattern.suppression_outer = initial.suppression_outer;
        }
        let mut snare_pattern = snare_pattern;
        if snare_pattern.suppression_inner.len() < 3 || snare_pattern.suppression_outer.len() < 3 {
            let initial = TemporalPattern::snare_initial();
            snare_pattern.suppression_inner = initial.suppression_inner;
            snare_pattern.suppression_outer = initial.suppression_outer;
        }
        let mut hat_pattern = raw.hat_pattern.map_or_else(
            TemporalPattern::hat_initial,
            TemporalPatternRaw::into_pattern,
        );
        if hat_pattern.suppression_inner.len() < 3 || hat_pattern.suppression_outer.len() < 3 {
            let initial = TemporalPattern::hat_initial();
            hat_pattern.suppression_inner = initial.suppression_inner;
            hat_pattern.suppression_outer = initial.suppression_outer;
        }
        let drop_gap_threshold = if raw.legacy_drop_gap_window.is_some() {
            DetectorSettings::default().drop_gap_threshold
        } else {
            raw.drop_gap_threshold
        };
        Self {
            percussion: raw.percussion,
            arrangement: raw.arrangement,
            bin_gains: raw.bin_gains,
            band_onset,
            excess: raw.excess,
            kick_low_hz: raw.kick_low_hz,
            kick_high_hz: raw.kick_high_hz,
            kick_pattern,
            kick_onset_pre_gain: raw.kick_onset_pre_gain,
            kick_onset_attack: raw.kick_onset_attack,
            kick_onset_release: raw.kick_onset_release,
            kick_onset_average: raw.kick_onset_average,
            kick_onset_sensitivity: raw.kick_onset_sensitivity,
            kick_onset_min_threshold: raw.kick_onset_min_threshold,
            kick_onset_min_interval: raw.kick_onset_min_interval,
            kick_onset_suppression: raw.kick_onset_suppression,
            kick_snare_choke: raw.kick_snare_choke,
            kick_drop_choke: raw.kick_drop_choke,
            kick_quake_factor: raw.kick_quake_factor,
            kick_sequence_every: raw.kick_sequence_every,
            snare_pattern,
            snare_onset_pre_gain: raw.snare_onset_pre_gain,
            snare_onset_attack: raw.snare_onset_attack,
            snare_onset_release: raw.snare_onset_release,
            snare_onset_average: raw.snare_onset_average,
            snare_onset_sensitivity: raw.snare_onset_sensitivity,
            snare_onset_min_threshold: raw.snare_onset_min_threshold,
            snare_onset_min_interval: raw.snare_onset_min_interval,
            snare_onset_suppression: raw.snare_onset_suppression,
            snare_min_rms_db: raw.snare_min_rms_db,
            snare_flip_every: raw.snare_flip_every,
            snare_poi_switches: raw.snare_poi_switches,
            snare_choke: raw.snare_choke,
            flip_stiffness: raw.flip_stiffness,
            flip_damping: raw.flip_damping,
            hat_pattern,
            hat_onset_pre_gain: raw.hat_onset_pre_gain,
            hat_onset_attack: raw.hat_onset_attack,
            hat_onset_release: raw.hat_onset_release,
            hat_onset_average: raw.hat_onset_average,
            hat_onset_sensitivity: raw.hat_onset_sensitivity,
            hat_onset_min_threshold: raw.hat_onset_min_threshold,
            hat_onset_min_interval: raw.hat_onset_min_interval,
            hat_onset_suppression: raw.hat_onset_suppression,
            drop_kernels: DropKernels {
                onset: raw.drop_onset_kernel,
            },
            drop_gap_threshold,
            drop_onset_pre_gain: raw.drop_onset_pre_gain,
            drop_onset_attack: raw.drop_onset_attack,
            drop_onset_release: raw.drop_onset_release,
            drop_onset_average: raw.drop_onset_average,
            drop_onset_sensitivity: raw.drop_onset_sensitivity,
            drop_onset_min_threshold: raw.drop_onset_min_threshold,
            drop_onset_min_interval: raw.drop_onset_min_interval,
            drop_onset_suppression: raw.drop_onset_suppression,
            drop_quake_factor: raw.drop_quake_factor,
            quake_stiffness: raw.quake_stiffness,
            quake_damping: raw.quake_damping,
        }
        .validate()
        .map(|mut settings| {
            if histogram_migration {
                // Validate the complete old recipe above before discarding its
                // incompatible regional gain curves and frequency masks.
                settings.percussion = PercussionSettings::default();
                settings.bin_gains = PercussionBinGains::default();
                log::info!("Percussion revision 3: restored factory frequency masks and input gain curves; arrangement and motion preferences retained");
            }
            if needs_migration {
                // Validate the original recipe before migrating score units:
                // corrupt settings must never be silently repaired and saved.
                let defaults = Self::default();
                settings.kick_onset_average = defaults.kick_onset_average;
                settings.kick_onset_sensitivity = defaults.kick_onset_sensitivity;
                settings.kick_onset_min_threshold = defaults.kick_onset_min_threshold;
                settings.kick_onset_min_interval = defaults.kick_onset_min_interval;
                settings.snare_onset_average = defaults.snare_onset_average;
                settings.snare_onset_sensitivity = defaults.snare_onset_sensitivity;
                settings.snare_onset_min_threshold = defaults.snare_onset_min_threshold;
                settings.snare_onset_min_interval = defaults.snare_onset_min_interval;
                settings.hat_onset_average = defaults.hat_onset_average;
                settings.hat_onset_sensitivity = defaults.hat_onset_sensitivity;
                settings.hat_onset_min_threshold = defaults.hat_onset_min_threshold;
                settings.hat_onset_min_interval = defaults.hat_onset_min_interval;
            }
            settings
        })
    }
}

impl From<DetectorSettings> for DetectorSettingsRaw {
    fn from(settings: DetectorSettings) -> Self {
        Self {
            percussion: settings.percussion,
            arrangement: settings.arrangement,
            bin_gains: settings.bin_gains,
            onset_revision: 3,
            band_onset: settings.band_onset.to_vec(),
            excess: settings.excess,
            kick_low_hz: settings.kick_low_hz,
            kick_high_hz: settings.kick_high_hz,
            legacy_band_flux: Vec::new(),
            legacy_flux_attack: 0.0,
            legacy_flux_release: 0.0,
            legacy_flux_average: 0.0,
            legacy_flux_sensitivity: 0.0,
            legacy_flux_min_threshold: 0.0,
            legacy_flux_min_interval: 0.0,
            kick_pattern: Some(settings.kick_pattern.clone().into()),
            legacy_kick_kernel: Some(std::array::from_fn(|band| if band < 5 { 1.0 } else { 0.0 })),
            kick_onset_pre_gain: settings.kick_onset_pre_gain,
            kick_onset_attack: settings.kick_onset_attack,
            kick_onset_release: settings.kick_onset_release,
            kick_onset_average: settings.kick_onset_average,
            kick_onset_sensitivity: settings.kick_onset_sensitivity,
            kick_onset_min_threshold: settings.kick_onset_min_threshold,
            kick_onset_min_interval: settings.kick_onset_min_interval,
            kick_onset_suppression: settings.kick_onset_suppression,
            kick_snare_choke: settings.kick_snare_choke,
            kick_drop_choke: settings.kick_drop_choke,
            kick_quake_factor: settings.kick_quake_factor,
            kick_sequence_every: settings.kick_sequence_every,
            snare_pattern: Some(settings.snare_pattern.clone().into()),
            legacy_snare_kernel: Some(std::array::from_fn(|band| {
                if matches!(band, 5 | 6 | 10 | 12) {
                    1.0
                } else {
                    0.0
                }
            })),
            snare_onset_pre_gain: settings.snare_onset_pre_gain,
            snare_onset_attack: settings.snare_onset_attack,
            snare_onset_release: settings.snare_onset_release,
            snare_onset_average: settings.snare_onset_average,
            snare_onset_sensitivity: settings.snare_onset_sensitivity,
            snare_onset_min_threshold: settings.snare_onset_min_threshold,
            snare_onset_min_interval: settings.snare_onset_min_interval,
            snare_onset_suppression: settings.snare_onset_suppression,
            snare_min_rms_db: settings.snare_min_rms_db,
            snare_flip_every: settings.snare_flip_every,
            snare_poi_switches: settings.snare_poi_switches,
            snare_choke: settings.snare_choke,
            flip_stiffness: settings.flip_stiffness,
            flip_damping: settings.flip_damping,
            hat_pattern: Some(settings.hat_pattern.clone().into()),
            hat_onset_pre_gain: settings.hat_onset_pre_gain,
            hat_onset_attack: settings.hat_onset_attack,
            hat_onset_release: settings.hat_onset_release,
            hat_onset_average: settings.hat_onset_average,
            hat_onset_sensitivity: settings.hat_onset_sensitivity,
            hat_onset_min_threshold: settings.hat_onset_min_threshold,
            hat_onset_min_interval: settings.hat_onset_min_interval,
            hat_onset_suppression: settings.hat_onset_suppression,
            drop_gap_threshold: settings.drop_gap_threshold,
            drop_onset_pre_gain: settings.drop_onset_pre_gain,
            drop_onset_attack: settings.drop_onset_attack,
            drop_onset_release: settings.drop_onset_release,
            drop_onset_average: settings.drop_onset_average,
            drop_onset_sensitivity: settings.drop_onset_sensitivity,
            drop_onset_min_threshold: settings.drop_onset_min_threshold,
            drop_onset_min_interval: settings.drop_onset_min_interval,
            drop_onset_suppression: settings.drop_onset_suppression,
            drop_onset_kernel: settings.drop_kernels.onset,
            drop_quake_factor: settings.drop_quake_factor,
            quake_stiffness: settings.quake_stiffness,
            quake_damping: settings.quake_damping,
            legacy_drop_split: 0,
            legacy_drop_onsets: 0,
            legacy_drop_window: 0.0,
            legacy_drop_gap_window: None,
            legacy_drop_gap_gain: 0.0,
            legacy_drop_low_lp: 0.0,
            legacy_drop_high_lp: 0.0,
            legacy_drop_low_kernel: [0.0; BAND_COUNT],
            legacy_drop_high_kernel: [0.0; BAND_COUNT],
            legacy_drop_gap_level: 0.0,
            legacy_drop_gap_time: 0.0,
            legacy_drop_quiet_level: 0.0,
            legacy_drop_quiet_time: 0.0,
            legacy_kick_low: 0,
            legacy_kick_high: 0,
            legacy_kick_window: 0.0,
            legacy_snare_bands: [None; 8],
            legacy_snare_window: 0.0,
            legacy_snare_min_onsets: 0,
            legacy_snare_min_power: 0.0,
        }
    }
}

/// Onsets produced by one analysis hop. Only the composite detectors report
/// boolean onsets; the per-band signal is a continuous flow.
#[derive(Clone, Copy, Debug, Default)]
pub struct DetectorEvents {
    pub kick: Option<f32>,
    pub snare: Option<f32>,
    pub hat: Option<f32>,
    pub drop: Option<f32>,
    /// The gap gate crossed from unarmed to armed on this hop.
    pub gap_entry: bool,
}

/// Legacy envelope recipe retained in the public configuration API.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluxSettings {
    pub attack: f32,
    pub release: f32,
    /// Reference memory in seconds: the time constant of the flux-level
    /// average and the window of the peak median.
    pub average: f32,
    /// Threshold as a fraction of the reference.
    pub sensitivity: f32,
    /// Absolute threshold floor. The peak median also admits only peaks above
    /// this value.
    pub min_threshold: f32,
    pub min_interval: f32,
}

impl Default for FluxSettings {
    fn default() -> Self {
        Self {
            attack: 0.005,
            release: 0.05,
            average: 0.2,
            sensitivity: 1.5,
            min_threshold: 0.01,
            min_interval: 0.14,
        }
    }
}

/// One composite projection: the inverted-kernel flow, the part of it the
/// suppression weight subtracts, and the residual that reaches the tracker.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
struct OnsetProjection {
    /// Inverted-kernel weighted onset flow after pre-gain, before suppression.
    inhibition: f32,
    /// Inhibition after the detector's suppression weight.
    suppressed: f32,
    /// Excitation minus the weighted inhibition, clamped at zero.
    residual: f32,
}

/// All per-band trackers and the four confirmed detectors. One `observe` call
/// advances the state by one analysis hop.
pub struct OnsetBank {
    band_excess: [f32; BAND_COUNT],
    band_abdr: [f32; BAND_COUNT],
    band_abl: [f32; BAND_COUNT],
    band_drp: [f32; BAND_COUNT],
    band_onset: [f32; BAND_COUNT],
    /// Recent per-band flow retained for the analyzer's history diagnostics.
    onset_history: VecDeque<[f32; BAND_COUNT]>,
    sharp: SharpOnsets,
    drop: DropTracker,
    time: f64,
}

impl Default for OnsetBank {
    fn default() -> Self {
        Self::new()
    }
}

impl OnsetBank {
    pub fn new() -> Self {
        Self {
            band_excess: [0.0; BAND_COUNT],
            band_abdr: [0.0; BAND_COUNT],
            band_abl: [0.0; BAND_COUNT],
            band_drp: [0.0; BAND_COUNT],
            band_onset: [0.0; BAND_COUNT],
            onset_history: VecDeque::with_capacity(PATTERN_MAX_STEPS),
            sharp: SharpOnsets::default(),
            drop: DropTracker::default(),
            time: 0.0,
        }
    }

    /// Feed one causal analysis hop to the percussion and arrangement detectors.
    pub fn observe(
        &mut self,
        spectrum: &Spectrum,
        band_abdr: &[f32; BAND_COUNT],
        band_abl: &[f32; BAND_COUNT],
        settings: &DetectorSettings,
        dt: f32,
    ) -> DetectorEvents {
        self.time += f64::from(dt);
        let wsdr = band_abdr.iter().sum::<f32>() / BAND_COUNT as f32;
        for band in 0..BAND_COUNT {
            let drp = if wsdr > 1e-6 {
                band_abdr[band] / wsdr
            } else {
                0.0
            };
            self.band_excess[band] = spectrum.band_excess[band];
            self.band_abdr[band] = band_abdr[band];
            self.band_abl[band] = band_abl[band];
            self.band_drp[band] = drp;
            self.band_onset[band] = spectrum.band_excess[band] * drp * band_abl[band];
        }
        if self.onset_history.len() == PATTERN_MAX_STEPS {
            self.onset_history.pop_front();
        }
        self.onset_history.push_back(self.band_onset);
        DetectorEvents {
            drop: self.drop.observe(spectrum, self.time, dt, settings),
            gap_entry: self.drop.gap_entry,
            ..DetectorEvents::default()
        }
    }

    /// Independent fast branch; arrangement history advances only in `observe`.
    pub fn observe_percussion(
        &mut self,
        frame: &PercussionFrame,
        settings: &DetectorSettings,
    ) -> DetectorEvents {
        self.sharp
            .observe_percussion(frame, frame.at, &settings.percussion)
    }

    pub fn percussion_decisions(&self) -> &VecDeque<super::percussion::PercussionDecision> {
        self.sharp.decisions()
    }

    pub fn percussion_reference(
        &self,
        channel: usize,
    ) -> super::percussion::PercussionReferenceSnapshot {
        self.sharp.reference(channel)
    }

    pub fn reset_percussion(&mut self) {
        self.sharp.reset_features();
    }

    pub fn percussion_channel_status(
        &self,
        channel: usize,
    ) -> super::percussion::PercussionChannelStatus {
        self.sharp.channel_status(channel)
    }

    pub fn reset(&mut self) {
        self.band_excess = [0.0; BAND_COUNT];
        self.band_abdr = [0.0; BAND_COUNT];
        self.band_abl = [0.0; BAND_COUNT];
        self.band_drp = [0.0; BAND_COUNT];
        self.band_onset = [0.0; BAND_COUNT];
        self.onset_history.clear();
        self.sharp = SharpOnsets::default();
        self.drop.reset();
        self.time = 0.0;
    }

    pub fn onset_history(&self) -> &VecDeque<[f32; BAND_COUNT]> {
        &self.onset_history
    }

    pub fn status(&self) -> DetectionStatus {
        DetectionStatus {
            bands: std::array::from_fn(|index| BandStatus {
                excess: self.band_excess[index],
                onset: self.band_onset[index],
                abdr: self.band_abdr[index],
                abl: self.band_abl[index],
                drp: self.band_drp[index],
            }),
            kick: self.sharp.triggers()[0],
            snare: self.sharp.triggers()[1],
            hat: self.sharp.triggers()[2],
            drop: TriggerStatus {
                flash: self.drop.flash,
                power: self.drop.last_power,
                count: 0,
                armed: self.drop.eligible,
                choked: false,
            },
            kick_flux: self.sharp.fluxes()[0],
            snare_flux: self.sharp.fluxes()[1],
            hat_flux: self.sharp.fluxes()[2],
            drop_status: self.drop.status,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BandStatus {
    /// Raw relative spectral excess for this band.
    pub excess: f32,
    /// Continuous onset flow after DRP and ABL scaling.
    pub onset: f32,
    /// Tracked high-minus-low reference distance.
    pub abdr: f32,
    /// Absolute band loudness derived from the high reference.
    pub abl: f32,
    /// Dynamic-range parity, ABDR / WSDR.
    pub drp: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct TriggerStatus {
    pub flash: f32,
    pub power: f32,
    pub count: u32,
    pub armed: bool,
    pub choked: bool,
}

impl TriggerStatus {
    pub const ZERO: Self = Self {
        flash: 0.0,
        power: 0.0,
        count: 0,
        armed: false,
        choked: false,
    };
}

/// The arrangement tracker's spectrum, trend, energy, and bass-hit evidence.
#[derive(Clone, Copy, Debug)]
pub struct DropStatus {
    /// Analysis-window RMS in dBFS.
    pub rms_db: f32,
    /// Smoothed RMS envelope in dBFS.
    pub envelope_db: f32,
    /// Level-normalized log spectrum, centered and scaled by 24 dB.
    pub shape: [f32; BAND_COUNT],
    pub reference_shape: [f32; BAND_COUNT],
    pub shape_distance: f32,
    pub trend_change: f32,
    pub changed_fraction: f32,
    /// Spectral requirements used for this hop, retained with historical values.
    pub shape_distance_threshold: f32,
    pub trend_change_threshold: f32,
    pub changed_fraction_threshold: f32,
    /// Current envelope relative to the remembered active section.
    pub energy_delta_db: f32,
    /// Required relative energy loss at this analysis hop, as a positive dB value.
    pub mix_loss_threshold_db: f32,
    /// Independent mix-loss requirement and its continuous timer at this hop.
    pub mix_loss_hold_seconds: f32,
    pub mix_loss_held_seconds: f32,
    pub mix_loss_condition_met: bool,
    /// Relative gap-start condition, including history and the silence exception.
    pub gap_conditions_met: bool,
    /// Configured continuous hold and the tracker's accumulated quiet time.
    pub gap_hold_seconds: f32,
    pub gap_held_seconds: f32,
    /// Independent absolute RMS requirement and continuous timer at this hop.
    pub gap_rms_threshold_db: f32,
    pub gap_rms_hold_seconds: f32,
    pub gap_rms_held_seconds: f32,
    pub gap_rms_condition_met: bool,
    pub gap_history_ready: bool,
    /// Time until another drop may fire; relative gap arming also respects this interval.
    pub gap_cooldown_remaining: f32,
    /// Configured overall RMS floor for confirming a bass return.
    pub drop_rms_threshold_db: f32,
    /// Fast and slow envelopes of the weighted LF magnitude.
    pub bass_fast_db: f32,
    pub bass_slow_db: f32,
    /// Time constants used for the LF envelope measurements at this hop.
    pub bass_fast_seconds: f32,
    pub bass_slow_seconds: f32,
    /// Minimum fast envelope level needed to clear the configured onset fraction.
    pub bass_onset_threshold_db: f32,
    /// A current LF rise that began after this gap was confirmed.
    pub bass_onset_ready: bool,
    /// Positive fraction of the fast LF envelope above the slow envelope.
    pub bass_hit: f32,
    pub hit_threshold: f32,
    pub quiet_progress: f32,
    pub onset: OnsetFluxStatus,
}

impl Default for DropStatus {
    fn default() -> Self {
        let tuning = ArrangementSettings::default();
        Self {
            rms_db: -80.0,
            envelope_db: -80.0,
            shape: [0.0; BAND_COUNT],
            reference_shape: [0.0; BAND_COUNT],
            shape_distance: 0.0,
            trend_change: 0.0,
            changed_fraction: 0.0,
            shape_distance_threshold: tuning.shape_distance,
            trend_change_threshold: tuning.trend_change,
            changed_fraction_threshold: tuning.changed_fraction,
            energy_delta_db: 0.0,
            mix_loss_threshold_db: tuning.mix_loss_db,
            mix_loss_hold_seconds: tuning.mix_loss_hold,
            mix_loss_held_seconds: 0.0,
            mix_loss_condition_met: false,
            gap_conditions_met: false,
            gap_hold_seconds: tuning.gap_hold,
            gap_held_seconds: 0.0,
            gap_rms_threshold_db: tuning.gap_rms_db,
            gap_rms_hold_seconds: tuning.gap_rms_hold,
            gap_rms_held_seconds: 0.0,
            gap_rms_condition_met: false,
            gap_history_ready: false,
            gap_cooldown_remaining: 0.0,
            drop_rms_threshold_db: tuning.drop_min_rms_db,
            bass_fast_db: -80.0,
            bass_slow_db: -80.0,
            bass_fast_seconds: tuning.bass_fast_time,
            bass_slow_seconds: tuning.bass_slow_time,
            bass_onset_threshold_db: -80.0,
            bass_onset_ready: false,
            bass_hit: 0.0,
            hit_threshold: 0.12,
            quiet_progress: 0.0,
            onset: OnsetFluxStatus {
                threshold: 0.12,
                ..OnsetFluxStatus::default()
            },
        }
    }
}

/// Attack score and adaptive decision threshold. `smoothed` is a short peak
/// hold for display only; it never delays or causes an onset.
#[derive(Clone, Copy, Debug, Default)]
pub struct OnsetFluxStatus {
    /// Winsorized attack score actually admitted to the adaptive average.
    pub adaptation_input: f32,
    /// Short display-only peak hold of that input, preserving attacks between frames.
    pub adaptation_input_peak: f32,
    /// Causal components used to decide this hop's threshold.
    pub baseline: f32,
    pub deviation: f32,
    pub sensitivity_margin: f32,
    pub minimum_threshold: f32,
    /// Fraction of the detector region that is new in this short hop.
    pub sharpness: f32,
    /// Significant frequency area participating in the attack.
    pub coverage: f32,
    /// Whether current energy and region validation permit an onset.
    pub mix_gate: bool,
    pub flux: f32,
    pub smoothed: f32,
    /// Running attack reference; `None` before it has data.
    pub reference: Option<f32>,
    pub threshold: f32,
    /// Legacy diagnostics, zero for the new classifiers.
    pub inhibition: f32,
    /// What the suppression weight subtracts from the excitation.
    pub suppressed: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct DetectionStatus {
    pub bands: [BandStatus; BAND_COUNT],
    pub kick: TriggerStatus,
    pub snare: TriggerStatus,
    pub hat: TriggerStatus,
    pub drop: TriggerStatus,
    pub kick_flux: OnsetFluxStatus,
    pub snare_flux: OnsetFluxStatus,
    pub hat_flux: OnsetFluxStatus,
    pub drop_status: DropStatus,
}

/// One axis and direction of the sequenced quake impulse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuakeDirection {
    Left,
    Top,
    Right,
    Bottom,
}

impl QuakeDirection {
    const CYCLE: [Self; 4] = [Self::Left, Self::Top, Self::Right, Self::Bottom];

    /// Screen-space unit vector for this direction.
    pub fn unit(self) -> (f32, f32) {
        match self {
            Self::Left => (-1.0, 0.0),
            Self::Top => (0.0, -1.0),
            Self::Right => (1.0, 0.0),
            Self::Bottom => (0.0, 1.0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "LEFT",
            Self::Top => "TOP",
            Self::Right => "RIGHT",
            Self::Bottom => "BOTTOM",
        }
    }
}

/// Advances the quake direction every Nth kick and restarts on a snare.
#[derive(Clone, Copy, Debug, Default)]
pub struct QuakeSequence {
    count: u32,
    cursor: usize,
}

impl QuakeSequence {
    pub fn next(&mut self, every: u32) -> QuakeDirection {
        let every = every.max(1);
        self.count += 1;
        if self.count.is_multiple_of(every) {
            let direction = QuakeDirection::CYCLE[self.cursor];
            self.cursor = (self.cursor + 1) % QuakeDirection::CYCLE.len();
            direction
        } else {
            QuakeDirection::CYCLE[self.cursor]
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// A damped spring used by the camera and model actions.
#[derive(Clone, Copy, Debug, Default)]
pub struct Spring {
    value: f32,
    velocity: f32,
    target: f32,
}

impl Spring {
    pub fn value(&self) -> f32 {
        self.value
    }

    #[cfg(test)]
    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn velocity(&self) -> f32 {
        self.velocity
    }

    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    pub fn impulse(&mut self, velocity: f32) {
        self.velocity += velocity;
    }

    pub fn advance(&mut self, dt: f32, stiffness: f32, damping: f32) {
        assert!(stiffness > 0.0, "a spring needs a positive stiffness");
        let mut remaining = dt.max(0.0);
        while remaining > 0.0 {
            let step = remaining.min(SPRING_STEP);
            let acceleration = -stiffness * (self.value - self.target) - damping * self.velocity;
            self.velocity += acceleration * step;
            self.value += self.velocity * step;
            remaining -= step;
        }
    }

    pub fn reset(&mut self, value: f32) {
        self.value = value;
        self.velocity = 0.0;
        self.target = value;
    }

    /// Whether the spring is within `position_epsilon` of its target and
    /// moving no faster than `velocity_epsilon`.
    #[must_use]
    pub fn is_settled(&self, position_epsilon: f32, velocity_epsilon: f32) -> bool {
        (self.value - self.target).abs() <= position_epsilon
            && self.velocity.abs() <= velocity_epsilon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_gain_edits_are_local_and_interpolate_on_a_different_fft_grid() {
        let mut profile = FrequencyGainProfile::default();
        for frequency in [0.0, 50.0, 100.0, 2000.0, 192_000.0] {
            assert_eq!(profile.gain_at(frequency), 1.0);
        }
        profile.set_bin(BinAddress::new(100.0, 50.0), 0.25);
        assert_eq!(profile.gain_at(50.0), 1.0);
        assert_eq!(profile.gain_at(100.0), 0.25);
        assert_eq!(profile.gain_at(150.0), 1.0);
        assert_eq!(profile.gain_at(0.0), 1.0);
        assert_eq!(profile.gain_at(200.0), 1.0);
        assert!((profile.gain_at(125.0) - 0.625).abs() < 1e-6);

        profile.set_bin(BinAddress::new(150.0, 50.0), 0.5);
        assert_eq!(
            profile.gain_at(100.0),
            0.25,
            "an adjacent edit preserves the earlier bin"
        );
        assert_eq!(profile.gain_at(150.0), 0.5);
        assert_eq!(profile.gain_at(200.0), 1.0);
        assert!(
            (profile.gain_at(125.0) - 0.375).abs() < 1e-6,
            "a new FFT grid samples the same frequency curve"
        );

        profile.set_bin(BinAddress::new(125.0, 25.0), 0.0);
        assert_eq!(profile.gain_at(100.0), 0.25);
        assert_eq!(profile.gain_at(125.0), 0.0);
        assert_eq!(profile.gain_at(150.0), 0.5);
        profile.validate().unwrap();
        assert!(profile
            .points
            .windows(2)
            .all(|pair| pair[0].hz < pair[1].hz));
    }

    #[test]
    fn bin_gain_channels_are_independent_and_reset_to_unity() {
        let address = BinAddress::new(140.625, 46.875);
        assert_eq!(address.center_millihz, 140_625);
        assert_eq!(address.width_millihz, 46_875);
        assert_eq!(BinAddress::new(f32::NAN, 0.0), BinAddress::default());
        let mut profiles = PercussionBinGains::default();
        profiles.set_bin(0, address, 0.0);
        profiles.set_bin(1, address, 0.4);
        assert_eq!(profiles.value(0, address), 0.0);
        assert_eq!(profiles.value(1, address), 0.4);
        assert_eq!(profiles.value(2, address), 1.0);
        profiles.set_bin(0, address, f32::NAN);
        assert_eq!(profiles.value(0, address), 0.0);
        profiles.reset(0);
        assert_eq!(profiles.value(0, address), 1.0);
        assert_eq!(profiles.value(1, address), 0.4);
        profiles.validate().unwrap();
    }

    #[test]
    fn bin_gain_settings_default_for_old_configs_and_roundtrip_custom_curves() {
        for saved in ["()", "(onset_revision:2)"] {
            let restored: DetectorSettings = ron::from_str(saved).unwrap();
            assert_eq!(restored.bin_gains, PercussionBinGains::default());
        }
        let mut settings = DetectorSettings::default();
        settings
            .bin_gains
            .set_bin(0, BinAddress::new(140.625, 46.875), 0.2);
        settings
            .bin_gains
            .set_bin(2, BinAddress::new(6012.0, 43.066), 0.0);
        let encoded = ron::to_string(&settings).unwrap();
        let decoded: DetectorSettings = ron::from_str(&encoded).unwrap();
        assert_eq!(decoded, settings);
        for invalid in [
            "(bin_gains:(kick:(points:[(hz:100.0,gain:1.1)])))",
            "(bin_gains:(kick:(points:[(hz:100.0,gain:-0.1)])))",
            "(bin_gains:(kick:(points:[(hz:NaN,gain:1.0)])))",
            "(bin_gains:(hat:(points:[(hz:100.0,gain:NaN)])))",
            "(bin_gains:(snare:(points:[(hz:-1.0,gain:1.0)])))",
            "(bin_gains:(snare:(points:[(hz:192001.0,gain:1.0)])))",
            "(bin_gains:(kick:(points:[(hz:100.0,gain:0.0),(hz:100.0,gain:1.0)])))",
            "(bin_gains:(kick:(points:[(hz:200.0,gain:0.0),(hz:100.0,gain:1.0)])))",
        ] {
            assert!(
                ron::from_str::<DetectorSettings>(invalid).is_err(),
                "{invalid}"
            );
        }
        let too_large = FrequencyGainProfile {
            points: (0..=BIN_GAIN_MAX_POINTS)
                .map(|i| BinGainPoint {
                    hz: i as f32,
                    gain: 1.0,
                })
                .collect(),
        };
        assert!(too_large.validate().is_err());
    }

    #[test]
    fn arrangement_controls_default_for_old_configs_and_preserve_custom_values() {
        let legacy: DetectorSettings = ron::from_str("() ").unwrap();
        assert_eq!(legacy.arrangement, ArrangementSettings::default());
        let mut tuned = legacy;
        for setting in ArrangementSetting::ALL {
            let (low, high) = setting.range();
            tuned.arrangement.set(setting, low + (high - low) * 0.7);
        }
        let saved = ron::to_string(&tuned).unwrap();
        let restored: DetectorSettings = ron::from_str(&saved).unwrap();
        assert_eq!(restored.arrangement, tuned.arrangement);
        assert!(ron::from_str::<DetectorSettings>("(arrangement:(gap_hold:0.0))").is_err());
        assert!(ron::from_str::<DetectorSettings>("(arrangement:(mix_loss_db:NaN))").is_err());
    }

    #[test]
    fn independent_mix_loss_hold_defaults_for_old_configs_and_round_trips_custom_values() {
        for saved in [
            "()",
            "(arrangement:(mix_loss_db:3.5,gap_hold:2.0,gap_rms_hold:0.75))",
        ] {
            let settings: DetectorSettings = ron::from_str(saved).unwrap();
            assert_eq!(settings.arrangement.mix_loss_hold, 0.906454);
        }
        let mut settings: DetectorSettings =
            ron::from_str("(arrangement:(mix_loss_db:3.5,gap_hold:2.0,gap_rms_hold:0.75))")
                .unwrap();
        settings
            .arrangement
            .set(ArrangementSetting::MixLossHold, 0.85);
        assert_eq!(settings.arrangement.mix_loss_db, 3.5);
        assert_eq!(settings.arrangement.gap_hold, 2.0);
        assert_eq!(settings.arrangement.gap_rms_hold, 0.75);
        let saved = ron::to_string(&settings).unwrap();
        let restored: DetectorSettings = ron::from_str(&saved).unwrap();
        assert_eq!(restored, settings);
        assert_eq!(ArrangementSetting::MixLossHold.format(0.85), "850 ms");
        assert_eq!(ArrangementSetting::MixLossHold.format(1.2), "1.20 s");
        assert!(ArrangementSetting::MixLossHold.is_time());
        let status = DropStatus::default();
        assert_eq!(status.mix_loss_hold_seconds, 0.906454);
        assert_eq!(status.mix_loss_held_seconds, 0.0);
        assert!(!status.mix_loss_condition_met);
    }

    #[test]
    fn independent_mix_loss_hold_validates_ranges_and_ignores_nonfinite_edits() {
        for saved in [
            "(arrangement:(mix_loss_hold:0.0))",
            "(arrangement:(mix_loss_hold:0.049))",
            "(arrangement:(mix_loss_hold:10.001))",
            "(arrangement:(mix_loss_hold:NaN))",
            "(arrangement:(mix_loss_hold:inf))",
        ] {
            assert!(ron::from_str::<DetectorSettings>(saved).is_err(), "{saved}");
        }
        let mut settings = ArrangementSettings::default();
        settings.set(ArrangementSetting::MixLossHold, -1.0);
        assert_eq!(settings.mix_loss_hold, 0.05);
        settings.set(ArrangementSetting::MixLossHold, 100.0);
        assert_eq!(settings.mix_loss_hold, 10.0);
        let saved = settings;
        settings.set(ArrangementSetting::MixLossHold, f32::NAN);
        settings.set(ArrangementSetting::MixLossHold, f32::INFINITY);
        assert_eq!(settings, saved);
        settings.validate().unwrap();
    }

    #[test]
    fn absolute_rms_gap_controls_preserve_old_settings_and_round_trip_custom_values() {
        for saved in [
            "()",
            "(arrangement:(mix_loss_db:0.25,shape_distance:0.01,gap_hold:0.05))",
        ] {
            let restored: DetectorSettings = ron::from_str(saved).unwrap();
            assert_eq!(restored.arrangement.gap_rms_db, -24.0);
            assert_eq!(restored.arrangement.gap_rms_hold, 0.5);
        }
        let restored: DetectorSettings =
            ron::from_str("(arrangement:(mix_loss_db:0.25,shape_distance:0.01,gap_hold:0.05))")
                .unwrap();
        assert_eq!(restored.arrangement.mix_loss_db, 0.25);
        assert_eq!(restored.arrangement.shape_distance, 0.01);
        assert_eq!(restored.arrangement.gap_hold, 0.05);

        let mut settings = restored;
        settings
            .arrangement
            .set(ArrangementSetting::GapRmsDb, -33.0);
        settings
            .arrangement
            .set(ArrangementSetting::GapRmsHold, 0.75);
        let encoded = ron::to_string(&settings).unwrap();
        let decoded: DetectorSettings = ron::from_str(&encoded).unwrap();
        assert_eq!(decoded, settings);
        assert_eq!(ArrangementSetting::GapRmsDb.format(-33.0), "-33.0 dB");
        assert_eq!(ArrangementSetting::GapRmsHold.format(0.75), "750 ms");
        assert!(ArrangementSetting::GapRmsHold.is_time());
        assert!(!ArrangementSetting::GapRmsDb.is_time());
    }

    #[test]
    fn absolute_rms_gap_controls_validate_ranges_and_ignore_nonfinite_edits() {
        for saved in [
            "(arrangement:(gap_rms_db:-81.0))",
            "(arrangement:(gap_rms_db:0.1))",
            "(arrangement:(gap_rms_db:NaN))",
            "(arrangement:(gap_rms_hold:0.0))",
            "(arrangement:(gap_rms_hold:10.1))",
            "(arrangement:(gap_rms_hold:inf))",
        ] {
            assert!(ron::from_str::<DetectorSettings>(saved).is_err(), "{saved}");
        }
        let mut settings = ArrangementSettings::default();
        settings.set(ArrangementSetting::GapRmsDb, -100.0);
        settings.set(ArrangementSetting::GapRmsHold, 100.0);
        assert_eq!(settings.gap_rms_db, -80.0);
        assert_eq!(settings.gap_rms_hold, 10.0);
        let saved = settings;
        settings.set(ArrangementSetting::GapRmsDb, f32::NAN);
        settings.set(ArrangementSetting::GapRmsHold, f32::INFINITY);
        assert_eq!(settings, saved);
        settings.validate().unwrap();
    }

    #[test]
    fn lf_envelope_controls_default_for_saved_configs_and_round_trip_custom_times() {
        for saved in ["()", "(arrangement:(gap_hold:0.05,bass_hit_threshold:0.1))"] {
            let settings: DetectorSettings = ron::from_str(saved).unwrap();
            assert_eq!(settings.arrangement.bass_fast_time, 0.05005731);
            assert_eq!(settings.arrangement.bass_slow_time, 0.5005731);
        }
        let mut settings = DetectorSettings::default();
        settings
            .arrangement
            .set(ArrangementSetting::BassFastTime, 0.010);
        settings
            .arrangement
            .set(ArrangementSetting::BassSlowTime, 0.125);
        let encoded = ron::to_string(&settings).unwrap();
        let decoded: DetectorSettings = ron::from_str(&encoded).unwrap();
        assert_eq!(decoded, settings);
        assert_eq!(ArrangementSetting::BassFastTime.format(0.010), "10 ms");
        assert_eq!(ArrangementSetting::BassSlowTime.format(0.125), "125 ms");
        assert!(ArrangementSetting::BassFastTime.is_time());
        assert!(ArrangementSetting::BassSlowTime.is_time());
        let defaults = DropStatus::default();
        assert_eq!(defaults.bass_fast_seconds, 0.05005731);
        assert_eq!(defaults.bass_slow_seconds, 0.5005731);
    }

    #[test]
    fn lf_envelope_controls_keep_the_edited_time_separated_and_validate_saved_pairs() {
        let mut settings = ArrangementSettings::default();
        settings.set(ArrangementSetting::BassSlowTime, 0.020);
        assert_eq!(settings.bass_fast_time, 0.05005731);
        assert_eq!(settings.bass_slow_time, settings.bass_fast_time + 0.005);
        settings.validate().unwrap();
        let slow_before_edit = settings.bass_slow_time;
        settings.set(ArrangementSetting::BassFastTime, 0.200);
        assert_eq!(settings.bass_slow_time, slow_before_edit);
        assert_eq!(settings.bass_fast_time, settings.bass_slow_time - 0.005);
        settings.validate().unwrap();
        settings.set(ArrangementSetting::BassSlowTime, 2.0);
        settings.set(ArrangementSetting::BassFastTime, 0.200);
        assert_eq!(settings.bass_fast_time, 0.200);
        assert_eq!(settings.bass_slow_time, 2.0);
        let saved = settings;
        settings.set(ArrangementSetting::BassFastTime, f32::NAN);
        settings.set(ArrangementSetting::BassSlowTime, f32::INFINITY);
        assert_eq!(settings, saved);
        let boundary: DetectorSettings =
            ron::from_str("(arrangement:(bass_fast_time:0.200,bass_slow_time:0.205))").unwrap();
        boundary.arrangement.validate().unwrap();
        for invalid in [
            "(arrangement:(bass_fast_time:0.001))",
            "(arrangement:(bass_fast_time:0.201))",
            "(arrangement:(bass_slow_time:0.019))",
            "(arrangement:(bass_slow_time:2.001))",
            "(arrangement:(bass_fast_time:0.050,bass_slow_time:0.040))",
            "(arrangement:(bass_fast_time:0.050,bass_slow_time:0.050))",
            "(arrangement:(bass_fast_time:NaN))",
            "(arrangement:(bass_slow_time:inf))",
        ] {
            assert!(
                ron::from_str::<DetectorSettings>(invalid).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn kick_frequency_limits_default_for_existing_configs_and_roundtrip_custom_values() {
        let defaults = DetectorSettings::default();
        for serialized in ["()", "(onset_revision:2,kick_onset_sensitivity:2.2)"] {
            let restored: DetectorSettings = ron::from_str(serialized).unwrap();
            assert_eq!(restored.kick_low_hz, defaults.kick_low_hz);
            assert_eq!(restored.kick_high_hz, defaults.kick_high_hz);
        }
        let custom = DetectorSettings {
            kick_low_hz: 75.0,
            kick_high_hz: 410.5,
            ..defaults
        };
        let serialized = ron::to_string(&custom).unwrap();
        let restored: DetectorSettings = ron::from_str(&serialized).unwrap();
        assert_eq!(restored, custom);
        for invalid in [
            "(kick_low_hz:19.0)",
            "(kick_high_hz:2001.0)",
            "(kick_low_hz:NaN)",
            "(kick_high_hz:NaN)",
            "(kick_low_hz:300.0,kick_high_hz:200.0)",
            "(kick_low_hz:200.0,kick_high_hz:209.0)",
        ] {
            assert!(
                ron::from_str::<DetectorSettings>(invalid).is_err(),
                "{invalid}"
            );
        }
        for valid in [
            "(kick_low_hz:20.0,kick_high_hz:30.0)",
            "(kick_low_hz:1018.7,kick_high_hz:1028.7)",
            "(kick_low_hz:1990.0,kick_high_hz:2000.0)",
        ] {
            assert!(ron::from_str::<DetectorSettings>(valid).is_ok(), "{valid}");
        }
    }

    #[test]
    fn kick_routing_tracks_configured_hz_overlap_without_touching_other_regions() {
        let defaults = DetectorSettings::default();
        let selected = |settings: &DetectorSettings| {
            (0..BAND_COUNT)
                .filter(|&band| settings.kick_contains(band))
                .collect::<Vec<_>>()
        };
        assert_eq!(selected(&defaults), [0, 1, 2, 3, 4, 5]);
        let custom = DetectorSettings {
            kick_low_hz: 50.0,
            kick_high_hz: 110.0,
            ..defaults.clone()
        };
        assert_eq!(selected(&custom), [1, 2, 3]);
        assert!(!custom.kick_contains(BAND_COUNT));
        for band in 0..BAND_COUNT {
            assert_eq!(custom.snare_contains(band), defaults.snare_contains(band));
            assert_eq!(custom.hat_contains(band), defaults.hat_contains(band));
        }
        let boundary = DetectorSettings {
            kick_low_hz: 151.31866,
            kick_high_hz: 223.14035,
            ..defaults
        };
        assert_eq!(selected(&boundary), [4]);
    }

    fn defaults() -> DetectorSettings {
        DetectorSettings::default()
    }

    #[test]
    fn abl_scales_the_high_reference_before_the_curve() {
        let settings = BandOnsetSettings {
            abl_gain: 2.0,
            abl_curve: Curve::Linear,
            abl_amount: 1.0,
        };
        assert!((settings.loudness(0.25) - 0.5).abs() < 1e-6);
        assert_eq!(BandOnsetSettings::default().loudness(1.0), 1.0);
        assert_eq!(BandOnsetSettings::default().loudness(0.0), 0.0);
    }

    #[test]
    fn band_onset_defaults_match_the_recipe() {
        let settings = defaults();
        let band = settings.band_onset[0];
        assert_eq!(band.abl_gain, 1.0);
        assert_eq!(band.abl_curve, Curve::SCurve);
        assert_eq!(band.abl_amount, 2.5);
        assert_eq!(settings.excess.background_attack, 0.320);
        assert_eq!(settings.excess.background_release, 0.200);
        assert_eq!(settings.excess.floor, 0.010);
        assert_eq!(settings.excess.cap, 1.0);
        assert!(settings
            .band_onset
            .iter()
            .all(|candidate| *candidate == band));
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn temporal_settings_migrate_score_units_but_preserve_motion_and_energy_preferences() {
        let legacy = "(kick_onset_min_threshold:0.027,kick_onset_average:1.2,snare_min_rms_db:-29.0,kick_quake_factor:17.0)";
        let migrated: DetectorSettings = ron::from_str(legacy).unwrap();
        assert_eq!(migrated.kick_onset_min_threshold, 0.25);
        assert_eq!(migrated.kick_onset_average, 0.7738249);
        assert_eq!(migrated.snare_min_rms_db, -29.0);
        assert_eq!(migrated.kick_quake_factor, 17.0);
        let tuned = DetectorSettings {
            kick_onset_min_threshold: 0.22,
            kick_onset_average: 1.4,
            ..migrated
        };
        let saved = ron::to_string(&tuned).unwrap();
        assert!(saved.contains("onset_revision:3"));
        let restored: DetectorSettings = ron::from_str(&saved).unwrap();
        assert_eq!(restored.kick_onset_min_threshold, 0.22);
        assert_eq!(restored.kick_onset_average, 1.4);
    }

    #[test]
    fn the_live_detectors_describe_their_frequency_regions() {
        let settings = defaults();
        assert!(settings.kick_contains(0));
        assert!(settings.kick_contains(4));
        assert!(!settings.kick_contains(15));
        assert!(settings.snare_contains(8));
        assert!(settings.snare_contains(15));
        assert!(!settings.snare_contains(1));
        assert!(settings.hat_contains(15));
        assert!(!settings.hat_contains(0));
    }

    #[test]
    fn the_hat_kernel_matches_high_band_flow() {
        let settings = defaults();
        let mut high = VecDeque::new();
        let mut low = VecDeque::new();
        for _ in 0..8 {
            high.push_back(std::array::from_fn(
                |band| if band >= 9 { 1.0 } else { 0.0 },
            ));
            low.push_back(std::array::from_fn(|band| if band < 6 { 1.0 } else { 0.0 }));
        }
        let high = settings.hat_pattern.project(
            &high,
            settings.hat_onset_suppression,
            settings.hat_onset_pre_gain,
            8,
        );
        let low = settings.hat_pattern.project(
            &low,
            settings.hat_onset_suppression,
            settings.hat_onset_pre_gain,
            8,
        );
        assert!(high.residual > low.residual);
        assert!(high.residual > 0.0);
    }

    #[test]
    fn the_suppression_kernel_is_independent_of_the_main_kernel() {
        let settings = defaults();
        let low_band = 0.5 / BAND_COUNT as f32;
        let high_band = 12.5 / BAND_COUNT as f32;
        assert!(
            settings
                .kick_pattern
                .suppression_field_value(0.05, low_band)
                > 0.0,
            "the kick suppressor covers the low bands"
        );
        assert_eq!(
            settings
                .kick_pattern
                .suppression_field_value(0.05, high_band),
            0.0,
            "the suppressor ignores the high bands"
        );
        assert!(
            settings.kick_pattern.field_value(0.05, high_band) > 0.0,
            "the main kernel still covers the high bands"
        );
    }

    #[test]
    fn band_onset_flow_applies_drp_and_abl() {
        let settings = defaults();
        let dt = 1.0 / 86.0;
        let mut bank = OnsetBank::new();
        let spectrum = Spectrum {
            band_excess: [0.5; BAND_COUNT],
            ..Spectrum::default()
        };
        let abdr = std::array::from_fn(|band| if band == 0 { 16.0 } else { 0.0 });
        let mut abl = [1.0; BAND_COUNT];
        abl[0] = 0.5;
        bank.observe(&spectrum, &abdr, &abl, &settings, dt);
        let status = bank.status();
        assert!((status.bands[0].drp - 16.0).abs() < 1e-6);
        assert!((status.bands[0].abl - 0.5).abs() < 1e-6);
        assert!((status.bands[0].onset - 4.0).abs() < 1e-6);
        assert_eq!(status.bands[1].onset, 0.0);
        bank.observe(&spectrum, &abdr, &abl, &settings, dt);
        assert!(
            (bank.status().bands[0].onset - 4.0).abs() < 1e-6,
            "the onset flow stays continuous while the excess is present"
        );
    }

    #[test]
    fn temporal_pattern_interpolates_between_polygons() {
        let pattern = TemporalPattern {
            inner: rectangle(0.25, 0.75, 0.25, 0.75),
            outer: rectangle(0.1, 0.9, 0.1, 0.9),
            suppression_inner: Vec::new(),
            suppression_outer: Vec::new(),
            effective: 1.0,
        };
        assert!((pattern.field_value(0.5, 0.5) - 1.0).abs() < 1e-6);
        assert_eq!(pattern.field_value(0.05, 0.5), 0.0);
        let edge = pattern.field_value(0.15, 0.5);
        assert!(edge > 0.0 && edge < 1.0, "annulus value was {edge}");
    }

    #[test]
    fn initial_patterns_stay_inside_the_normalized_grid() {
        for pattern in [
            TemporalPattern::kick_initial(),
            TemporalPattern::snare_initial(),
            TemporalPattern::hat_initial(),
        ] {
            pattern.validate().expect("initial patterns stay valid");
        }
    }

    #[test]
    fn effective_area_keeps_the_leftmost_part_and_fades_its_end() {
        let pattern = TemporalPattern {
            inner: rectangle(0.0, 1.0, 0.0, 1.0),
            outer: rectangle(0.0, 1.0, 0.0, 1.0),
            suppression_inner: Vec::new(),
            suppression_outer: Vec::new(),
            effective: 0.5,
        };
        assert!((pattern.effective_field_value(0.25, 0.5) - 1.0).abs() < 1e-6);
        assert_eq!(pattern.effective_field_value(0.5, 0.5), 0.0);
        assert_eq!(pattern.effective_field_value(0.9, 0.5), 0.0);
        assert!((pattern.effective_field_value(0.45, 0.5) - 1.0).abs() < 1e-6);
        assert!((pattern.effective_field_value(0.48, 0.5) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn onset_pre_gain_trims_the_pattern_projection() {
        let mut settings = defaults();
        settings.kick_onset_suppression = 0.0;
        settings.kick_onset_pre_gain = 0.25;
        let history = VecDeque::from([[1.0; BAND_COUNT]]);
        let full = settings
            .kick_pattern
            .project(&history, settings.kick_onset_suppression, 1.0, 1);
        let projection = settings.kick_pattern.project(
            &history,
            settings.kick_onset_suppression,
            settings.kick_onset_pre_gain,
            1,
        );
        assert!((projection.residual - full.residual * 0.25).abs() < 1e-6);
        assert!((projection.inhibition - full.inhibition * 0.25).abs() < 1e-6);
        assert!((projection.suppressed - 0.0).abs() < 1e-6);
    }

    #[test]
    fn the_suppression_kernel_suppresses_other_instrument_flow() {
        let mut settings = defaults();
        settings.snare_onset_suppression = 1.0;
        let mut low = [0.0; BAND_COUNT];
        low[..5].fill(1.0);
        let low_history = VecDeque::from([low]);
        let projection = settings.snare_pattern.project(
            &low_history,
            settings.snare_onset_suppression,
            settings.snare_onset_pre_gain,
            1,
        );
        assert_eq!(
            projection.residual, 0.0,
            "a low-only transient must not confirm a snare"
        );
        assert!(projection.inhibition > 0.0);
        assert_eq!(projection.suppressed, projection.inhibition);

        let broadband = VecDeque::from([[1.0; BAND_COUNT]]);
        let unsuppressed =
            settings
                .snare_pattern
                .project(&broadband, 0.0, settings.snare_onset_pre_gain, 1);
        settings.snare_onset_suppression = 0.5;
        let projection = settings.snare_pattern.project(
            &broadband,
            settings.snare_onset_suppression,
            settings.snare_onset_pre_gain,
            1,
        );
        assert!((projection.inhibition - unsuppressed.inhibition).abs() < 1e-6);
        assert!(
            (projection.suppressed - projection.inhibition * 0.5).abs() < 1e-6,
            "half the suppression projection is subtracted from a broadband hit"
        );
        assert!(
            (projection.residual - (unsuppressed.residual - projection.suppressed).max(0.0)).abs()
                < 1e-6
        );
    }

    #[test]
    fn drop_kernel_weights_stay_inside_zero_to_one() {
        let mut kernels = DropKernels::default();
        kernels.set(DropKernel::Onset, 2, 2.0);
        assert_eq!(kernels.onset[2], 1.0);
        kernels.set(DropKernel::Onset, 3, -1.0);
        assert_eq!(kernels.onset[3], 0.0);
        assert_eq!(kernels.weights(DropKernel::Onset), kernels.onset);
    }

    #[test]
    fn the_quake_sequence_advances_every_nth_kick_and_resets() {
        let mut sequence = QuakeSequence::default();
        assert_eq!(sequence.next(1), QuakeDirection::Left);
        assert_eq!(sequence.next(1), QuakeDirection::Top);
        assert_eq!(sequence.next(1), QuakeDirection::Right);
        assert_eq!(sequence.next(1), QuakeDirection::Bottom);
        sequence.reset();
        assert_eq!(sequence.next(2), QuakeDirection::Left);
        assert_eq!(sequence.next(2), QuakeDirection::Left);
        assert_eq!(sequence.next(2), QuakeDirection::Top);
        assert_eq!(QuakeDirection::Top.unit(), (0.0, -1.0));
    }

    #[test]
    fn a_damped_spring_overshoots_and_settles_on_its_target() {
        let mut spring = Spring::default();
        spring.set_target(1.0);
        let mut peak: f32 = 0.0;
        for _ in 0..240 {
            spring.advance(1.0 / 120.0, 70.0, 7.0);
            peak = peak.max(spring.value());
        }
        assert!(peak > 1.0, "an underdamped spring overshoots");
        assert!((spring.value() - 1.0).abs() < 0.01);
    }

    #[test]
    fn invalid_settings_are_rejected_early() {
        let mut settings = defaults();
        settings.band_onset[0].abl_gain = 0.0;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.excess.cap = 0.0;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.kick_onset_pre_gain = 0.0;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.kick_pattern.inner[0][0] = -0.1;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.snare_pattern.outer[0][1] = f32::NAN;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.kick_onset_attack = 0.0;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.snare_onset_sensitivity = 9.0;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.snare_min_rms_db = 1.0;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.snare_min_rms_db = -61.0;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.snare_min_rms_db = f32::NAN;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.drop_gap_threshold = 1.0;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.drop_gap_threshold = -61.0;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.drop_gap_threshold = f32::NAN;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.drop_kernels.onset[0] = -0.1;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.drop_kernels.onset[0] = f32::NAN;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.drop_onset_attack = 0.0;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.drop_onset_suppression = 3.0;
        assert!(settings.validate().is_err());

        let mut settings = defaults();
        settings.kick_sequence_every = 0;
        assert!(settings.validate().is_err());
    }
}
