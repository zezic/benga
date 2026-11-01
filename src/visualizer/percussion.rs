//! Physical feature and population contract for the adaptive percussion path.
//! All powers are normalized full-scale squared; energies are FS² seconds.
use serde::{Deserialize, Serialize};

pub const HISTOGRAM_BINS: usize = 24;
pub const HEATMAP_BINS: usize = HISTOGRAM_BINS;
pub const MAX_RECENT_MARKERS: usize = 64;
pub const ENVELOPE_POINTS: usize = 64;
pub const REFERENCE_CAPACITY: usize = 8192;
pub const DECISION_CAPACITY: usize = 256;

/// Optional learning/trigger criteria. Defaults preserve the established
/// detector behavior; numerical validity and bounded event bookkeeping remain
/// unconditional when a user disables any of these criteria.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PercussionFilters {
    pub input_energy: bool,
    pub input_growth: bool,
    pub input_prominence: bool,
    pub min_sharpness: bool,
    pub output_prominence: bool,
    pub attack_level: bool,
    pub mix_rms: bool,
    pub sharpness_percentile: bool,
    pub energy_percentile: bool,
    pub output_interval: bool,
    pub reject_overlap: bool,
}

impl Default for PercussionFilters {
    fn default() -> Self {
        Self {
            input_energy: true,
            input_growth: true,
            input_prominence: true,
            min_sharpness: true,
            output_prominence: true,
            attack_level: true,
            mix_rms: true,
            sharpness_percentile: true,
            energy_percentile: true,
            output_interval: true,
            reject_overlap: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PercussionChannelSettings {
    pub filters: PercussionFilters,
    pub low_hz: f32,
    pub high_hz: f32,
    /// End of the raised-cosine POWER taper. Equal to high_hz disables taper.
    pub taper_hz: f32,
    pub feature_ms: f32,
    pub concentration_ms: f32,
    pub envelope_ms: f32,
    pub background_ms: f32,
    pub merge_ms: f32,
    pub rearm_ms: f32,
    pub admission_energy: f32,
    pub admission_growth: f32,
    pub admission_prominence: f32,
    pub min_sharpness: f32,
    /// Final contrast floor, separate from the permissive admission floor.
    pub min_output_prominence: f32,
    pub min_attack_db: f32,
    pub output_rms_db: f32,
    pub sharpness_percentile: f32,
    pub energy_percentile: f32,
    pub memory_seconds: f32,
    pub min_reference: usize,
    pub output_interval_ms: f32,
}
impl Default for PercussionChannelSettings {
    fn default() -> Self {
        Self {
            filters: PercussionFilters::default(),
            low_hz: 60.0,
            high_hz: 200.0,
            taper_hz: 200.0,
            feature_ms: 24.0,
            concentration_ms: 5.0,
            envelope_ms: 1.0,
            background_ms: 80.0,
            merge_ms: 6.0,
            rearm_ms: 12.0,
            admission_energy: 0.000_000_1,
            admission_growth: 0.001,
            admission_prominence: 0.08,
            min_sharpness: 0.40,
            min_output_prominence: 0.16,
            min_attack_db: -38.0,
            output_rms_db: -27.0,
            sharpness_percentile: 60.0,
            energy_percentile: 40.0,
            memory_seconds: 15.0,
            min_reference: 8,
            output_interval_ms: 65.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PercussionSettings {
    #[serde(deserialize_with = "deserialize_channels")]
    pub channels: [PercussionChannelSettings; 3],
    /// Fast FFT samples at 48 kHz. Kick has a separate physical LF window.
    pub fft_size: usize,
    pub hop_ms: f32,
    /// Display-only count of recent admitted pairs overlaid on the heatmap.
    pub recent_markers: usize,
}

// A channel's omitted fields inherit that instrument's defaults. Deserializing
// each channel directly would apply the generic channel default to snare/hat
// and would also lose kick's longer observation horizon.
macro_rules! channel_overrides {
    ($($field:ident: $ty:ty),* $(,)?) => {
        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields, rename = "PercussionChannelSettings")]
        struct ChannelOverrides {
            $(#[serde(deserialize_with = "deserialize_present")]
            $field: Option<$ty>,)*
        }

        impl ChannelOverrides {
            fn apply(self, settings: &mut PercussionChannelSettings) {
                $(if let Some(value) = self.$field { settings.$field = value; })*
            }
        }
    };
}

channel_overrides! {
    filters: PercussionFilters,
    low_hz: f32,
    high_hz: f32,
    taper_hz: f32,
    feature_ms: f32,
    concentration_ms: f32,
    envelope_ms: f32,
    background_ms: f32,
    merge_ms: f32,
    rearm_ms: f32,
    admission_energy: f32,
    admission_growth: f32,
    admission_prominence: f32,
    min_sharpness: f32,
    min_output_prominence: f32,
    min_attack_db: f32,
    output_rms_db: f32,
    sharpness_percentile: f32,
    energy_percentile: f32,
    memory_seconds: f32,
    min_reference: usize,
    output_interval_ms: f32,
}

fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn deserialize_channels<'de, D>(deserializer: D) -> Result<[PercussionChannelSettings; 3], D::Error>
where
    D: serde::Deserializer<'de>,
{
    let patches = <[ChannelOverrides; 3]>::deserialize(deserializer)?;
    let mut channels = PercussionSettings::default().channels;
    for (settings, patch) in channels.iter_mut().zip(patches) {
        patch.apply(settings);
    }
    Ok(channels)
}
impl Default for PercussionSettings {
    fn default() -> Self {
        let kick = PercussionChannelSettings {
            feature_ms: 44.0,
            min_sharpness: 0.225,
            min_output_prominence: 0.55,
            ..PercussionChannelSettings::default()
        };
        let snare = PercussionChannelSettings {
            low_hz: 2_000.0,
            high_hz: 5_000.0,
            taper_hz: 10_000.0,
            min_sharpness: 0.46,
            min_output_prominence: 0.16,
            feature_ms: 24.0,
            output_interval_ms: 65.0,
            ..kick
        };
        let hat = PercussionChannelSettings {
            low_hz: 5_000.0,
            high_hz: 16_000.0,
            taper_hz: 16_000.0,
            min_sharpness: 0.46,
            min_output_prominence: 0.16,
            feature_ms: 24.0,
            output_interval_ms: 25.0,
            ..kick
        };
        Self {
            channels: [kick, snare, hat],
            fft_size: 512,
            hop_ms: 1.0,
            recent_markers: 32,
        }
    }
}
impl PercussionSettings {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (4..=MAX_RECENT_MARKERS).contains(&self.recent_markers),
            "recent heatmap markers must be 4..={MAX_RECENT_MARKERS}"
        );
        anyhow::ensure!(
            [256, 512].contains(&self.fft_size),
            "percussion FFT must be 256 or 512 samples at 48 kHz"
        );
        anyhow::ensure!(
            self.hop_ms.is_finite() && (0.5..=2.0).contains(&self.hop_ms),
            "percussion hop must be 0.5..=2 ms"
        );
        for (channel, settings) in self.channels.iter().enumerate() {
            for setting in PercussionSetting::CHANNEL {
                let value = setting.get(self, channel);
                let (lo, hi) = setting.range();
                anyhow::ensure!(
                    value.is_finite() && (lo..=hi).contains(&value),
                    "percussion channel {channel} {} must be finite in {lo}..={hi}",
                    setting.label()
                );
            }
            anyhow::ensure!(
                settings.high_hz >= settings.low_hz + 10.0,
                "percussion mask must span at least 10 Hz"
            );
            anyhow::ensure!(
                settings.taper_hz >= settings.high_hz,
                "percussion power taper must end at/above main region"
            );
            anyhow::ensure!(
                settings.concentration_ms <= settings.feature_ms,
                "concentration window must fit the feature horizon"
            );
            anyhow::ensure!(
                settings.min_reference <= REFERENCE_CAPACITY,
                "reference warmup exceeds bounded capacity"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PercussionSetting {
    InputEnergyEnabled,
    InputGrowthEnabled,
    InputProminenceEnabled,
    MinSharpnessEnabled,
    OutputProminenceEnabled,
    AttackLevelEnabled,
    MixRmsEnabled,
    SharpnessPercentileEnabled,
    EnergyPercentileEnabled,
    OutputIntervalEnabled,
    RejectOverlap,
    LowHz,
    HighHz,
    TaperHz,
    FeatureMs,
    ConcentrationMs,
    EnvelopeMs,
    BackgroundMs,
    MergeMs,
    RearmMs,
    AdmissionEnergy,
    AdmissionGrowth,
    AdmissionProminence,
    MinSharpness,
    MinOutputProminence,
    MinAttackDb,
    OutputRmsDb,
    SharpnessPercentile,
    EnergyPercentile,
    MemorySeconds,
    MinReference,
    OutputIntervalMs,
    FftSize,
    HopMs,
    RecentMarkers,
}
impl PercussionSetting {
    pub const CHANNEL: [Self; 32] = [
        Self::InputEnergyEnabled,
        Self::InputGrowthEnabled,
        Self::InputProminenceEnabled,
        Self::MinSharpnessEnabled,
        Self::OutputProminenceEnabled,
        Self::AttackLevelEnabled,
        Self::MixRmsEnabled,
        Self::SharpnessPercentileEnabled,
        Self::EnergyPercentileEnabled,
        Self::OutputIntervalEnabled,
        Self::RejectOverlap,
        Self::LowHz,
        Self::HighHz,
        Self::TaperHz,
        Self::FeatureMs,
        Self::ConcentrationMs,
        Self::EnvelopeMs,
        Self::BackgroundMs,
        Self::MergeMs,
        Self::RearmMs,
        Self::AdmissionEnergy,
        Self::AdmissionGrowth,
        Self::AdmissionProminence,
        Self::MinSharpness,
        Self::MinOutputProminence,
        Self::MinAttackDb,
        Self::OutputRmsDb,
        Self::SharpnessPercentile,
        Self::EnergyPercentile,
        Self::MemorySeconds,
        Self::MinReference,
        Self::OutputIntervalMs,
    ];
    pub fn primary() -> &'static [Self] {
        &[
            Self::SharpnessPercentile,
            Self::EnergyPercentile,
            Self::MemorySeconds,
            Self::MinSharpness,
            Self::MinAttackDb,
            Self::OutputRmsDb,
            Self::OutputIntervalMs,
        ]
    }
    pub fn is_filter_toggle(self) -> bool {
        matches!(
            self,
            Self::InputEnergyEnabled
                | Self::InputGrowthEnabled
                | Self::InputProminenceEnabled
                | Self::MinSharpnessEnabled
                | Self::OutputProminenceEnabled
                | Self::AttackLevelEnabled
                | Self::MixRmsEnabled
                | Self::SharpnessPercentileEnabled
                | Self::EnergyPercentileEnabled
                | Self::OutputIntervalEnabled
                | Self::RejectOverlap
        )
    }
    pub fn filter_toggle(self) -> Option<Self> {
        match self {
            Self::AdmissionEnergy => Some(Self::InputEnergyEnabled),
            Self::AdmissionGrowth => Some(Self::InputGrowthEnabled),
            Self::AdmissionProminence => Some(Self::InputProminenceEnabled),
            Self::MinSharpness => Some(Self::MinSharpnessEnabled),
            Self::MinOutputProminence => Some(Self::OutputProminenceEnabled),
            Self::MinAttackDb => Some(Self::AttackLevelEnabled),
            Self::OutputRmsDb => Some(Self::MixRmsEnabled),
            Self::SharpnessPercentile => Some(Self::SharpnessPercentileEnabled),
            Self::EnergyPercentile => Some(Self::EnergyPercentileEnabled),
            Self::OutputIntervalMs => Some(Self::OutputIntervalEnabled),
            _ => None,
        }
    }
    pub fn advanced() -> &'static [Self] {
        &[
            Self::LowHz,
            Self::HighHz,
            Self::TaperHz,
            Self::FeatureMs,
            Self::ConcentrationMs,
            Self::EnvelopeMs,
            Self::BackgroundMs,
            Self::MergeMs,
            Self::RearmMs,
            Self::AdmissionEnergy,
            Self::AdmissionGrowth,
            Self::AdmissionProminence,
            Self::MinOutputProminence,
            Self::MinReference,
            Self::FftSize,
            Self::HopMs,
            Self::RecentMarkers,
            Self::RejectOverlap,
        ]
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::InputEnergyEnabled => "Input energy filter",
            Self::InputGrowthEnabled => "Input rise filter",
            Self::InputProminenceEnabled => "Input contrast filter",
            Self::MinSharpnessEnabled => "Sharpness floor filter",
            Self::OutputProminenceEnabled => "Trigger contrast filter",
            Self::AttackLevelEnabled => "Trigger energy filter",
            Self::MixRmsEnabled => "Mix level filter",
            Self::SharpnessPercentileEnabled => "Sharpness percentile filter",
            Self::EnergyPercentileEnabled => "Energy percentile filter",
            Self::OutputIntervalEnabled => "Trigger spacing filter",
            Self::RejectOverlap => "Reject overlapping attacks",
            Self::LowHz => "Low Hz",
            Self::HighHz => "High Hz",
            Self::TaperHz => "Power taper Hz",
            Self::FeatureMs => "Feature window",
            Self::ConcentrationMs => "Concentration window",
            Self::EnvelopeMs => "Envelope smoothing",
            Self::BackgroundMs => "Background memory",
            Self::MergeMs => "Merge lobes",
            Self::RearmMs => "Rearm settle",
            Self::AdmissionEnergy => "Input energy floor",
            Self::AdmissionGrowth => "Input rise floor",
            Self::AdmissionProminence => "Input contrast floor",
            Self::MinSharpness => "Sharpness floor",
            Self::MinOutputProminence => "Trigger contrast floor",
            Self::MinAttackDb => "Trigger energy floor",
            Self::OutputRmsDb => "Mix level floor",
            Self::SharpnessPercentile => "Sharpness percentile",
            Self::EnergyPercentile => "Energy percentile",
            Self::MemorySeconds => "Reference lifetime",
            Self::MinReference => "Warmup observations",
            Self::OutputIntervalMs => "Trigger spacing",
            Self::FftSize => "Fast FFT at 48 kHz",
            Self::HopMs => "Analysis hop",
            Self::RecentMarkers => "Recent additions",
        }
    }
    pub fn unit(self) -> &'static str {
        match self {
            Self::LowHz | Self::HighHz | Self::TaperHz => "Hz",
            Self::FeatureMs
            | Self::ConcentrationMs
            | Self::EnvelopeMs
            | Self::BackgroundMs
            | Self::MergeMs
            | Self::RearmMs
            | Self::OutputIntervalMs
            | Self::HopMs => "ms",
            Self::AdmissionEnergy => "FS²s",
            Self::AdmissionGrowth => "FS",
            Self::MinAttackDb | Self::OutputRmsDb => "dBFS",
            Self::SharpnessPercentile | Self::EnergyPercentile => "%",
            Self::MemorySeconds => "s",
            Self::MinReference | Self::RecentMarkers => "events",
            Self::FftSize => "samples",
            _ => "",
        }
    }
    pub fn range(self) -> (f32, f32) {
        if self.is_filter_toggle() {
            return (0.0, 1.0);
        }
        match self {
            Self::InputEnergyEnabled
            | Self::InputGrowthEnabled
            | Self::InputProminenceEnabled
            | Self::MinSharpnessEnabled
            | Self::OutputProminenceEnabled
            | Self::AttackLevelEnabled
            | Self::MixRmsEnabled
            | Self::SharpnessPercentileEnabled
            | Self::EnergyPercentileEnabled
            | Self::OutputIntervalEnabled
            | Self::RejectOverlap => unreachable!("toggle range handled above"),
            Self::LowHz => (20.0, 20_000.0),
            Self::HighHz | Self::TaperHz => (30.0, 24_000.0),
            Self::FeatureMs => (16.0, 48.0),
            Self::ConcentrationMs => (2.0, 10.0),
            Self::EnvelopeMs => (0.1, 5.0),
            Self::BackgroundMs => (20.0, 500.0),
            Self::MergeMs => (2.0, 20.0),
            Self::RearmMs => (2.0, 50.0),
            Self::AdmissionEnergy => (1e-10, 1e-4),
            Self::AdmissionGrowth => (0.0001, 0.1),
            Self::AdmissionProminence => (0.01, 0.8),
            Self::MinSharpness => (0.1, 1.0),
            Self::MinOutputProminence => (0.0, 0.9),
            Self::MinAttackDb => (-65.0, -10.0),
            Self::OutputRmsDb => (-60.0, 0.0),
            Self::SharpnessPercentile | Self::EnergyPercentile => (0.0, 100.0),
            Self::MemorySeconds => (1.0, 60.0),
            Self::MinReference => (1.0, 64.0),
            Self::OutputIntervalMs => (5.0, 300.0),
            Self::FftSize => (256.0, 512.0),
            Self::HopMs => (0.5, 2.0),
            Self::RecentMarkers => (4.0, MAX_RECENT_MARKERS as f32),
        }
    }
    pub fn get(self, s: &PercussionSettings, channel: usize) -> f32 {
        let c = s.channels[channel];
        match self {
            Self::InputEnergyEnabled => u8::from(c.filters.input_energy) as f32,
            Self::InputGrowthEnabled => u8::from(c.filters.input_growth) as f32,
            Self::InputProminenceEnabled => u8::from(c.filters.input_prominence) as f32,
            Self::MinSharpnessEnabled => u8::from(c.filters.min_sharpness) as f32,
            Self::OutputProminenceEnabled => u8::from(c.filters.output_prominence) as f32,
            Self::AttackLevelEnabled => u8::from(c.filters.attack_level) as f32,
            Self::MixRmsEnabled => u8::from(c.filters.mix_rms) as f32,
            Self::SharpnessPercentileEnabled => u8::from(c.filters.sharpness_percentile) as f32,
            Self::EnergyPercentileEnabled => u8::from(c.filters.energy_percentile) as f32,
            Self::OutputIntervalEnabled => u8::from(c.filters.output_interval) as f32,
            Self::RejectOverlap => u8::from(c.filters.reject_overlap) as f32,
            Self::LowHz => c.low_hz,
            Self::HighHz => c.high_hz,
            Self::TaperHz => c.taper_hz,
            Self::FeatureMs => c.feature_ms,
            Self::ConcentrationMs => c.concentration_ms,
            Self::EnvelopeMs => c.envelope_ms,
            Self::BackgroundMs => c.background_ms,
            Self::MergeMs => c.merge_ms,
            Self::RearmMs => c.rearm_ms,
            Self::AdmissionEnergy => c.admission_energy,
            Self::AdmissionGrowth => c.admission_growth,
            Self::AdmissionProminence => c.admission_prominence,
            Self::MinSharpness => c.min_sharpness,
            Self::MinOutputProminence => c.min_output_prominence,
            Self::MinAttackDb => c.min_attack_db,
            Self::OutputRmsDb => c.output_rms_db,
            Self::SharpnessPercentile => c.sharpness_percentile,
            Self::EnergyPercentile => c.energy_percentile,
            Self::MemorySeconds => c.memory_seconds,
            Self::MinReference => c.min_reference as f32,
            Self::OutputIntervalMs => c.output_interval_ms,
            Self::FftSize => s.fft_size as f32,
            Self::HopMs => s.hop_ms,
            Self::RecentMarkers => s.recent_markers as f32,
        }
    }
    pub fn set(self, s: &mut PercussionSettings, channel: usize, value: f32) {
        if !value.is_finite() {
            return;
        }
        let (lo, hi) = self.range();
        let value = value.clamp(lo, hi);
        let c = &mut s.channels[channel];
        match self {
            Self::InputEnergyEnabled => c.filters.input_energy = value >= 0.5,
            Self::InputGrowthEnabled => c.filters.input_growth = value >= 0.5,
            Self::InputProminenceEnabled => c.filters.input_prominence = value >= 0.5,
            Self::MinSharpnessEnabled => c.filters.min_sharpness = value >= 0.5,
            Self::OutputProminenceEnabled => c.filters.output_prominence = value >= 0.5,
            Self::AttackLevelEnabled => c.filters.attack_level = value >= 0.5,
            Self::MixRmsEnabled => c.filters.mix_rms = value >= 0.5,
            Self::SharpnessPercentileEnabled => c.filters.sharpness_percentile = value >= 0.5,
            Self::EnergyPercentileEnabled => c.filters.energy_percentile = value >= 0.5,
            Self::OutputIntervalEnabled => c.filters.output_interval = value >= 0.5,
            Self::RejectOverlap => c.filters.reject_overlap = value >= 0.5,
            Self::LowHz => c.low_hz = value.min(c.high_hz - 10.0),
            Self::HighHz => {
                c.high_hz = value.max(c.low_hz + 10.0);
                c.taper_hz = c.taper_hz.max(c.high_hz);
            }
            Self::TaperHz => c.taper_hz = value.max(c.high_hz),
            Self::FeatureMs => {
                c.feature_ms = value;
                c.concentration_ms = c.concentration_ms.min(value);
            }
            Self::ConcentrationMs => c.concentration_ms = value.min(c.feature_ms),
            Self::EnvelopeMs => c.envelope_ms = value,
            Self::BackgroundMs => c.background_ms = value,
            Self::MergeMs => c.merge_ms = value,
            Self::RearmMs => c.rearm_ms = value,
            Self::AdmissionEnergy => c.admission_energy = value,
            Self::AdmissionGrowth => c.admission_growth = value,
            Self::AdmissionProminence => c.admission_prominence = value,
            Self::MinSharpness => c.min_sharpness = value,
            Self::MinOutputProminence => c.min_output_prominence = value,
            Self::MinAttackDb => c.min_attack_db = value,
            Self::OutputRmsDb => c.output_rms_db = value,
            Self::SharpnessPercentile => c.sharpness_percentile = value,
            Self::EnergyPercentile => c.energy_percentile = value,
            Self::MemorySeconds => c.memory_seconds = value,
            Self::MinReference => c.min_reference = value.round() as usize,
            Self::OutputIntervalMs => c.output_interval_ms = value,
            Self::FftSize => s.fft_size = if value < 384.0 { 256 } else { 512 },
            Self::HopMs => s.hop_ms = value,
            Self::RecentMarkers => s.recent_markers = value.round() as usize,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RankInterval {
    pub lower: f32,
    pub upper: f32,
}

/// An admitted pair and its original output outcome. These points come from
/// the paired population, so rejected outputs remain visible as learned data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PercussionReferencePoint {
    pub candidate_id: u64,
    pub time: f64,
    pub sharpness: f32,
    pub energy: f32,
    pub eligible: bool,
    pub provisional: bool,
    pub overlapping: bool,
    /// Membership in the reference at snapshot time; expired additions remain
    /// available as recent markers but never contribute to the heatmap counts.
    pub in_reference: bool,
}

/// Saved gate values for the exact reference/configuration used at a decision.
/// Only the two effective feature bounds are drawable as a 2D acceptance area;
/// prominence, mix RMS and output interval are additional independent gates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PercussionThresholdSnapshot {
    pub filters: PercussionFilters,
    pub min_sharpness: f32,
    pub min_attack_db: f32,
    pub admission_energy: f32,
    pub admission_growth: f32,
    pub admission_prominence: f32,
    pub min_output_prominence: f32,
    pub output_rms_db: f32,
    pub output_interval_ms: f32,
    pub feature_seconds: f32,
    pub sharpness_effective_cutoff: f32,
    pub energy_effective_cutoff: f32,
    /// `None` means every feature gate on that axis is disabled.
    pub sharpness_effective_bound: Option<f32>,
    pub energy_effective_bound: Option<f32>,
    pub sharpness_tolerance: f32,
    pub energy_relative_tolerance: f32,
    pub energy_absolute_tolerance: f32,
    pub warmup: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PercussionReferenceSnapshot {
    pub count: usize,
    pub oldest_time: Option<f64>,
    pub newest_time: Option<f64>,
    pub effective_seconds: f32,
    pub capacity_truncated: bool,
    pub sharpness_histogram: [u32; HISTOGRAM_BINS],
    pub energy_histogram: [u32; HISTOGRAM_BINS],
    /// Exact paired counts, indexed [energy bin][sharpness bin].
    pub joint_histogram: [[u32; HEATMAP_BINS]; HEATMAP_BINS],
    /// Last admitted pairs in this epoch, ordered oldest to newest. `recent_count`
    /// bounds valid entries; display N is independent of this saved capacity.
    #[serde(
        serialize_with = "serialize_reference_points",
        deserialize_with = "deserialize_reference_points"
    )]
    pub recent_points: [PercussionReferencePoint; MAX_RECENT_MARKERS],
    pub recent_count: usize,
    pub thresholds: PercussionThresholdSnapshot,
    /// Plot energy axis is dBFS-equivalent fixed-horizon attack level.
    pub energy_min_db: f32,
    pub energy_max_db: f32,
    pub sharpness_cutoff: Option<f32>,
    pub energy_cutoff: Option<f32>,
}
impl Default for PercussionReferenceSnapshot {
    fn default() -> Self {
        Self {
            count: 0,
            oldest_time: None,
            newest_time: None,
            effective_seconds: 0.0,
            capacity_truncated: false,
            sharpness_histogram: [0; HISTOGRAM_BINS],
            energy_histogram: [0; HISTOGRAM_BINS],
            joint_histogram: [[0; HEATMAP_BINS]; HEATMAP_BINS],
            recent_points: [PercussionReferencePoint::default(); MAX_RECENT_MARKERS],
            recent_count: 0,
            thresholds: PercussionThresholdSnapshot::default(),
            energy_min_db: -80.0,
            energy_max_db: 0.0,
            sharpness_cutoff: None,
            energy_cutoff: None,
        }
    }
}

fn serialize_reference_points<S: serde::Serializer>(
    points: &[PercussionReferencePoint; MAX_RECENT_MARKERS],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    points.as_slice().serialize(serializer)
}

fn deserialize_reference_points<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<[PercussionReferencePoint; MAX_RECENT_MARKERS], D::Error> {
    let points = Vec::<PercussionReferencePoint>::deserialize(deserializer)?;
    points
        .try_into()
        .map_err(|points: Vec<PercussionReferencePoint>| {
            serde::de::Error::invalid_length(points.len(), &"64 saved reference points")
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecisionReason {
    Eligible,
    Provisional,
    Numerical,
    Overlap,
    NegligibleGrowth,
    AdmissionEnergy,
    Prominence,
    PhysicalSharpness,
    OutputProminence,
    AttackLevel,
    MixRms,
    SharpnessPercentile,
    EnergyPercentile,
    Refractory,
    Capacity,
}
impl DecisionReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::Provisional => "provisional warmup",
            Self::Numerical => "invalid numerical input",
            Self::Overlap => "overlapping feature windows",
            Self::NegligibleGrowth => "insufficient input rise",
            Self::AdmissionEnergy => "below input energy floor",
            Self::Prominence => "below input contrast floor",
            Self::PhysicalSharpness => "below sharpness floor",
            Self::OutputProminence => "below trigger contrast floor",
            Self::AttackLevel => "below trigger energy floor",
            Self::MixRms => "below mix level floor",
            Self::SharpnessPercentile => "below sharpness cutoff",
            Self::EnergyPercentile => "below energy cutoff",
            Self::Refractory => "trigger spacing",
            Self::Capacity => "bounded pending queue full",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnvelopePoint {
    pub seconds: f32,
    pub amplitude: f32,
    pub power: f32,
    /// Independently smoothed regional amplitudes before movement is combined.
    pub left_amplitude: f32,
    pub right_amplitude: f32,
    /// Combined positive side movement accumulated since this attack began.
    pub cumulative_growth: f32,
}

/// Immutable, pre-insertion evidence. The histogram is the reference that made
/// this decision, never a subsequent live distribution.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PercussionDecision {
    pub id: u64,
    /// Creation identity; `id` above is the monotonic publication cursor.
    pub candidate_id: u64,
    pub channel: usize,
    pub epoch: u64,
    pub onset_time: f64,
    pub completion_time: f64,
    pub emission_time: f64,
    pub sharpness: f32,
    pub energy: f32,
    pub absolute_energy: f32,
    pub growth: f32,
    pub prominence: f32,
    pub background_power: f32,
    /// Separate backgrounds frozen at the shared stereo attack boundary.
    pub background_stereo_power: [f32; 2],
    pub peak_amplitude: f32,
    pub mix_rms_db: f32,
    pub admitted: bool,
    pub eligible: bool,
    pub provisional: bool,
    /// The measurement shares source time with another candidate. Allowing
    /// overlaps keeps this diagnostic even when the overlap filter is off.
    pub overlapping: bool,
    pub rise_censored: bool,
    pub tolerance_used: bool,
    pub reason: DecisionReason,
    pub sharpness_rank: Option<RankInterval>,
    pub energy_rank: Option<RankInterval>,
    pub reference: PercussionReferenceSnapshot,
    #[serde(serialize_with = "serialize_envelope")]
    pub envelope: [EnvelopePoint; ENVELOPE_POINTS],
    pub envelope_len: usize,
    pub feature_seconds: f32,
    pub support_seconds: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct PercussionChannelStatus {
    pub envelope: f32,
    pub background_power: f32,
    pub available: bool,
    pub collecting: bool,
    pub epoch: u64,
    pub candidates: u64,
    pub admitted: u64,
    pub eligible: u64,
    pub reference: PercussionReferenceSnapshot,
}

fn serialize_envelope<S: serde::Serializer>(
    points: &[EnvelopePoint; ENVELOPE_POINTS],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    points.as_slice().serialize(serializer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_envelope_points_default_the_new_side_diagnostics() {
        let point: EnvelopePoint =
            serde_json::from_str(r#"{"seconds":0.001,"amplitude":0.2,"power":0.04}"#).unwrap();
        assert_eq!(point.left_amplitude, 0.0);
        assert_eq!(point.right_amplitude, 0.0);
        assert_eq!(point.cumulative_growth, 0.0);
        assert_eq!(point.amplitude, 0.2);
    }

    #[test]
    fn recent_marker_display_setting_defaults_round_trips_and_validates() {
        let mut settings: PercussionSettings = ron::from_str("()").unwrap();
        assert_eq!(settings.recent_markers, 32);
        PercussionSetting::RecentMarkers.set(&mut settings, 0, 64.0);
        settings.validate().unwrap();
        let restored: PercussionSettings =
            ron::from_str(&ron::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
        for count in [0, 3, MAX_RECENT_MARKERS + 1] {
            settings.recent_markers = count;
            assert!(settings.validate().is_err());
        }
    }

    #[test]
    fn filter_switches_default_on_and_round_trip_partial_channel_configs() {
        let defaults = PercussionSettings::default();
        assert!(PercussionSetting::CHANNEL
            .into_iter()
            .filter(|setting| setting.is_filter_toggle())
            .all(|setting| setting.get(&defaults, 0) == 1.0));
        let mut disabled = defaults;
        for setting in PercussionSetting::CHANNEL {
            if setting.is_filter_toggle() {
                setting.set(&mut disabled, 0, 0.0);
            }
        }
        disabled.validate().unwrap();
        assert_eq!(
            disabled.channels[0].admission_energy,
            defaults.channels[0].admission_energy
        );
        let restored: PercussionSettings =
            ron::from_str(&ron::to_string(&disabled).unwrap()).unwrap();
        assert_eq!(restored, disabled);
        let partial: PercussionSettings =
            ron::from_str("(channels:((filters:(input_energy:false)),(),()))").unwrap();
        assert!(!partial.channels[0].filters.input_energy);
        assert!(partial.channels[0].filters.input_growth);
        assert_eq!(partial.channels[1].filters, PercussionFilters::default());
        assert_eq!(partial.channels[0].feature_ms, 44.0);
        assert!(ron::from_str::<PercussionSettings>(
            "(channels:((filters:(unknown:false)),(),()))"
        )
        .is_err());
    }

    #[test]
    fn joint_reference_snapshot_round_trips_saved_points_and_gate_values() {
        let mut snapshot = PercussionReferenceSnapshot {
            count: 1,
            recent_count: 1,
            ..PercussionReferenceSnapshot::default()
        };
        snapshot.joint_histogram[3][5] = 1;
        snapshot.recent_points[0] = PercussionReferencePoint {
            candidate_id: 123,
            time: 2.5,
            sharpness: 0.2,
            energy: 1e-6,
            eligible: false,
            provisional: true,
            overlapping: false,
            in_reference: true,
        };
        snapshot.thresholds.feature_seconds = 0.044;
        snapshot.thresholds.sharpness_effective_cutoff = 0.225;
        snapshot.thresholds.warmup = true;
        let restored: PercussionReferenceSnapshot =
            serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
        assert_eq!(restored, snapshot);
        let restored: PercussionReferenceSnapshot =
            ron::from_str(&ron::to_string(&snapshot).unwrap()).unwrap();
        assert_eq!(restored, snapshot);
    }

    #[test]
    fn missing_channel_fields_preserve_instrument_masks_and_feature_defaults() {
        let expected = PercussionSettings::default();
        for source in ["()", "(channels:((),(),()))"] {
            let restored: PercussionSettings = ron::from_str(source).unwrap();
            assert_eq!(restored, expected);
            restored.validate().unwrap();
            assert_eq!(restored.channels[0].feature_ms, 44.0);
            assert_eq!(restored.channels[0].min_sharpness, 0.225);
            assert_eq!(restored.channels[0].min_output_prominence, 0.55);
            assert_eq!(restored.channels[1].low_hz, 2_000.0);
            assert_eq!(restored.channels[1].high_hz, 5_000.0);
            assert_eq!(restored.channels[1].taper_hz, 10_000.0);
            assert_eq!(restored.channels[2].low_hz, 5_000.0);
            assert_eq!(restored.channels[2].high_hz, 16_000.0);
            for channel in &restored.channels[1..] {
                assert_eq!(channel.feature_ms, 24.0);
                assert_eq!(channel.min_output_prominence, 0.16);
            }
        }
    }

    #[test]
    fn partial_channel_overrides_keep_other_defaults_and_round_trip_strictly() {
        let mut expected = PercussionSettings::default();
        expected.channels[0].energy_percentile = 75.0;
        expected.channels[1].sharpness_percentile = 82.0;
        expected.channels[2].output_interval_ms = 42.0;
        let restored: PercussionSettings = ron::from_str(
            "(channels:((energy_percentile:75.0),(sharpness_percentile:82.0),(output_interval_ms:42.0)))",
        ).unwrap();
        assert_eq!(restored, expected);
        for serialized in [
            ron::to_string(&restored).unwrap(),
            ron::ser::to_string_pretty(&restored, ron::ser::PrettyConfig::default()).unwrap(),
            ron::ser::to_string_pretty(
                &restored,
                ron::ser::PrettyConfig::default().struct_names(true),
            )
            .unwrap(),
        ] {
            assert_eq!(
                ron::from_str::<PercussionSettings>(&serialized).unwrap(),
                restored
            );
        }
        assert_eq!(
            serde_json::from_str::<PercussionSettings>(r#"{"channels":[{}, {}, {}]}"#).unwrap(),
            PercussionSettings::default()
        );
        assert!(ron::from_str::<PercussionSettings>("(channels:((unknown:1.0),(),()))").is_err());
        let invalid: PercussionSettings =
            ron::from_str("(channels:((feature_ms:NaN),(),()))").unwrap();
        assert!(
            invalid.validate().is_err(),
            "explicit malformed input must not be repaired by defaults"
        );
    }
}
