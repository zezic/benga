//! Live arrangement graphs and immutable percussion decision inspection.
//!
//! Percussion records come from the audio clock, including rejected candidates;
//! display refresh never reconstructs a decision against a newer reference.

use std::collections::{BTreeMap, VecDeque};

use vello::{
    kurbo::{Affine, BezPath, Circle, Line, Point, Rect, Stroke},
    peniko::{Color, Fill},
    Scene,
};

use crate::visualizer::{
    ArrangementSetting, DebugHistory, DebugSample, Detector, DropStatus, OnsetBinMeter, Parameter,
    PercussionDecision, PercussionReferencePoint, PercussionReferenceSnapshot, PercussionSetting,
    RankInterval, Visualizer,
};
use crate::{
    text::{BlockAlign, Fonts, Text, TextPaint, TextPlan, UiFace},
    theme::Theme,
};

use super::{ScenePalette, Stage};

const HISTORY_SECONDS: f32 = 8.0;
const SHAPE_PANEL_HEIGHT: f64 = 860.0;
const ONSET_PANEL_HEIGHT: f64 = 730.0;
const CONTROL_PANEL_HEIGHT: f64 = 730.0;
const HEATMAP_TOP: f64 = 260.0;
const HEATMAP_HEIGHT: f64 = 198.0;
const SPECTRUM_TOP: f64 = 540.0;
const BIN_EDITOR_TOP: f64 = 598.0;
const BINS_PER_PAGE: usize = 12;

#[derive(Clone, Copy)]
struct Sample {
    at: f32,
    counts: [u32; 4],
    armed: bool,
    distance: f32,
    trend: f32,
    rms_db: f32,
    envelope_db: f32,
    energy_delta_db: f32,
    mix_loss_threshold_db: f32,
    gap: DropStatus,
}

impl From<&DebugSample> for Sample {
    fn from(sample: &DebugSample) -> Self {
        Self {
            at: sample.at,
            counts: [
                sample.kick_onset.count,
                sample.snare_onset.count,
                sample.hat_onset.count,
                sample.drop_onset.count,
            ],
            armed: sample.drop_onset.armed,
            distance: sample.drop_status.shape_distance,
            trend: sample.drop_status.trend_change,
            rms_db: sample.drop_status.rms_db,
            envelope_db: sample.drop_status.envelope_db,
            energy_delta_db: sample.drop_status.energy_delta_db,
            mix_loss_threshold_db: sample.drop_status.mix_loss_threshold_db,
            gap: sample.drop_status,
        }
    }
}

struct Label {
    source: String,
    text: Text,
}

#[derive(Clone, Copy)]
struct Panel {
    rect: Rect,
    scale: f64,
}

struct Slider {
    parameter: Parameter,
    rect: Rect,
    track: Rect,
    unit: f64,
    value: f32,
    caption: Text,
    readout: Text,
    filter: Option<(Parameter, bool, Rect)>,
    toggle_only: bool,
}

impl Slider {
    fn value_at(&self, point: Point) -> f32 {
        self.parameter
            .from_fraction(((point.x - self.track.x0) / self.track.width()) as f32)
    }
}

struct HoverHint {
    source: &'static str,
    width: f32,
    text: Text,
    plan: TextPlan,
    rect: Rect,
    unit: f64,
}

struct BinSlider {
    parameter: Parameter,
    meter: OnsetBinMeter,
    rect: Rect,
    track: Rect,
    frequency: Text,
    unit: f64,
}

impl BinSlider {
    fn value_at(&self, point: Point) -> f32 {
        (1.0 - (point.y - self.track.y0) / self.track.height()).clamp(0.0, 1.0) as f32
    }
}

pub(crate) enum AnalysisInteraction {
    Handled,
    Parameter(Parameter, f32),
    ResetBins(Detector),
}

#[derive(Default)]
pub(super) struct AnalysisOverlay {
    samples: VecDeque<Sample>,
    shape: DropStatus,
    decisions: VecDeque<PercussionDecision>,
    reference: PercussionReferenceSnapshot,
    audio_time: f64,
    support_seconds: f32,
    inspected_id: Option<u64>,
    saved_reference: bool,
    hover_hint: Option<HoverHint>,
    recent_markers: usize,
    current_epoch: u64,
    labels: BTreeMap<&'static str, Label>,
    refresh: f32,
    left: Option<Panel>,
    right: Option<Panel>,
    gap_controls: Option<Panel>,
    onset_controls: Option<Panel>,
    selected: usize,
    sliders: Vec<Slider>,
    bins: Vec<OnsetBinMeter>,
    selected_bins: Vec<usize>,
    bin_sliders: Vec<BinSlider>,
    bin_page: usize,
    pointer: Option<Point>,
    drag: Option<Parameter>,
    no_audio: bool,
}

impl AnalysisOverlay {
    pub fn update(
        &mut self,
        dt: f32,
        history: &DebugHistory,
        fonts: &mut Fonts,
        stage: &Stage,
        visualizer: &Visualizer,
    ) {
        if history
            .latest()
            .is_some_and(|sample| self.samples.back().is_some_and(|last| sample.at < last.at))
        {
            self.samples.clear();
            self.inspected_id = None;
            self.saved_reference = false;
        }
        let after = self
            .samples
            .back()
            .map_or(f32::NEG_INFINITY, |sample| sample.at);
        for sample in history.samples().filter(|sample| sample.at > after) {
            self.samples.push_back(Sample::from(sample));
        }
        if let Some(latest) = history.latest() {
            self.shape = latest.drop_status;
            self.no_audio = false;
            while self
                .samples
                .front()
                .is_some_and(|sample| latest.at - sample.at > HISTORY_SECONDS)
            {
                self.samples.pop_front();
            }
        } else {
            self.samples.clear();
            self.shape = DropStatus::default();
            self.no_audio = true;
        }
        self.decisions.clear();
        self.decisions
            .extend(visualizer.percussion_decisions().iter().cloned());
        self.reference = visualizer.percussion_reference(self.selected);
        self.recent_markers = visualizer.detector_settings().percussion.recent_markers;
        self.current_epoch = visualizer.percussion_channel_status(self.selected).epoch;
        self.audio_time = visualizer.percussion_audio_time();
        self.support_seconds = visualizer.percussion_support_seconds()[self.selected];
        if self
            .inspected_id
            .is_some_and(|id| !self.decisions.iter().any(|event| event.id == id))
        {
            self.inspected_id = None;
        }
        let panels = anchored_panels(stage);
        self.left = panels.map(|p| p[0]);
        self.gap_controls = panels.map(|p| p[1]);
        self.onset_controls = panels.map(|p| p[2]);
        self.right = panels.map(|p| p[3]);
        self.update_sliders(fonts, visualizer, stage);
        self.update_bins(fonts, visualizer);
        self.update_hover_hint(fonts, stage);
        self.refresh -= dt;
        if self.refresh > 0.0 && !self.labels.is_empty() {
            return;
        }
        self.refresh = 0.1;
        for (key, source, size, face) in [
            ("shape_title", "SPECTRAL SHAPE", 15.0, UiFace::SemiBold),
            (
                "onset_title",
                "ONSET HEATMAP · REV 3",
                15.0,
                UiFace::SemiBold,
            ),
            ("shape_key", "LIVE  /  SLOW REFERENCE", 12.0, UiFace::Light),
            ("axis_low", "32 Hz", 11.0, UiFace::Light),
            ("axis_mid", "1 kHz", 11.0, UiFace::Light),
            ("axis_high", "16 kHz", 11.0, UiFace::Light),
            ("raw_axis_low", "20 Hz", 10.5, UiFace::Light),
            (
                "movement",
                "SHAPE / TREND / AREA · MULTIPLE OF REQUIREMENT",
                11.0,
                UiFace::Light,
            ),
            (
                "movement_key",
                "SHIFT SOLID · TREND DASH · AREA DOT · 1x = REQUIRED",
                9.0,
                UiFace::Light,
            ),
            ("movement_top", "4x", 9.0, UiFace::Light),
            ("movement_cut", "1x", 9.0, UiFace::Light),
            ("movement_zero", "0", 9.0, UiFace::Light),
            (
                "movement_hint",
                "TRACE VALUES ABOVE 4x ARE CLIPPED",
                8.7,
                UiFace::Light,
            ),
            (
                "timing_title",
                "SHAPE GAP PATH · OR LOSS / RMS PATHS ABOVE",
                11.0,
                UiFace::SemiBold,
            ),
            (
                "timing_key",
                "SHIFT AND (TREND OR AREA) AND ENERGY · SILENCE CAN BYPASS SHAPE",
                8.7,
                UiFace::Light,
            ),
            ("timing_shift", "SHIFT", 9.0, UiFace::Light),
            ("timing_trend", "TREND", 9.0, UiFace::Light),
            ("timing_area", "AREA", 9.0, UiFace::Light),
            ("timing_energy", "ENERGY", 9.0, UiFace::Light),
            ("timing_together", "OVERLAP", 9.0, UiFace::SemiBold),
            ("timing_hold", "REL HOLD", 9.0, UiFace::Light),
            (
                "return_title",
                "LF ENVELOPES AFTER GAP · dB",
                11.0,
                UiFace::SemiBold,
            ),
            (
                "gap_reference_group",
                "REFERENCE / SPECTRUM",
                10.0,
                UiFace::SemiBold,
            ),
            (
                "gap_shape_group",
                "SHAPE CONDITIONS",
                10.0,
                UiFace::SemiBold,
            ),
            (
                "gap_energy_group",
                "ENERGY LOSS / RMS GAP",
                10.0,
                UiFace::SemiBold,
            ),
            ("gap_timing_group", "TIMING / HOLD", 10.0, UiFace::SemiBold),
            ("gap_return_group", "BASS RETURN", 10.0, UiFace::SemiBold),
            (
                "energy",
                "MIX LOSS · INDEPENDENT ARMING · dB",
                11.0,
                UiFace::SemiBold,
            ),
            (
                "loss_key",
                "SOLID: LOSS · DASH: REQUIRED · TINT: ENERGY PASSES",
                9.0,
                UiFace::Light,
            ),
            (
                "level_title",
                "RMS GAP · INDEPENDENT ARMING · dBFS",
                11.0,
                UiFace::Light,
            ),
            (
                "level_key",
                "SOLID: RMS · DASH: GAP LIMIT · DOT: REFERENCE",
                9.0,
                UiFace::Light,
            ),
            ("loss_zero", "0", 9.0, UiFace::Light),
            ("level_bottom", "-80", 9.0, UiFace::Light),
            (
                "gap_controls",
                "GAP / DROP CONTROLS",
                14.0,
                UiFace::SemiBold,
            ),
            (
                "candidate_title",
                "CANDIDATES · FILTERED OUT / LEARNED ONLY / TRIGGERED",
                10.5,
                UiFace::Light,
            ),
            (
                "envelope_title",
                "ATTACK ENVELOPES · L SOLID / R DASH / COMBINED FAINT",
                10.5,
                UiFace::Light,
            ),
            (
                "heatmap_title",
                "SHARPNESS / EARLY ATTACK LEVEL",
                10.5,
                UiFace::SemiBold,
            ),
            ("heatmap_x_axis", "0", 10.0, UiFace::Light),
            ("heatmap_x_mid", "SHARPNESS  0.5", 10.0, UiFace::Light),
            ("heatmap_x_max", "1", 10.0, UiFace::Light),
            ("heatmap_high", "0", 10.0, UiFace::Light),
            ("heatmap_middle", "-40", 10.0, UiFace::Light),
            ("heatmap_low", "-80", 10.0, UiFace::Light),
            ("heatmap_unit", "dBFS", 9.0, UiFace::Light),
            (
                "heatmap_markers",
                "FILLED: ORIGINAL TRIGGER · HOLLOW: LEARNED · X: LATEST / INSPECTED",
                9.0,
                UiFace::Light,
            ),
            ("fewer_markers", "-", 12.0, UiFace::SemiBold),
            ("more_markers", "+", 12.0, UiFace::SemiBold),
            ("previous_event", "<", 11.0, UiFace::SemiBold),
            ("next_event", ">", 11.0, UiFace::SemiBold),
            ("latest_event", "LIVE", 10.5, UiFace::SemiBold),
            (
                "page_hint",
                "CHECK FIRST · THEN LEARN ACCEPTED INPUTS",
                9.0,
                UiFace::Light,
            ),
            (
                "spectrum_title",
                "SELECTED SPECTRUM / AFTER ATTENUATION",
                11.0,
                UiFace::SemiBold,
            ),
            (
                "spectrum_hint",
                "CLICK TO INSPECT BINS",
                10.5,
                UiFace::Light,
            ),
            (
                "bins_title",
                "INPUT BINS · AMPLITUDE GAIN | ONSET",
                11.0,
                UiFace::SemiBold,
            ),
            ("reset_bins", "RESET", 11.0, UiFace::SemiBold),
            ("previous_bins", "<", 13.0, UiFace::SemiBold),
            ("next_bins", ">", 13.0, UiFace::SemiBold),
            ("time_start", "-8 s", 11.0, UiFace::Light),
            ("time_end", "NOW", 11.0, UiFace::Light),
            (
                "switch_channel",
                "CLICK GRAPH TO SWITCH CHANNEL",
                11.0,
                UiFace::Light,
            ),
        ] {
            self.cache(fonts, key, source.to_owned(), size, face);
        }
        let armed = self.samples.back().is_some_and(|sample| sample.armed);
        self.cache(
            fonts,
            "shape_state",
            if self.no_audio {
                "WAITING FOR AUDIO".to_owned()
            } else if armed && self.shape.gap_cooldown_remaining > 0.0 {
                format!(
                    "GAP ARMED · DROP INTERVAL {:.2} s REMAINS",
                    self.shape.gap_cooldown_remaining
                )
            } else if armed {
                "GAP ARMED / WAITING FOR BASS HIT".to_owned()
            } else {
                "TRACKING ARRANGEMENT".to_owned()
            },
            12.0,
            UiFace::SemiBold,
        );
        self.cache(
            fonts,
            "bin_page",
            format!(
                "BINS {}–{} / {}",
                (self.bin_page * BINS_PER_PAGE + 1).min(self.selected_bins.len()),
                ((self.bin_page + 1) * BINS_PER_PAGE).min(self.selected_bins.len()),
                self.selected_bins.len()
            ),
            11.0,
            UiFace::Light,
        );
        self.cache(
            fonts,
            "shape_values",
            format!(
                "SHIFT {:02.0}%  TRENDS {:02.0}%  AREA {:02.0}%",
                self.shape.shape_distance * 100.0,
                self.shape.trend_change * 100.0,
                self.shape.changed_fraction * 100.0,
            ),
            12.0,
            UiFace::Light,
        );
        self.cache(
            fonts,
            "energy_values",
            if self.no_audio {
                "WAITING FOR AUDIO".to_owned()
            } else {
                let loss = -self.shape.energy_delta_db;
                let required = self.shape.mix_loss_threshold_db;
                format!(
                    "LOSS {loss:+.2} dB / NEED > {required:.2} dB · {}",
                    if loss > required { "PASS" } else { "WAIT" },
                )
            },
            11.0,
            UiFace::SemiBold,
        );
        self.cache(
            fonts,
            "loss_hold",
            if self.no_audio {
                "LOSS HOLD -- / -- ms".to_owned()
            } else {
                format!(
                    "LOSS HOLD {:.0} / {:.0} ms · {}",
                    self.shape.mix_loss_held_seconds * 1000.0,
                    self.shape.mix_loss_hold_seconds * 1000.0,
                    if armed {
                        "GAP ARMED"
                    } else if self.shape.mix_loss_condition_met {
                        "BUILDING"
                    } else {
                        "RESET"
                    }
                )
            },
            9.0,
            UiFace::SemiBold,
        );
        let (loss_min, loss_max) = self.loss_bounds();
        for (key, value) in [("loss_top", loss_max), ("loss_bottom", loss_min)] {
            let label = if value.fract().abs() < 0.001 {
                format!("{value:+.0}")
            } else {
                format!("{value:+.2}")
            };
            self.cache(fonts, key, label, 9.0, UiFace::Light);
        }
        self.cache(
            fonts,
            "level_top",
            format!("{:.0}", self.level_ceiling()),
            9.0,
            UiFace::Light,
        );
        self.cache(
            fonts,
            "loss_state",
            if self.no_audio {
                "LOSS IS MEASURED AGAINST THE LEARNED REFERENCE".to_owned()
            } else if armed && self.shape.gap_cooldown_remaining > 0.0 {
                format!(
                    "GAP ARMED · DROP INTERVAL {:.2} s REMAINS",
                    self.shape.gap_cooldown_remaining
                )
            } else if armed {
                "GAP CONFIRMED · WAITING FOR AN AUDIBLE BASS RETURN".to_owned()
            } else if self.shape.mix_loss_condition_met {
                "LOSS-ONLY HOLD BUILDING · SHAPE CHANGE NOT REQUIRED".to_owned()
            } else {
                "LOSS HOLD RESET · RMS PATH CAN STILL ARM THE GAP".to_owned()
            },
            8.7,
            UiFace::Light,
        );
        self.cache(
            fonts,
            "reference_values",
            if self.no_audio {
                "RMS -- / GAP LIMIT -- dBFS".to_owned()
            } else {
                format!(
                    "RMS {:.1} / NEED < {:.1} dBFS · {}",
                    self.shape.rms_db,
                    self.shape.gap_rms_threshold_db,
                    if self.shape.gap_rms_condition_met {
                        "LOW"
                    } else {
                        "WAIT"
                    },
                )
            },
            9.0,
            UiFace::Light,
        );
        self.cache(
            fonts,
            "rms_gap_hold",
            if self.no_audio {
                "RMS HOLD -- / -- ms".to_owned()
            } else {
                format!(
                    "RMS HOLD {:.0} / {:.0} ms · {}",
                    self.shape.gap_rms_held_seconds * 1000.0,
                    self.shape.gap_rms_hold_seconds * 1000.0,
                    if armed {
                        "GAP ARMED"
                    } else if self.shape.gap_rms_condition_met {
                        "BUILDING"
                    } else {
                        "RESET"
                    }
                )
            },
            9.0,
            UiFace::SemiBold,
        );
        self.cache(
            fonts,
            "return_key",
            format!(
                "FAST {:.0} ms SOLID · SLOW {:.0} ms DOT · REQUIRED DASH",
                self.shape.bass_fast_seconds * 1000.0,
                self.shape.bass_slow_seconds * 1000.0
            ),
            8.7,
            UiFace::Light,
        );
        self.cache(
            fonts,
            "hit_values",
            format!(
                "FAST {:.1} / SLOW {:.1} dB · RISE {:.0}% / NEED {:.0}%",
                self.shape.bass_fast_db,
                self.shape.bass_slow_db,
                self.shape.bass_hit * 100.0,
                self.shape.hit_threshold * 100.0,
            ),
            9.0,
            UiFace::Light,
        );
        let (bass_low, bass_high) = self.bass_bounds();
        for (key, value) in [("bass_low", bass_low), ("bass_high", bass_high)] {
            self.cache(fonts, key, format!("{value:.0}"), 9.0, UiFace::Light);
        }
        self.cache(
            fonts,
            "return_rms",
            format!(
                "RMS {:.1} / MIN {:.1} dBFS · {}{}",
                self.shape.rms_db,
                self.shape.drop_rms_threshold_db,
                if self.shape.rms_db >= self.shape.drop_rms_threshold_db {
                    "PASS"
                } else {
                    "WAIT"
                },
                if !armed {
                    ""
                } else if self.shape.bass_onset_ready {
                    " · LF RISE READY"
                } else {
                    " · WAIT NEW LF RISE"
                },
            ),
            9.0,
            UiFace::Light,
        );
        self.cache(
            fonts,
            "hold_values",
            if self.no_audio {
                "WAITING FOR AUDIO".to_owned()
            } else {
                format!(
                    "RELATIVE HOLD {:.0} / {:.0} ms · {}",
                    self.shape.gap_held_seconds * 1000.0,
                    self.shape.gap_hold_seconds * 1000.0,
                    if armed {
                        "GAP ARMED"
                    } else if self.shape.gap_held_seconds >= self.shape.gap_hold_seconds {
                        "COMPLETE"
                    } else if self.shape.gap_conditions_met {
                        "BUILDING"
                    } else {
                        "RESET"
                    }
                )
            },
            10.0,
            UiFace::SemiBold,
        );
        self.cache(
            fonts,
            "timing_state",
            if self.no_audio {
                "SHAPE OVERLAP, LOSS HOLD OR RMS HOLD CAN ARM THE GAP".to_owned()
            } else if armed && self.shape.gap_cooldown_remaining > 0.0 {
                format!(
                    "GAP ARMED · DROP INTERVAL {:.2} s REMAINS",
                    self.shape.gap_cooldown_remaining
                )
            } else if armed {
                "GAP CONFIRMED · WAITING FOR BASS RETURN".to_owned()
            } else if self.shape.gap_rms_condition_met {
                "RMS PATH BUILDING · SPECTRAL CHANGES NOT REQUIRED".to_owned()
            } else if self.shape.gap_conditions_met {
                if self.shape.gap_cooldown_remaining > 0.0 {
                    format!(
                        "LOSS HOLD CAN ARM · DROP INTERVAL {:.2} s REMAINS",
                        self.shape.gap_cooldown_remaining
                    )
                } else {
                    "SHAPE + LOSS OVERLAP · EITHER HOLD CAN ARM".to_owned()
                }
            } else if self.shape.mix_loss_condition_met {
                "LOSS PATH BUILDING · SPECTRAL CHANGES NOT REQUIRED".to_owned()
            } else if !self.shape.gap_history_ready {
                "SHAPE PATH LEARNING · LOSS AND RMS HOLDS ALSO AVAILABLE".to_owned()
            } else {
                let shape_passes = self.shape.shape_distance > self.shape.shape_distance_threshold
                    && (self.shape.trend_change > self.shape.trend_change_threshold
                        || self.shape.changed_fraction >= self.shape.changed_fraction_threshold);
                let energy_passes = self.shape.energy_delta_db < -self.shape.mix_loss_threshold_db;
                match (shape_passes, energy_passes) {
                    (true, false) => "RELATIVE PATH: SHAPE PASSES · WAITING FOR ENERGY LOSS",
                    (false, true) => "LOSS PASSES · ITS OWN HOLD CAN ARM WITHOUT SHAPE",
                    _ => "SHAPE OVERLAP, LOSS HOLD OR RMS HOLD CAN ARM THE GAP",
                }
                .to_owned()
            },
            9.0,
            UiFace::Light,
        );
        let selected = self.selected_detector();
        self.cache(
            fonts,
            "selected_channel",
            selected.name().to_owned(),
            14.0,
            UiFace::SemiBold,
        );
        self.cache(
            fonts,
            "onset_controls",
            format!("{} · DETECTION PIPELINE", selected.name()),
            14.0,
            UiFace::SemiBold,
        );
        let reference = self.viewed_reference();
        let gate = if self.selected_bins.is_empty() {
            "REGION UNAVAILABLE".to_owned()
        } else if self.reference.count
            < visualizer.detector_settings().percussion.channels[self.selected].min_reference
        {
            format!("LIVE WARMUP · N={}", self.reference.count)
        } else {
            format!("LIVE REFERENCE · N={}", self.reference.count)
        };
        self.cache(fonts, "gate", gate, 11.0, UiFace::SemiBold);
        let latest = self.viewed_decision().cloned();
        let (decision, ranks, population, timing) = if let Some(event) = latest {
            let state = if event.eligible {
                if event.provisional {
                    "PROVISIONAL HIT"
                } else {
                    "TRIGGERED"
                }
            } else if event.admitted {
                "LEARNED"
            } else {
                "REJECTED"
            };
            let decision = format!(
                "{} · {}{}",
                state,
                decision_caption(event.reason),
                if event.tolerance_used {
                    " · tie tolerance"
                } else {
                    ""
                }
            );
            let ranks = format!(
                "LAST CHECK · RANK BEFORE LEARNING  SHARP {}  ENERGY {}{}",
                rank_label(event.sharpness_rank),
                rank_label(event.energy_rank),
                if event.rise_censored {
                    " · rise incomplete"
                } else {
                    ""
                }
            );
            let population = if !self.saved_reference {
                format!(
                    "LIVE AUDIO REFERENCE · N={} · {:.1} s{}",
                    reference.count,
                    reference.effective_seconds,
                    if reference.capacity_truncated {
                        " · capped"
                    } else {
                        ""
                    }
                )
            } else {
                format!(
                    "BEFORE LEARNING EVENT #{} · N={} · {:.1} s{}",
                    event.id,
                    reference.count,
                    reference.effective_seconds,
                    if reference.capacity_truncated {
                        " · capped"
                    } else {
                        ""
                    }
                )
            };
            let timing = format!(
                "START {:.3} s · FEATURES +{:.1} ms · {} +{:.1} ms",
                event.onset_time,
                (event.completion_time - event.onset_time) * 1000.0,
                if event.eligible { "EMIT" } else { "DECISION" },
                (event.emission_time - event.onset_time) * 1000.0
            );
            (decision, ranks, population, timing)
        } else {
            (
                "WAITING FOR A REGIONAL ATTACK".to_owned(),
                "LEARNING INITIAL ATTACKS · RANKS NOT READY".to_owned(),
                format!(
                    "LIVE REFERENCE · N={} · {:.1} s",
                    reference.count, reference.effective_seconds
                ),
                "FEATURE WINDOW START / COMPLETION / EMISSION USE AUDIO TIME".to_owned(),
            )
        };
        self.cache(fonts, "decision", decision, 10.5, UiFace::SemiBold);
        self.cache(fonts, "ranks", ranks, 10.0, UiFace::Light);
        self.cache(fonts, "population", population, 10.0, UiFace::Light);
        self.cache(fonts, "event_timing", timing, 9.5, UiFace::Light);
        let reference = self.viewed_reference();
        let shown = reference.recent_count.min(self.recent_markers);
        let percentile_state = if self.audio_time <= 0.0 {
            "WAITING FOR AUDIO"
        } else if reference.thresholds.warmup {
            "WARMUP · NO PERCENTILE CUTS"
        } else if reference.sharpness_cutoff.is_none() && reference.energy_cutoff.is_none() {
            "PERCENTILES OFF"
        } else if reference.sharpness_cutoff.is_none() {
            "SHARPNESS PERCENTILE OFF"
        } else if reference.energy_cutoff.is_none() {
            "ENERGY PERCENTILE OFF"
        } else {
            "SOLID: PERCENTILE CUTS"
        };
        let output_floors = match (
            reference.thresholds.filters.min_sharpness,
            reference.thresholds.filters.attack_level,
        ) {
            (true, true) => "DASH: TRIGGER FLOORS",
            (true, false) => "DASH: SHARPNESS FLOOR",
            (false, true) => "DASH: ENERGY FLOOR",
            (false, false) => "FIXED FLOORS OFF",
        };
        let input_floor = if reference.thresholds.filters.input_energy {
            "DOT: INPUT ENERGY"
        } else {
            "INPUT ENERGY OFF"
        };
        self.cache(
            fonts,
            "heatmap_cutoffs",
            format!("{percentile_state} · {output_floors} · {input_floor}"),
            8.6,
            UiFace::Light,
        );
        let clustered = self
            .heatmap_bounds()
            .and_then(|plot| heatmap_marker_cluster(plot, &reference, self.recent_markers));
        self.cache(
            fonts,
            "heatmap_cluster",
            clustered.map_or_else(String::new, |(_, count)| format!("x{count} HERE")),
            9.5,
            UiFace::SemiBold,
        );
        self.cache(
            fonts,
            "recent_markers",
            format!("LAST {shown}/{}", self.recent_markers),
            10.0,
            UiFace::SemiBold,
        );
        let filters = reference.thresholds.filters;
        let other_filters = filters.input_growth
            || filters.input_prominence
            || filters.reject_overlap
            || filters.output_prominence
            || filters.mix_rms
            || filters.output_interval;
        self.cache(
            fonts,
            "heatmap_gates",
            if other_filters {
                "SHADE: SHARPNESS + ENERGY ONLY · OTHER ENABLED FILTERS STILL APPLY"
            } else {
                "SHADE: SHARPNESS + ENERGY · NO OTHER OPTIONAL FILTERS ENABLED"
            }
            .to_owned(),
            8.6,
            UiFace::Light,
        );
        self.cache(
            fonts,
            "heatmap_recent",
            "FAINT: EXPIRED / EXCLUDED FROM DENSITY · COINCIDENT MARKERS OVERLAP".to_owned(),
            8.3,
            UiFace::Light,
        );
        let resolution = self
            .bins
            .windows(2)
            .next()
            .map_or(0.0, |pair| pair[1].frequency_hz - pair[0].frequency_hz);
        self.cache(
            fonts,
            "resolution",
            format!(
                "POWER MASK · {:.1} Hz BINS · {:.1} ms WINDOW",
                resolution,
                self.support_seconds * 1000.0
            ),
            10.0,
            UiFace::Light,
        );
    }

    fn viewed_decision(&self) -> Option<&PercussionDecision> {
        self.inspected_id
            .and_then(|id| {
                self.decisions
                    .iter()
                    .find(|event| event.id == id && event.channel == self.selected)
            })
            .or_else(|| {
                self.decisions
                    .iter()
                    .rev()
                    .find(|event| event.channel == self.selected)
            })
    }

    fn heatmap_candidate(&self) -> Option<&PercussionDecision> {
        self.viewed_decision()
            .filter(|event| self.saved_reference || event.epoch == self.current_epoch)
    }

    fn viewed_reference(&self) -> PercussionReferenceSnapshot {
        if !self.saved_reference {
            self.reference
        } else {
            self.viewed_decision()
                .map_or(self.reference, |event| event.reference)
        }
    }

    fn cache(
        &mut self,
        fonts: &mut Fonts,
        key: &'static str,
        source: String,
        size: f32,
        face: UiFace,
    ) {
        if self
            .labels
            .get(key)
            .is_some_and(|label| label.source == source)
        {
            return;
        }
        let text = fonts.layout_ui(&source, size, face);
        self.labels.insert(key, Label { source, text });
    }

    fn update_sliders(&mut self, fonts: &mut Fonts, visualizer: &Visualizer, stage: &Stage) {
        let mut index = 0;
        if let Some(panel) = self.gap_controls {
            for setting in ArrangementSetting::ALL {
                let top = match setting {
                    ArrangementSetting::ReferenceTime => 48.0,
                    ArrangementSetting::ShapeDistance => 175.0,
                    ArrangementSetting::TrendChange => 205.0,
                    ArrangementSetting::ChangedFraction => 235.0,
                    ArrangementSetting::MixLossDb => 296.0,
                    ArrangementSetting::MixLossHold => 330.0,
                    ArrangementSetting::GapRmsDb => 446.0,
                    ArrangementSetting::GapRmsHold => 480.0,
                    ArrangementSetting::GapHold => 558.0,
                    ArrangementSetting::DropRefractory => 602.0,
                    ArrangementSetting::BassFastTime => 724.0,
                    ArrangementSetting::BassSlowTime => 754.0,
                    ArrangementSetting::BassHitThreshold => 784.0,
                    ArrangementSetting::DropMinRmsDb => 822.0,
                };
                self.update_slider(
                    index,
                    top,
                    30.0,
                    panel,
                    Parameter::Arrangement(setting),
                    fonts,
                    visualizer,
                    stage,
                );
                index += 1;
            }
        }
        if let Some(panel) = self.onset_controls {
            let detector = self.selected_detector();
            let mut top = 28.0;
            for (key, title, settings) in percussion_control_groups() {
                self.cache(fonts, key, title.to_owned(), 10.0, UiFace::SemiBold);
                top += 18.0;
                for &setting in settings {
                    self.update_slider(
                        index,
                        top,
                        24.0,
                        panel,
                        Parameter::Percussion(detector, setting),
                        fonts,
                        visualizer,
                        stage,
                    );
                    index += 1;
                    top += 24.0;
                }
            }
        }
        self.sliders.truncate(index);
    }

    #[allow(clippy::too_many_arguments)]
    fn update_slider(
        &mut self,
        index: usize,
        top: f64,
        row_height: f64,
        panel: Panel,
        parameter: Parameter,
        fonts: &mut Fonts,
        visualizer: &Visualizer,
        stage: &Stage,
    ) {
        let unit = stage.unit * panel.scale;
        let top = panel.rect.y0 + top * unit;
        let rect = Rect::new(
            panel.rect.x0 + 8.0 * unit,
            top,
            panel.rect.x1 - 8.0 * unit,
            top + (row_height - 1.0) * unit,
        );
        let track = Rect::new(
            rect.x0 + 3.0 * unit,
            top + 19.0 * unit,
            rect.x1 - 3.0 * unit,
            top + 19.0 * unit,
        );
        let value = visualizer.parameter_value(parameter);
        let toggle_only =
            matches!(parameter, Parameter::Percussion(_, setting) if setting.is_filter_toggle());
        let filter_parameter = match parameter {
            Parameter::Percussion(detector, setting) if setting.is_filter_toggle() => {
                Some(Parameter::Percussion(detector, setting))
            }
            Parameter::Percussion(detector, setting) => setting
                .filter_toggle()
                .map(|toggle| Parameter::Percussion(detector, toggle)),
            _ => None,
        };
        let filter = filter_parameter.map(|toggle| {
            (
                toggle,
                visualizer.parameter_value(toggle) >= 0.5,
                Rect::new(
                    rect.x0,
                    top + 3.0 * unit,
                    rect.x0 + 10.0 * unit,
                    top + 13.0 * unit,
                ),
            )
        });
        if self
            .sliders
            .get(index)
            .is_none_or(|slider| slider.parameter != parameter)
        {
            let slider = Slider {
                parameter,
                rect,
                track,
                unit,
                value,
                caption: fonts.layout_ui(
                    slider_caption(parameter),
                    if row_height < 30.0 { 10.7 } else { 12.0 },
                    UiFace::Light,
                ),
                readout: fonts.layout_ui(
                    &parameter.format(value),
                    if row_height < 30.0 { 10.7 } else { 12.0 },
                    UiFace::SemiBold,
                ),
                filter,
                toggle_only,
            };
            if index < self.sliders.len() {
                self.sliders[index] = slider;
            } else {
                self.sliders.push(slider);
            }
        } else {
            let slider = &mut self.sliders[index];
            slider.rect = rect;
            slider.track = track;
            slider.unit = unit;
            slider.filter = filter;
            slider.toggle_only = toggle_only;
            if slider.value != value {
                slider.value = value;
                slider.readout = fonts.layout_ui(
                    &parameter.format(value),
                    if row_height < 30.0 { 10.7 } else { 12.0 },
                    UiFace::SemiBold,
                );
            }
        }
    }

    fn update_bins(&mut self, fonts: &mut Fonts, visualizer: &Visualizer) {
        self.bins.clear();
        self.bins
            .extend_from_slice(visualizer.onset_bins(self.selected_detector()));
        self.selected_bins.clear();
        self.selected_bins.extend(
            self.bins
                .iter()
                .enumerate()
                .filter_map(|(index, bin)| (bin.selected > 0.0).then_some(index)),
        );
        self.bin_page = self
            .bin_page
            .min(self.selected_bins.len().saturating_sub(1) / BINS_PER_PAGE);
        let Some(panel) = self.right else {
            self.bin_sliders.clear();
            return;
        };
        let unit = panel.rect.height() / ONSET_PANEL_HEIGHT;
        let start = self.bin_page * BINS_PER_PAGE;
        let count = self
            .selected_bins
            .len()
            .saturating_sub(start)
            .min(BINS_PER_PAGE);
        let cell_width = (panel.rect.width() - 8.0 * unit) / count.max(1) as f64;
        for slot in 0..count {
            let meter = self.bins[self.selected_bins[start + slot]];
            let parameter = Parameter::OnsetBinGain(self.selected_detector(), meter.address);
            let x = panel.rect.x0 + 4.0 * unit + slot as f64 * cell_width;
            let rect = Rect::new(
                x,
                panel.rect.y0 + 622.0 * unit,
                x + cell_width,
                panel.rect.y0 + 705.0 * unit,
            );
            let track = Rect::new(
                x + cell_width * 0.34,
                panel.rect.y0 + 628.0 * unit,
                x + cell_width * 0.34,
                panel.rect.y0 + 682.0 * unit,
            );
            if self
                .bin_sliders
                .get(slot)
                .is_none_or(|slider| slider.parameter != parameter)
            {
                let slider = BinSlider {
                    parameter,
                    meter,
                    rect,
                    track,
                    unit,
                    frequency: fonts.layout_ui(
                        &frequency_label(meter.frequency_hz),
                        11.0,
                        UiFace::Light,
                    ),
                };
                if slot < self.bin_sliders.len() {
                    self.bin_sliders[slot] = slider;
                } else {
                    self.bin_sliders.push(slider);
                }
            } else {
                let slider = &mut self.bin_sliders[slot];
                slider.meter = meter;
                slider.rect = rect;
                slider.track = track;
                slider.unit = unit;
            }
        }
        self.bin_sliders.truncate(count);
        let detail = self
            .pointer
            .and_then(|point| {
                self.bin_sliders
                    .iter()
                    .find(|slider| slider.rect.contains(point))
            })
            .map_or_else(
                || "HOVER A BIN FOR ITS RANGE".to_owned(),
                |slider| {
                    format!(
                        "{:.0}–{:.0} Hz · GAIN {:.0}% · SELECTED {:.0}%",
                        slider.meter.low_hz,
                        slider.meter.high_hz,
                        slider.meter.gain * 100.0,
                        slider.meter.selected * 100.0
                    )
                },
            );
        self.cache(fonts, "bin_detail", detail, 11.0, UiFace::Light);
        let selection = self
            .pointer
            .and_then(|point| {
                self.bin_sliders
                    .iter()
                    .find(|slider| slider.rect.contains(point))
            })
            .map_or_else(
                || "DRAG FADERS TO ATTENUATE".to_owned(),
                |slider| format!("{:.0}% OF THIS BIN SELECTED", slider.meter.selected * 100.0),
            );
        self.cache(fonts, "bin_selection", selection, 11.0, UiFace::Light);
    }

    pub fn selected_detector(&self) -> Detector {
        [Detector::Kick, Detector::Snare, Detector::Hat][self.selected]
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn contains(&self, point: Point) -> bool {
        [
            self.left,
            self.right,
            self.gap_controls,
            self.onset_controls,
        ]
        .into_iter()
        .flatten()
        .any(|panel| panel.rect.contains(point))
    }

    pub fn onset_graph_bounds(&self) -> Option<Rect> {
        self.right.map(|panel| {
            let unit = panel.rect.height() / ONSET_PANEL_HEIGHT;
            Rect::new(
                panel.rect.x0,
                panel.rect.y0 + 80.0 * unit,
                panel.rect.x1,
                panel.rect.y0 + 111.0 * unit,
            )
        })
    }

    fn event_buttons(&self) -> Option<[Rect; 3]> {
        self.right.map(|panel| {
            let unit = panel.rect.height() / ONSET_PANEL_HEIGHT;
            [
                Rect::new(
                    panel.rect.x1 - 78.0 * unit,
                    panel.rect.y0 + 115.0 * unit,
                    panel.rect.x1 - 56.0 * unit,
                    panel.rect.y0 + 133.0 * unit,
                ),
                Rect::new(
                    panel.rect.x1 - 54.0 * unit,
                    panel.rect.y0 + 115.0 * unit,
                    panel.rect.x1 - 32.0 * unit,
                    panel.rect.y0 + 133.0 * unit,
                ),
                Rect::new(
                    panel.rect.x1 - 30.0 * unit,
                    panel.rect.y0 + 115.0 * unit,
                    panel.rect.x1,
                    panel.rect.y0 + 133.0 * unit,
                ),
            ]
        })
    }

    fn heatmap_bounds(&self) -> Option<Rect> {
        self.right.map(|panel| {
            let unit = panel.rect.height() / ONSET_PANEL_HEIGHT;
            Rect::new(
                panel.rect.x0 + 34.0 * unit,
                panel.rect.y0 + HEATMAP_TOP * unit,
                panel.rect.x1 - 4.0 * unit,
                panel.rect.y0 + (HEATMAP_TOP + HEATMAP_HEIGHT) * unit,
            )
        })
    }

    fn marker_count_buttons(&self) -> Option<[Rect; 2]> {
        self.right.map(|panel| {
            let unit = panel.rect.height() / ONSET_PANEL_HEIGHT;
            [
                Rect::new(
                    panel.rect.x1 - 38.0 * unit,
                    panel.rect.y0 + 234.0 * unit,
                    panel.rect.x1 - 20.0 * unit,
                    panel.rect.y0 + 252.0 * unit,
                ),
                Rect::new(
                    panel.rect.x1 - 18.0 * unit,
                    panel.rect.y0 + 234.0 * unit,
                    panel.rect.x1,
                    panel.rect.y0 + 252.0 * unit,
                ),
            ]
        })
    }

    fn spectrum_bounds(&self) -> Option<Rect> {
        self.right.map(|panel| {
            let unit = panel.rect.height() / ONSET_PANEL_HEIGHT;
            Rect::new(
                panel.rect.x0,
                panel.rect.y0 + SPECTRUM_TOP * unit,
                panel.rect.x1,
                panel.rect.y0 + 594.0 * unit,
            )
        })
    }

    fn bin_button_bounds(&self) -> Option<[Rect; 3]> {
        self.right.map(|panel| {
            let unit = panel.rect.height() / ONSET_PANEL_HEIGHT;
            [
                Rect::new(
                    panel.rect.x1 - 54.0 * unit,
                    panel.rect.y0 + 599.0 * unit,
                    panel.rect.x1,
                    panel.rect.y0 + 619.0 * unit,
                ),
                Rect::new(
                    panel.rect.x0,
                    panel.rect.y0 + 710.0 * unit,
                    panel.rect.x0 + 28.0 * unit,
                    panel.rect.y0 + 729.0 * unit,
                ),
                Rect::new(
                    panel.rect.x1 - 28.0 * unit,
                    panel.rect.y0 + 710.0 * unit,
                    panel.rect.x1,
                    panel.rect.y0 + 729.0 * unit,
                ),
            ]
        })
    }

    pub fn pointer_down(&mut self, point: Point) -> Option<AnalysisInteraction> {
        self.pointer = Some(point);
        self.drag = None;
        if let Some((parameter, enabled, _)) = self
            .sliders
            .iter()
            .filter_map(|slider| slider.filter)
            .find(|(_, _, rect)| rect.inflate(4.0, 4.0).contains(point))
        {
            return Some(AnalysisInteraction::Parameter(
                parameter,
                if enabled { 0.0 } else { 1.0 },
            ));
        }
        if let Some(buttons) = self.marker_count_buttons() {
            if let Some(button) = buttons.iter().position(|button| button.contains(point)) {
                let value = if button == 0 {
                    self.recent_markers.saturating_sub(4)
                } else {
                    self.recent_markers + 4
                };
                return Some(AnalysisInteraction::Parameter(
                    Parameter::Percussion(
                        self.selected_detector(),
                        PercussionSetting::RecentMarkers,
                    ),
                    value.clamp(4, 64) as f32,
                ));
            }
        }
        if let Some(buttons) = self.event_buttons() {
            if let Some(button) = buttons.iter().position(|button| button.contains(point)) {
                let events: Vec<_> = self
                    .decisions
                    .iter()
                    .filter(|event| event.channel == self.selected)
                    .map(|event| event.id)
                    .collect();
                if button == 2 {
                    self.inspected_id = None;
                    self.saved_reference = false;
                } else if !events.is_empty() {
                    self.saved_reference = true;
                    let current = self
                        .inspected_id
                        .and_then(|id| events.iter().position(|&saved| saved == id))
                        .unwrap_or(events.len() - 1);
                    let index = if button == 0 {
                        current.saturating_sub(1)
                    } else {
                        (current + 1).min(events.len() - 1)
                    };
                    self.inspected_id = Some(events[index]);
                }
                self.refresh = 0.0;
                return Some(AnalysisInteraction::Handled);
            }
        }
        if let Some(slider) = self.bin_sliders.iter().find(|slider| {
            slider
                .track
                .inflate(5.0 * slider.unit, 5.0 * slider.unit)
                .contains(point)
        }) {
            let value = slider.value_at(point);
            self.drag = Some(slider.parameter);
            return Some(AnalysisInteraction::Parameter(slider.parameter, value));
        }
        if let Some([reset, previous, next]) = self.bin_button_bounds() {
            if reset.contains(point) {
                return Some(AnalysisInteraction::ResetBins(self.selected_detector()));
            }
            if previous.contains(point) {
                self.bin_page = self.bin_page.saturating_sub(1);
                self.refresh = 0.0;
                return Some(AnalysisInteraction::Handled);
            }
            if next.contains(point) {
                self.bin_page = (self.bin_page + 1)
                    .min(self.selected_bins.len().saturating_sub(1) / BINS_PER_PAGE);
                self.refresh = 0.0;
                return Some(AnalysisInteraction::Handled);
            }
        }
        if let Some(plot) = self.spectrum_bounds().filter(|plot| plot.contains(point)) {
            let target = spectrum_frequency(((point.x - plot.x0) / plot.width()) as f32);
            if let Some(index) = self
                .selected_bins
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    (self.bins[**a].frequency_hz - target)
                        .abs()
                        .total_cmp(&(self.bins[**b].frequency_hz - target).abs())
                })
                .map(|(index, _)| index)
            {
                self.bin_page = index / BINS_PER_PAGE;
                self.refresh = 0.0;
            }
            return Some(AnalysisInteraction::Handled);
        }
        if let Some(slider) = self.sliders.iter().find(|slider| {
            !slider.toggle_only
                && slider.filter.is_none_or(|(_, enabled, _)| enabled)
                && slider
                    .track
                    .inflate(4.0 * slider.unit, 7.0 * slider.unit)
                    .contains(point)
        }) {
            let value = slider.value_at(point);
            self.drag = Some(slider.parameter);
            return Some(AnalysisInteraction::Parameter(slider.parameter, value));
        }
        if self
            .onset_graph_bounds()
            .is_some_and(|plot| plot.contains(point))
        {
            self.selected = (self.selected + 1) % 3;
            self.inspected_id = None;
            self.bin_page = 0;
            self.refresh = 0.0;
            return Some(AnalysisInteraction::Handled);
        }
        self.contains(point).then_some(AnalysisInteraction::Handled)
    }

    pub fn pointer_move(&mut self, point: Option<Point>) -> Option<(Parameter, f32)> {
        self.pointer = point;
        let Some(point) = point else {
            self.drag = None;
            return None;
        };
        let parameter = self.drag?;
        if let Some(slider) = self
            .bin_sliders
            .iter()
            .find(|slider| slider.parameter == parameter)
        {
            return Some((parameter, slider.value_at(point)));
        }
        self.sliders
            .iter()
            .find(|slider| slider.parameter == parameter)
            .map(|slider| (parameter, slider.value_at(point)))
    }

    pub fn pointer_up(&mut self) {
        self.drag = None;
    }

    pub fn cancel_interaction(&mut self) {
        self.drag = None;
        self.pointer = None;
    }

    #[cfg(test)]
    pub fn slider_center(&self, parameter: Parameter) -> Option<Point> {
        if let Some((_, _, rect)) = self
            .sliders
            .iter()
            .filter_map(|slider| slider.filter)
            .find(|(toggle, _, _)| *toggle == parameter)
        {
            return Some(rect.center());
        }
        self.sliders
            .iter()
            .find(|slider| slider.parameter == parameter)
            .map(|slider| slider.track.center())
            .or_else(|| {
                self.bin_sliders
                    .iter()
                    .find(|slider| slider.parameter == parameter)
                    .map(|slider| slider.track.center())
            })
    }

    #[cfg(test)]
    pub fn first_bin_parameter(&self) -> Option<Parameter> {
        self.bin_sliders.first().map(|slider| slider.parameter)
    }

    #[cfg(test)]
    pub fn marker_count_center(&self, more: bool) -> Option<Point> {
        self.marker_count_buttons()
            .map(|buttons| buttons[usize::from(more)].center())
    }

    #[cfg(test)]
    pub fn previous_event_center(&self) -> Option<Point> {
        self.event_buttons().map(|buttons| buttons[0].center())
    }

    #[cfg(test)]
    pub fn bin_reset_center(&self) -> Option<Point> {
        self.bin_button_bounds().map(|buttons| buttons[0].center())
    }

    #[cfg(test)]
    pub fn bin_next_center(&self) -> Option<Point> {
        self.bin_button_bounds().map(|buttons| buttons[2].center())
    }

    #[cfg(test)]
    pub fn panel_count(&self) -> usize {
        usize::from(self.left.is_some()) + usize::from(self.right.is_some())
    }

    pub fn draw(&self, scene: &mut Scene, stage: &Stage, palette: ScenePalette) {
        if let Some(panel) = self.left {
            let mut compact = *stage;
            compact.unit *= panel.scale;
            self.draw_shape(scene, panel.rect, &compact, palette);
        }
        if let Some(panel) = self.right {
            let mut compact = *stage;
            compact.unit *= panel.scale;
            self.draw_onsets(scene, panel.rect, &compact, palette);
        }
    }

    pub fn draw_controls(
        &self,
        scene: &mut Scene,
        stage: &Stage,
        palette: ScenePalette,
        theme: &Theme,
    ) {
        for (panel, title) in [
            (self.gap_controls, "gap_controls"),
            (self.onset_controls, "onset_controls"),
        ] {
            let Some(panel) = panel else {
                continue;
            };
            let mut compact = *stage;
            compact.unit *= panel.scale;
            let background = theme.colors.background;
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                background.with_alpha(0.86),
                None,
                &panel.rect.inflate(compact.px(5.0), compact.px(5.0)),
            );
            frame(scene, panel.rect, &compact, palette);
            self.label(
                scene,
                title,
                panel.rect,
                8.0,
                15.0,
                &compact,
                palette.ink.with_alpha(0.85),
            );
        }
        if let Some(panel) = self.gap_controls {
            let mut compact = *stage;
            compact.unit *= panel.scale;
            for (key, top) in [
                ("gap_reference_group", 38.0),
                ("gap_shape_group", 165.0),
                ("gap_energy_group", 286.0),
                ("gap_timing_group", 533.0),
                ("gap_return_group", 708.0),
            ] {
                self.label(
                    scene,
                    key,
                    panel.rect,
                    8.0,
                    top,
                    &compact,
                    palette.ink.with_alpha(0.85),
                );
            }
        }
        if let Some(panel) = self.onset_controls {
            let mut compact = *stage;
            compact.unit *= panel.scale;
            let mut top = 28.0;
            for (key, _, settings) in percussion_control_groups() {
                self.label(
                    scene,
                    key,
                    panel.rect,
                    8.0,
                    top + 12.0,
                    &compact,
                    palette.ink.with_alpha(0.85),
                );
                top += 18.0 + settings.len() as f64 * 24.0;
            }
        }
        for slider in &self.sliders {
            let unit = slider.unit;
            let enabled = slider.filter.is_none_or(|(_, enabled, _)| enabled);
            let color = self.control_color(palette, slider.rect, enabled || slider.toggle_only);
            let active = enabled
                && (self.drag == Some(slider.parameter)
                    || self
                        .pointer
                        .is_some_and(|point| slider.rect.contains(point)));
            if let Some((_, checked, rect)) = slider.filter {
                let checkbox_bounds = if slider.toggle_only {
                    slider.rect
                } else {
                    rect.inflate(4.0, 4.0)
                };
                let checkbox_color = self.control_color(palette, checkbox_bounds, true);
                scene.stroke(
                    &Stroke::new(unit * 0.9),
                    Affine::IDENTITY,
                    checkbox_color.with_alpha(0.75),
                    None,
                    &rect,
                );
                if checked {
                    let mark = rect.inset(unit * 2.5);
                    scene.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        checkbox_color.with_alpha(0.95),
                        None,
                        &mark,
                    );
                }
            }
            let caption_at = Point::new(
                slider.rect.x0
                    + if slider.filter.is_some() {
                        15.0 * unit
                    } else {
                        0.0
                    },
                slider.rect.y0 + 11.5 * unit,
            );
            slider.caption.draw_left(
                scene,
                Affine::translate(caption_at.to_vec2()) * Affine::scale(unit),
                color.with_alpha(if enabled { 0.9 } else { 0.45 }),
            );
            if slider.toggle_only {
                continue;
            }
            let value_at = Point::new(slider.rect.x1 - slider.readout.width() * unit, caption_at.y);
            slider.readout.draw_left(
                scene,
                Affine::translate(value_at.to_vec2()) * Affine::scale(unit),
                color.with_alpha(if enabled { 0.95 } else { 0.5 }),
            );
            let start = Point::new(slider.track.x0, slider.track.y0);
            let end = Point::new(slider.track.x1, slider.track.y0);
            let knob = Point::new(
                start.x + f64::from(slider.parameter.fraction(slider.value)) * slider.track.width(),
                start.y,
            );
            rule(
                scene,
                start,
                end,
                palette.muted.with_alpha(if enabled { 0.55 } else { 0.2 }),
                unit * 0.8,
            );
            rule(
                scene,
                start,
                knob,
                color.with_alpha(if active {
                    0.95
                } else if enabled {
                    0.7
                } else {
                    0.25
                }),
                unit * 1.2,
            );
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                color.with_alpha(if active {
                    1.0
                } else if enabled {
                    0.85
                } else {
                    0.3
                }),
                None,
                &Circle::new(knob, unit * if active { 3.0 } else { 2.2 }),
            );
        }
        if let Some(panel) = self.onset_controls {
            let mut compact = *stage;
            compact.unit *= panel.scale;
            self.label(
                scene,
                "page_hint",
                panel.rect,
                8.0,
                722.0,
                &compact,
                palette.muted.with_alpha(0.8),
            );
        }
        if let (Some(panel), Some(buttons)) = (self.right, self.event_buttons()) {
            let unit = panel.rect.height() / ONSET_PANEL_HEIGHT;
            let background = theme.colors.background;
            for (button, key) in
                buttons
                    .into_iter()
                    .zip(["previous_event", "next_event", "latest_event"])
            {
                let color = self.control_color(palette, button, true);
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    background.with_alpha(0.92),
                    None,
                    &button,
                );
                scene.stroke(
                    &Stroke::new(unit * 0.7),
                    Affine::IDENTITY,
                    color.with_alpha(0.5),
                    None,
                    &button,
                );
                if let Some(label) = self.labels.get(key) {
                    label.text.draw_left(
                        scene,
                        Affine::translate((
                            button.center().x - label.text.width() * unit * 0.5,
                            button.y0 + 13.0 * unit,
                        )) * Affine::scale(unit),
                        color,
                    );
                }
            }
        }
        self.draw_heatmap_controls(scene, stage, palette);
        self.draw_bin_controls(scene, stage, palette, theme);
        self.draw_hover_hint(scene, palette, theme);
    }

    /// Foreground is the resting color; only a hovered, interactive control
    /// borrows the current scene accent. Drag state alone does not recolor it.
    fn control_color(&self, palette: ScenePalette, bounds: Rect, enabled: bool) -> Color {
        if enabled && self.pointer.is_some_and(|point| bounds.contains(point)) {
            palette.accent
        } else {
            palette.ink
        }
    }

    fn hover_source(&self) -> Option<&'static str> {
        let point = self.pointer?;
        if let Some(slider) = self
            .sliders
            .iter()
            .find(|slider| slider.rect.contains(point))
        {
            if let Some((parameter, _, rect)) = slider
                .filter
                .filter(|(_, _, rect)| rect.inflate(4.0, 4.0).contains(point))
            {
                let _ = rect;
                return Some(parameter.analysis_hint());
            }
            return Some(slider.parameter.analysis_hint());
        }
        if self
            .bin_sliders
            .iter()
            .any(|slider| slider.rect.contains(point))
        {
            return Some("Drag to reduce this frequency bin for the selected instrument. This changes detector input, not the music volume, and starts a new reference history.");
        }
        if let Some(buttons) = self.event_buttons() {
            if let Some(index) = buttons.iter().position(|rect| rect.contains(point)) {
                return Some(match index {
                    0 => "Inspect the previous saved event and the exact reference and thresholds used before it was added.",
                    1 => "Inspect the next saved event and its original reference. Changing controls does not rewrite this saved decision.",
                    _ => "Show the reference from the last consumed audio. Filled markers keep their original trigger outcome, even when current thresholds have changed.",
                });
            }
        }
        if self
            .marker_count_buttons()
            .is_some_and(|buttons| buttons.iter().any(|rect| rect.contains(point)))
        {
            return Some("Show four fewer or more recent additions, up to 64. This affects only the displayed markers, and does not change learning or trigger filters.");
        }
        if let Some(buttons) = self.bin_button_bounds() {
            if let Some(index) = buttons.iter().position(|rect| rect.contains(point)) {
                return Some(if index == 0 {
                    "Restore all frequency-bin gains to 100% for this instrument. The changed input starts a new reference history."
                } else {
                    "Show the previous or next page of frequency-bin faders. This does not change the detector settings."
                });
            }
        }
        if self
            .spectrum_bounds()
            .is_some_and(|rect| rect.contains(point))
        {
            return Some("Click a frequency area to inspect its bins. The highlighted curve is the selected instrument input after its mask and bin gains.");
        }
        if self
            .onset_graph_bounds()
            .is_some_and(|rect| rect.contains(point))
        {
            return Some("Click to switch kick, snare and hat. Short marks failed input filtering, medium marks were learned only, and tall marks emitted a trigger.");
        }
        if self.right.is_some_and(|panel| {
            let unit = panel.rect.height() / ONSET_PANEL_HEIGHT;
            Rect::new(
                panel.rect.x0,
                panel.rect.y0 + 188.0 * unit,
                panel.rect.x1,
                panel.rect.y0 + 228.0 * unit,
            )
            .contains(point)
        }) {
            return Some("Left (solid) and right (dashed) are measured separately. The faint curve is their combined level; dotted lines are the backgrounds frozen when this attack began. A rise on either side contributes to sharpness even while the other side falls.");
        }
        if self
            .heatmap_bounds()
            .is_some_and(|rect| rect.contains(point))
        {
            return Some("Density counts current learned pairs. Filled markers are original triggers; hollow markers were learned only. Faint expired markers no longer affect thresholds. The shaded area passes only sharpness and energy filters. Other enabled input and trigger filters can still reject an attack.");
        }
        None
    }

    fn update_hover_hint(&mut self, fonts: &mut Fonts, stage: &Stage) {
        let Some(source) = self.hover_source() else {
            self.hover_hint = None;
            return;
        };
        let unit = stage.unit.max(0.7);
        let width = ((stage.width - stage.margin * 2.0) / unit - 16.0).clamp(100.0, 340.0) as f32;
        if self
            .hover_hint
            .as_ref()
            .is_none_or(|hint| hint.source != source || hint.width != width)
        {
            let text = fonts.layout_wrapped(
                source,
                12.0,
                UiFace::Light,
                Some(width),
                BlockAlign::Left,
                1.15,
            );
            let plan = text.plan();
            self.hover_hint = Some(HoverHint {
                source,
                width,
                text,
                plan,
                rect: Rect::ZERO,
                unit,
            });
        }
        let point = self.pointer.unwrap();
        let hint = self.hover_hint.as_mut().unwrap();
        let size = (
            (hint.text.width() + 16.0) * unit,
            (hint.text.height() + 16.0) * unit,
        );
        let x = (point.x + 12.0 * unit).clamp(
            stage.margin,
            (stage.width - stage.margin - size.0).max(stage.margin),
        );
        let y = if point.y + 18.0 * unit + size.1 <= stage.height - stage.margin {
            point.y + 18.0 * unit
        } else {
            (point.y - size.1 - 12.0 * unit).max(stage.margin)
        };
        hint.rect = Rect::new(x, y, x + size.0, y + size.1);
        hint.unit = unit;
    }

    fn draw_hover_hint(&self, scene: &mut Scene, palette: ScenePalette, theme: &Theme) {
        let Some(hint) = &self.hover_hint else {
            return;
        };
        let background = theme.colors.background;
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            background.with_alpha(0.97),
            None,
            &hint.rect,
        );
        scene.stroke(
            &Stroke::new(hint.unit * 0.8),
            Affine::IDENTITY,
            palette.muted.with_alpha(0.7),
            None,
            &hint.rect,
        );
        hint.plan.draw(
            scene,
            Affine::translate((
                hint.rect.x0 + 8.0 * hint.unit,
                hint.rect.y0 + 8.0 * hint.unit,
            )) * Affine::scale(hint.unit),
            palette.ink.with_alpha(0.95),
            TextPaint::Fill,
        );
    }

    fn draw_heatmap_controls(&self, scene: &mut Scene, stage: &Stage, palette: ScenePalette) {
        let (Some(panel), Some(buttons)) = (self.right, self.marker_count_buttons()) else {
            return;
        };
        let mut compact = *stage;
        compact.unit *= panel.scale;
        let unit = compact.unit;
        let background = if palette.ink.components[..3].iter().sum::<f32>() > 1.5 {
            Color::BLACK
        } else {
            Color::WHITE
        };
        let bank = Rect::new(
            panel.rect.x1 - 104.0 * unit,
            buttons[0].y0,
            panel.rect.x1,
            buttons[0].y1,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            background.with_alpha(0.92),
            None,
            &bank.inflate(unit * 2.0, unit),
        );
        self.label(
            scene,
            "recent_markers",
            panel.rect,
            panel.rect.width() / unit - 101.0,
            247.0,
            &compact,
            palette.ink.with_alpha(0.92),
        );
        for (index, (button, key)) in buttons
            .into_iter()
            .zip(["fewer_markers", "more_markers"])
            .enumerate()
        {
            let enabled = if index == 0 {
                self.recent_markers > 4
            } else {
                self.recent_markers < 64
            };
            let active = enabled && self.pointer.is_some_and(|point| button.contains(point));
            let color = self.control_color(palette, button, enabled);
            scene.stroke(
                &Stroke::new(unit * 0.7),
                Affine::IDENTITY,
                color.with_alpha(if active { 0.9 } else { 0.45 }),
                None,
                &button,
            );
            if let Some(label) = self.labels.get(key) {
                label.text.draw_left(
                    scene,
                    Affine::translate((
                        button.center().x - label.text.width() * unit * 0.5,
                        button.y0 + 13.0 * unit,
                    )) * Affine::scale(unit),
                    if enabled {
                        color.with_alpha(0.95)
                    } else {
                        palette.muted.with_alpha(0.35)
                    },
                );
            }
        }
    }

    fn draw_bin_controls(
        &self,
        scene: &mut Scene,
        stage: &Stage,
        palette: ScenePalette,
        theme: &Theme,
    ) {
        let Some(panel) = self.right else {
            return;
        };
        let mut compact = *stage;
        compact.unit *= panel.scale;
        let unit = compact.unit;
        let background = theme.colors.background;
        let editor = subrect(panel.rect, &compact, 0.0, BIN_EDITOR_TOP, 0.0, 132.0);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            background.with_alpha(0.86),
            None,
            &editor.inflate(unit * 4.0, unit * 3.0),
        );
        self.label(
            scene,
            "bins_title",
            panel.rect,
            0.0,
            614.0,
            &compact,
            palette.ink.with_alpha(0.85),
        );
        if let Some(buttons) = self.bin_button_bounds() {
            for (index, (button, key)) in buttons
                .into_iter()
                .zip(["reset_bins", "previous_bins", "next_bins"])
                .enumerate()
            {
                let enabled = match index {
                    1 => self.bin_page > 0,
                    2 => (self.bin_page + 1) * BINS_PER_PAGE < self.selected_bins.len(),
                    _ => true,
                };
                let active = enabled && self.pointer.is_some_and(|point| button.contains(point));
                let color = self.control_color(palette, button, enabled);
                scene.stroke(
                    &Stroke::new(unit * 0.7),
                    Affine::IDENTITY,
                    color.with_alpha(if active { 0.8 } else { 0.35 }),
                    None,
                    &button,
                );
                if let Some(label) = self.labels.get(key) {
                    label.text.draw_left(
                        scene,
                        Affine::translate((
                            button.center().x - label.text.width() * unit * 0.5,
                            button.y0 + 14.0 * unit,
                        )) * Affine::scale(unit),
                        if enabled {
                            color.with_alpha(0.9)
                        } else {
                            palette.muted.with_alpha(0.35)
                        },
                    );
                }
            }
        }
        for slider in &self.bin_sliders {
            let color = self.control_color(palette, slider.rect, true);
            let active = self.drag == Some(slider.parameter)
                || self
                    .pointer
                    .is_some_and(|point| slider.rect.contains(point));
            let x = slider.track.x0;
            let knob = Point::new(
                x,
                slider.track.y1
                    - f64::from(slider.meter.gain.clamp(0.0, 1.0)) * slider.track.height(),
            );
            rule(
                scene,
                Point::new(x, slider.track.y0),
                Point::new(x, slider.track.y1),
                palette.muted.with_alpha(0.45),
                unit,
            );
            rule(
                scene,
                Point::new(x, slider.track.y1),
                knob,
                color.with_alpha(if active { 1.0 } else { 0.7 }),
                unit * 1.4,
            );
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                color.with_alpha(0.95),
                None,
                &Circle::new(knob, unit * if active { 3.5 } else { 2.7 }),
            );
            let meter_x = slider.rect.x0 + slider.rect.width() * 0.70;
            let meter = Rect::new(
                meter_x - unit * 1.8,
                slider.track.y0,
                meter_x + unit * 1.8,
                slider.track.y1,
            );
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                palette.muted.with_alpha(0.12),
                None,
                &meter,
            );
            let amount = f64::from(slider.meter.onset.clamp(0.0, 1.0));
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                palette.ink.with_alpha(0.8),
                None,
                &Rect::new(
                    meter.x0,
                    meter.y1 - amount * meter.height(),
                    meter.x1,
                    meter.y1,
                ),
            );
            rule(
                scene,
                Point::new(slider.rect.x0 + unit * 3.0, slider.rect.y0),
                Point::new(
                    slider.rect.x0
                        + unit * 3.0
                        + (slider.rect.width() - unit * 6.0)
                            * f64::from(slider.meter.selected.clamp(0.0, 1.0)),
                    slider.rect.y0,
                ),
                palette.ink.with_alpha(0.45),
                unit,
            );
            slider.frequency.draw_left(
                scene,
                Affine::translate((
                    slider.rect.center().x - slider.frequency.width() * unit * 0.5,
                    panel.rect.y0 + 701.0 * unit,
                )) * Affine::scale(unit),
                palette.muted.with_alpha(0.85),
            );
        }
        let hovering_bin = self.pointer.is_some_and(|point| {
            self.bin_sliders
                .iter()
                .any(|slider| slider.rect.contains(point))
        });
        self.label(
            scene,
            if hovering_bin {
                "bin_detail"
            } else {
                "bin_page"
            },
            panel.rect,
            36.0,
            724.0,
            &compact,
            palette.muted.with_alpha(0.85),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn label(
        &self,
        scene: &mut Scene,
        key: &str,
        panel: Rect,
        x: f64,
        y: f64,
        stage: &Stage,
        color: Color,
    ) {
        if let Some(label) = self.labels.get(key) {
            let transform = Affine::translate((panel.x0 + stage.px(x), panel.y0 + stage.px(y)))
                * Affine::scale(stage.unit);
            label.text.draw_left(scene, transform, color);
        }
    }

    fn draw_shape(&self, scene: &mut Scene, panel: Rect, stage: &Stage, palette: ScenePalette) {
        frame(scene, panel, stage, palette);
        self.label(
            scene,
            "shape_title",
            panel,
            0.0,
            15.0,
            stage,
            palette.ink.with_alpha(0.8),
        );
        self.label(
            scene,
            "shape_state",
            panel,
            0.0,
            34.0,
            stage,
            palette.ink.with_alpha(0.85),
        );
        self.label(scene, "shape_key", panel, 0.0, 52.0, stage, palette.muted);
        let plot = subrect(panel, stage, 4.0, 62.0, 4.0, 68.0);
        grid(scene, plot, stage, palette);
        let baseline = plot.y0 + plot.height() * 0.5;
        rule(
            scene,
            Point::new(plot.x0, baseline),
            Point::new(plot.x1, baseline),
            palette.muted.with_alpha(0.25),
            stage.px(0.7),
        );
        for (shape, color, vertices) in [
            (
                &self.shape.reference_shape,
                palette.muted.with_alpha(0.55),
                false,
            ),
            (&self.shape.shape, palette.ink.with_alpha(0.9), true),
        ] {
            let mut path = BezPath::new();
            for (index, value) in shape.iter().enumerate() {
                let point = Point::new(
                    plot.x0 + index as f64 / (shape.len() - 1) as f64 * plot.width(),
                    baseline - f64::from(value.clamp(-1.0, 1.0)) * plot.height() * 0.48,
                );
                if index == 0 {
                    path.move_to(point);
                } else {
                    path.line_to(point);
                }
                if vertices {
                    scene.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        color,
                        None,
                        &Circle::new(point, stage.px(1.65)),
                    );
                }
            }
            scene.stroke(
                &Stroke::new(stage.px(if vertices { 1.15 } else { 0.8 })),
                Affine::IDENTITY,
                color,
                None,
                &path,
            );
        }
        for (key, x) in [
            ("axis_low", 0.0),
            ("axis_mid", panel.width() / stage.unit * 0.55 - 11.0),
            ("axis_high", panel.width() / stage.unit - 32.0),
        ] {
            self.label(
                scene,
                key,
                panel,
                x,
                145.0,
                stage,
                palette.muted.with_alpha(0.75),
            );
        }
        self.label(
            scene,
            "movement",
            panel,
            0.0,
            165.0,
            stage,
            palette.ink.with_alpha(0.7),
        );
        self.label(
            scene,
            "movement_key",
            panel,
            0.0,
            179.0,
            stage,
            palette.muted,
        );
        let movement = subrect(panel, stage, 60.0, 190.0, 0.0, 54.0);
        self.armed_intervals(scene, movement, palette.ink.with_alpha(0.09));
        grid(scene, movement, stage, palette);
        let cut_y = movement.y1 - movement.height() * 0.25;
        scene.stroke(
            &Stroke::new(stage.px(0.8)).with_dashes(0.0, [stage.px(6.0), stage.px(3.0)]),
            Affine::IDENTITY,
            palette.muted.with_alpha(0.8),
            None,
            &Line::new((movement.x0, cut_y), (movement.x1, cut_y)),
        );
        self.trace(
            scene,
            movement,
            |sample| sample.distance / sample.gap.shape_distance_threshold.max(1e-6) / 4.0,
            palette.ink.with_alpha(0.95),
            stage,
        );
        self.trace_stroke(
            scene,
            movement,
            |sample| sample.trend / sample.gap.trend_change_threshold.max(1e-6) / 4.0,
            palette.accent.with_alpha(0.9),
            Stroke::new(stage.px(1.0)).with_dashes(0.0, [stage.px(3.0), stage.px(2.0)]),
        );
        self.trace_stroke(
            scene,
            movement,
            |sample| {
                sample.gap.changed_fraction / sample.gap.changed_fraction_threshold.max(1e-6) / 4.0
            },
            palette.ink.with_alpha(0.65),
            Stroke::new(stage.px(1.1)).with_dashes(0.0, [stage.px(1.0), stage.px(2.0)]),
        );
        self.label(
            scene,
            "movement_top",
            panel,
            30.0,
            195.0,
            stage,
            palette.muted,
        );
        self.label(
            scene,
            "movement_cut",
            panel,
            30.0,
            234.0,
            stage,
            palette.muted,
        );
        self.label(
            scene,
            "movement_zero",
            panel,
            30.0,
            247.0,
            stage,
            palette.muted,
        );
        self.events(scene, movement, 3, palette.ink, stage);
        self.label(
            scene,
            "shape_values",
            panel,
            0.0,
            258.0,
            stage,
            palette.muted,
        );
        self.label(
            scene,
            "movement_hint",
            panel,
            0.0,
            272.0,
            stage,
            palette.muted,
        );
        self.label(
            scene,
            "energy",
            panel,
            0.0,
            286.0,
            stage,
            palette.ink.with_alpha(0.7),
        );
        self.label(scene, "loss_key", panel, 0.0, 300.0, stage, palette.muted);
        let loss_plot = subrect(panel, stage, 60.0, 310.0, 0.0, 56.0);
        let (minimum, maximum) = self.loss_bounds();
        let fraction = |loss: f32| (loss - minimum) / (maximum - minimum);
        let zero_y = loss_plot.y1 - f64::from(fraction(0.0)) * loss_plot.height();
        grid(scene, loss_plot, stage, palette);
        // Passing loss has its own continuous hold. Tint records this threshold
        // check, while the readout explains whether the loss-only path is ready.
        let mut previous = None;
        for sample in &self.samples {
            let x = self.x(loss_plot, sample.at);
            if let Some((start, passes)) = previous {
                if passes {
                    scene.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        palette.accent.with_alpha(0.08),
                        None,
                        &Rect::new(start, loss_plot.y0, x, loss_plot.y1),
                    );
                }
            }
            previous = Some((x, -sample.energy_delta_db > sample.mix_loss_threshold_db));
        }
        rule(
            scene,
            Point::new(loss_plot.x0, zero_y),
            Point::new(loss_plot.x1, zero_y),
            palette.muted.with_alpha(0.5),
            stage.px(0.7),
        );
        self.trace(
            scene,
            loss_plot,
            |sample| fraction(-sample.energy_delta_db),
            palette.ink.with_alpha(0.95),
            stage,
        );
        self.trace_stroke(
            scene,
            loss_plot,
            |sample| fraction(sample.mix_loss_threshold_db),
            palette.accent.with_alpha(0.9),
            Stroke::new(stage.px(1.1)).with_dashes(0.0, [stage.px(4.0), stage.px(3.0)]),
        );
        self.events(scene, loss_plot, 3, palette.ink, stage);
        for (key, y) in [("loss_top", 314.0), ("loss_bottom", 370.0)] {
            self.label(scene, key, panel, 30.0, y, stage, palette.muted);
        }
        self.label(
            scene,
            "loss_zero",
            panel,
            30.0,
            (zero_y - panel.y0) / stage.unit + 3.0,
            stage,
            palette.muted,
        );
        self.label(
            scene,
            "time_start",
            panel,
            60.0,
            380.0,
            stage,
            palette.muted,
        );
        self.label(scene, "time_end", panel, 412.0, 380.0, stage, palette.muted);
        self.label(
            scene,
            "energy_values",
            panel,
            0.0,
            396.0,
            stage,
            palette.ink.with_alpha(0.9),
        );
        self.label(scene, "loss_hold", panel, 0.0, 410.0, stage, palette.muted);
        self.label(scene, "loss_state", panel, 0.0, 423.0, stage, palette.muted);
        self.label(
            scene,
            "level_title",
            panel,
            0.0,
            443.0,
            stage,
            palette.ink.with_alpha(0.7),
        );
        self.label(scene, "level_key", panel, 0.0, 457.0, stage, palette.muted);
        let levels = subrect(panel, stage, 60.0, 465.0, 0.0, 32.0);
        grid(scene, levels, stage, palette);
        let ceiling = self.level_ceiling();
        let level_fraction = |db: f32| (db + 80.0) / (ceiling + 80.0);
        self.trace(
            scene,
            levels,
            |sample| level_fraction(sample.rms_db),
            palette.ink.with_alpha(0.95),
            stage,
        );
        self.trace_stroke(
            scene,
            levels,
            |sample| level_fraction(sample.gap.gap_rms_threshold_db),
            palette.accent.with_alpha(0.9),
            Stroke::new(stage.px(1.0)).with_dashes(0.0, [stage.px(4.0), stage.px(3.0)]),
        );
        self.trace_stroke(
            scene,
            levels,
            |sample| level_fraction(sample.envelope_db - sample.energy_delta_db),
            palette.muted.with_alpha(0.7),
            Stroke::new(stage.px(1.0)).with_dashes(0.0, [stage.px(1.0), stage.px(2.0)]),
        );
        self.label(scene, "level_top", panel, 30.0, 469.0, stage, palette.muted);
        self.label(
            scene,
            "level_bottom",
            panel,
            30.0,
            500.0,
            stage,
            palette.muted,
        );
        self.label(
            scene,
            "reference_values",
            panel,
            0.0,
            511.0,
            stage,
            palette.muted,
        );
        self.label(
            scene,
            "rms_gap_hold",
            panel,
            0.0,
            524.0,
            stage,
            palette.ink.with_alpha(0.85),
        );
        self.draw_gap_timing(scene, panel, stage, palette);
        self.label(
            scene,
            "return_title",
            panel,
            0.0,
            708.0,
            stage,
            palette.ink.with_alpha(0.8),
        );
        self.label(scene, "return_key", panel, 0.0, 722.0, stage, palette.muted);
        let bass = subrect(panel, stage, 60.0, 732.0, 0.0, 90.0);
        let (bass_low, bass_high) = self.bass_bounds();
        let bass_fraction = |db: f32| (db - bass_low) / (bass_high - bass_low);
        grid(scene, bass, stage, palette);
        self.armed_intervals(scene, bass, palette.accent.with_alpha(0.1));
        self.trace(
            scene,
            bass,
            |sample| bass_fraction(sample.gap.bass_fast_db),
            palette.ink.with_alpha(0.9),
            stage,
        );
        self.trace_stroke(
            scene,
            bass,
            |sample| bass_fraction(sample.gap.bass_slow_db),
            palette.ink.with_alpha(0.65),
            Stroke::new(stage.px(1.0)).with_dashes(0.0, [stage.px(1.0), stage.px(2.0)]),
        );
        self.trace_stroke(
            scene,
            bass,
            |sample| bass_fraction(sample.gap.bass_onset_threshold_db),
            palette.accent.with_alpha(0.9),
            Stroke::new(stage.px(1.0)).with_dashes(0.0, [stage.px(4.0), stage.px(3.0)]),
        );
        self.label(scene, "bass_high", panel, 30.0, 736.0, stage, palette.muted);
        self.label(scene, "bass_low", panel, 30.0, 825.0, stage, palette.muted);
        self.events(scene, bass, 3, palette.ink, stage);
        self.label(scene, "hit_values", panel, 0.0, 838.0, stage, palette.muted);
        self.label(scene, "return_rms", panel, 0.0, 852.0, stage, palette.muted);
    }

    fn bass_bounds(&self) -> (f32, f32) {
        if self.samples.is_empty() {
            return (-80.0, 0.0);
        }
        let levels = self.samples.iter().flat_map(|sample| {
            [
                sample.gap.bass_fast_db,
                sample.gap.bass_slow_db,
                sample.gap.bass_onset_threshold_db,
            ]
        });
        let (low, high) = levels.fold((0.0_f32, -80.0_f32), |(low, high), db| {
            (low.min(db), high.max(db))
        });
        let low = ((low - 3.0) / 6.0).floor() * 6.0;
        let high = ((high + 3.0) / 6.0).ceil() * 6.0;
        (low.max(-80.0), high.max(low.max(-80.0) + 12.0))
    }

    fn draw_gap_timing(
        &self,
        scene: &mut Scene,
        panel: Rect,
        stage: &Stage,
        palette: ScenePalette,
    ) {
        self.label(
            scene,
            "timing_title",
            panel,
            0.0,
            533.0,
            stage,
            palette.ink.with_alpha(0.8),
        );
        self.label(scene, "timing_key", panel, 0.0, 547.0, stage, palette.muted);
        let plot = subrect(panel, stage, 60.0, 558.0, 0.0, 84.0);
        grid(scene, plot, stage, palette);
        let rows: [(&str, fn(&Sample) -> bool); 5] = [
            ("timing_shift", |s| {
                s.distance > s.gap.shape_distance_threshold
            }),
            ("timing_trend", |s| s.trend > s.gap.trend_change_threshold),
            ("timing_area", |s| {
                s.gap.changed_fraction >= s.gap.changed_fraction_threshold
            }),
            ("timing_energy", |s| {
                -s.energy_delta_db > s.mix_loss_threshold_db
            }),
            ("timing_together", |s| s.gap.gap_conditions_met),
        ];
        for (index, (key, passes)) in rows.into_iter().enumerate() {
            let row = Rect::new(
                plot.x0,
                plot.y0 + stage.px(index as f64 * 14.0 + 2.0),
                plot.x1,
                plot.y0 + stage.px(index as f64 * 14.0 + 12.0),
            );
            let color = if index == 4 {
                palette.accent
            } else {
                palette.ink
            };
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                palette.muted.with_alpha(0.08),
                None,
                &row,
            );
            self.label(
                scene,
                key,
                panel,
                0.0,
                568.0 + index as f64 * 14.0,
                stage,
                color.with_alpha(0.8),
            );
            let mut start = None;
            for sample in &self.samples {
                let x = self.x(row, sample.at);
                if passes(sample) {
                    start.get_or_insert(x);
                } else if let Some(x0) = start.take() {
                    scene.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        color.with_alpha(0.55),
                        None,
                        &Rect::new(x0, row.y0, x, row.y1),
                    );
                }
            }
            if let Some(x0) = start {
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    color.with_alpha(0.55),
                    None,
                    &Rect::new(x0, row.y0, row.x1, row.y1),
                );
            }
        }
        let hold = Rect::new(
            plot.x0,
            plot.y0 + stage.px(72.0),
            plot.x1,
            plot.y0 + stage.px(82.0),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            palette.muted.with_alpha(0.08),
            None,
            &hold,
        );
        self.label(
            scene,
            "timing_hold",
            panel,
            0.0,
            638.0,
            stage,
            palette.ink.with_alpha(0.8),
        );
        self.trace(
            scene,
            hold,
            |sample| sample.gap.gap_held_seconds / sample.gap.gap_hold_seconds.max(1e-6),
            palette.ink.with_alpha(0.95),
            stage,
        );
        self.events(scene, plot, 3, palette.ink, stage);
        self.label(
            scene,
            "time_start",
            panel,
            60.0,
            656.0,
            stage,
            palette.muted,
        );
        self.label(scene, "time_end", panel, 412.0, 656.0, stage, palette.muted);
        self.label(
            scene,
            "hold_values",
            panel,
            0.0,
            672.0,
            stage,
            palette.ink.with_alpha(0.9),
        );
        self.label(
            scene,
            "timing_state",
            panel,
            0.0,
            686.0,
            stage,
            palette.muted,
        );
    }

    fn loss_bounds(&self) -> (f32, f32) {
        let low = self
            .samples
            .iter()
            .map(|sample| -sample.energy_delta_db)
            .fold(-0.25, f32::min);
        let high = self
            .samples
            .iter()
            .map(|sample| (-sample.energy_delta_db).max(sample.mix_loss_threshold_db))
            .fold(self.shape.mix_loss_threshold_db.max(0.5), f32::max);
        let step = if high <= 2.0 { 0.25 } else { 1.0 };
        let high = (high * 1.1 / step).ceil() * step;
        ((low.min(-high * 0.25) / step).floor() * step, high)
    }

    fn level_ceiling(&self) -> f32 {
        let peak = self
            .samples
            .iter()
            .map(|sample| {
                sample
                    .rms_db
                    .max(sample.envelope_db)
                    .max(sample.envelope_db - sample.energy_delta_db)
            })
            .fold(0.0, f32::max);
        (peak / 6.0).ceil() * 6.0
    }

    fn draw_onsets(&self, scene: &mut Scene, panel: Rect, stage: &Stage, palette: ScenePalette) {
        frame(scene, panel, stage, palette);
        for (key, x, y, color) in [
            ("onset_title", 0.0, 15.0, palette.ink),
            ("selected_channel", 0.0, 35.0, palette.ink),
            ("gate", 100.0, 35.0, palette.muted),
            ("switch_channel", 0.0, 54.0, palette.muted),
            ("candidate_title", 0.0, 73.0, palette.ink),
            ("population", 0.0, 128.0, palette.muted),
            ("decision", 0.0, 148.0, palette.ink),
            ("ranks", 0.0, 164.0, palette.muted),
            ("envelope_title", 0.0, 182.0, palette.ink),
            ("event_timing", 0.0, 238.0, palette.muted),
            ("heatmap_title", 0.0, 247.0, palette.ink),
            ("heatmap_high", 0.0, 267.0, palette.muted),
            ("heatmap_middle", 0.0, 363.0, palette.muted),
            ("heatmap_low", 0.0, 458.0, palette.muted),
            ("heatmap_unit", 0.0, 471.0, palette.muted),
            ("heatmap_x_axis", 34.0, 471.0, palette.muted),
            ("heatmap_x_mid", 180.0, 471.0, palette.muted),
            ("heatmap_x_max", 428.0, 471.0, palette.muted),
            ("heatmap_markers", 0.0, 485.0, palette.ink),
            ("heatmap_cutoffs", 0.0, 497.0, palette.muted),
            ("heatmap_recent", 0.0, 509.0, palette.muted),
            ("heatmap_gates", 0.0, 521.0, palette.muted),
            ("spectrum_title", 0.0, 532.0, palette.ink),
        ] {
            self.label(scene, key, panel, x, y, stage, color.with_alpha(0.85));
        }
        let timeline = subrect(panel, stage, 0.0, 80.0, 0.0, 31.0);
        grid(scene, timeline, stage, palette);
        for event in self
            .decisions
            .iter()
            .filter(|event| event.channel == self.selected)
        {
            let age = self.audio_time - event.completion_time;
            if !(0.0..=f64::from(HISTORY_SECONDS)).contains(&age) {
                continue;
            }
            let x = timeline.x1 - age / f64::from(HISTORY_SECONDS) * timeline.width();
            let color = if event.eligible {
                palette.ink
            } else if event.admitted {
                palette.ink.with_alpha(0.65)
            } else {
                palette.muted.with_alpha(0.5)
            };
            let height = if event.eligible {
                1.0
            } else if event.admitted {
                0.65
            } else {
                0.3
            };
            rule(
                scene,
                Point::new(x, timeline.y1),
                Point::new(x, timeline.y1 - timeline.height() * height),
                color,
                stage.px(if self.inspected_id == Some(event.id) {
                    2.5
                } else {
                    1.2
                }),
            );
        }
        self.draw_event_envelope(
            scene,
            subrect(panel, stage, 0.0, 188.0, 0.0, 40.0),
            stage,
            palette,
        );
        self.draw_heatmap(scene, stage, palette);
        let spectrum = subrect(panel, stage, 0.0, SPECTRUM_TOP, 0.0, 44.0);
        self.draw_spectrum(scene, spectrum, stage, palette);
        for (key, frequency, offset) in [
            ("raw_axis_low", 20.0, 0.0),
            ("axis_mid", 1000.0, 11.0),
            ("axis_high", 16000.0, 32.0),
        ] {
            self.label(
                scene,
                key,
                panel,
                f64::from(spectrum_fraction(frequency)) * panel.width() / stage.unit - offset,
                580.0,
                stage,
                palette.muted.with_alpha(0.8),
            );
        }
        self.label(
            scene,
            "resolution",
            panel,
            0.0,
            594.0,
            stage,
            palette.muted.with_alpha(0.9),
        );
    }

    fn draw_event_envelope(
        &self,
        scene: &mut Scene,
        plot: Rect,
        stage: &Stage,
        palette: ScenePalette,
    ) {
        grid(scene, plot, stage, palette);
        let Some(event) = self.viewed_decision() else {
            return;
        };
        let points = &event.envelope[..event.envelope_len.min(event.envelope.len())];
        let backgrounds = stereo_backgrounds(event);
        let maximum = points
            .iter()
            .flat_map(|point| {
                let sides = stereo_envelope_amplitudes(point);
                [point.amplitude, sides[0], sides[1]]
            })
            .chain(backgrounds.map(f32::sqrt))
            .fold(event.peak_amplitude.max(1e-9), f32::max);
        let position = |seconds: f32, amplitude: f32| {
            Point::new(
                plot.x0
                    + f64::from((seconds / event.feature_seconds.max(1e-6)).clamp(0.0, 1.0))
                        * plot.width(),
                plot.y1 - f64::from((amplitude / maximum).clamp(0.0, 1.0)) * plot.height(),
            )
        };
        for background in backgrounds {
            let y = position(0.0, background.sqrt()).y;
            segmented_rule(
                scene,
                Point::new(plot.x0, y),
                Point::new(plot.x1, y),
                palette.ink.with_alpha(0.45),
                stage.px(0.7),
                stage.px(1.0),
                stage.px(3.0),
            );
        }
        // Both endpoints show the fixed feature horizon saved with this event.
        // The curve never uses display-rate peak holds or a newer background.
        let mut path = BezPath::new();
        for (index, point) in points.iter().enumerate() {
            let at = position(point.seconds, point.amplitude);
            if index == 0 {
                path.move_to(at);
            } else {
                path.line_to(at);
            }
        }
        scene.stroke(
            &Stroke::new(stage.px(0.8)),
            Affine::IDENTITY,
            palette.muted.with_alpha(0.5),
            None,
            &path,
        );
        let mut left = BezPath::new();
        for (index, point) in points.iter().enumerate() {
            let at = position(point.seconds, stereo_envelope_amplitudes(point)[0]);
            if index == 0 {
                left.move_to(at);
            } else {
                left.line_to(at);
            }
        }
        scene.stroke(
            &Stroke::new(stage.px(1.25)),
            Affine::IDENTITY,
            palette.ink.with_alpha(0.95),
            None,
            &left,
        );
        for pair in points.windows(2) {
            segmented_rule(
                scene,
                position(pair[0].seconds, stereo_envelope_amplitudes(&pair[0])[1]),
                position(pair[1].seconds, stereo_envelope_amplitudes(&pair[1])[1]),
                palette.ink.with_alpha(0.95),
                stage.px(1.1),
                stage.px(3.0),
                stage.px(2.0),
            );
        }
        for x in [plot.x0, plot.x1] {
            rule(
                scene,
                Point::new(x, plot.y0),
                Point::new(x, plot.y1),
                palette.ink.with_alpha(0.35),
                stage.px(0.7),
            );
        }
    }

    fn draw_heatmap(&self, scene: &mut Scene, stage: &Stage, palette: ScenePalette) {
        let Some(plot) = self.heatmap_bounds() else {
            return;
        };
        let reference = self.viewed_reference();
        if self.audio_time <= 0.0 {
            grid(scene, plot, stage, palette);
            return;
        }
        let thresholds = reference.thresholds;
        let effective = heatmap_position(
            plot,
            &reference,
            thresholds.sharpness_effective_bound.unwrap_or(0.0),
            thresholds.energy_effective_bound.unwrap_or(0.0),
        );
        // This rectangle describes only the two feature conditions. Original
        // marker outcomes also include quality, mix RMS, prominence and spacing.
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            palette.ink.with_alpha(0.075),
            None,
            &Rect::new(effective.x, plot.y0, plot.x1, effective.y),
        );
        let peak = reference
            .joint_histogram
            .iter()
            .flatten()
            .copied()
            .max()
            .unwrap_or(0)
            .max(1) as f64;
        let cells = reference.joint_histogram.len();
        for (energy, row) in reference.joint_histogram.iter().enumerate() {
            for (sharpness, &count) in row.iter().enumerate() {
                if count == 0 {
                    continue;
                }
                let width = plot.width() / cells as f64;
                let height = plot.height() / cells as f64;
                let opacity = 0.12 + 0.55 * ((count as f64 + 1.0).ln() / (peak + 1.0).ln());
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    palette.ink.with_alpha(opacity as f32),
                    None,
                    &Rect::new(
                        plot.x0 + sharpness as f64 * width,
                        plot.y1 - (energy + 1) as f64 * height,
                        plot.x0 + (sharpness + 1) as f64 * width,
                        plot.y1 - energy as f64 * height,
                    ),
                );
            }
        }
        for index in 0..=4 {
            let x = plot.x0 + plot.width() * index as f64 / 4.0;
            let y = plot.y0 + plot.height() * index as f64 / 4.0;
            rule(
                scene,
                (x, plot.y0).into(),
                (x, plot.y1).into(),
                palette.muted.with_alpha(0.17),
                stage.px(0.5),
            );
            rule(
                scene,
                (plot.x0, y).into(),
                (plot.x1, y).into(),
                palette.muted.with_alpha(0.17),
                stage.px(0.5),
            );
        }
        // Admission has an energy floor but no sharpness-percentile condition.
        if thresholds.filters.input_energy {
            let admission = heatmap_position(plot, &reference, 0.0, thresholds.admission_energy);
            segmented_rule(
                scene,
                (plot.x0, admission.y).into(),
                (plot.x1, admission.y).into(),
                palette.muted.with_alpha(0.85),
                stage.px(0.9),
                stage.px(1.0),
                stage.px(3.0),
            );
        }
        let physical = heatmap_position(
            plot,
            &reference,
            thresholds.min_sharpness,
            thresholds.feature_seconds * 10.0_f32.powf(thresholds.min_attack_db / 10.0),
        );
        if thresholds.filters.min_sharpness {
            segmented_rule(
                scene,
                (physical.x, plot.y0).into(),
                (physical.x, plot.y1).into(),
                palette.ink.with_alpha(0.8),
                stage.px(1.0),
                stage.px(5.0),
                stage.px(3.0),
            );
        }
        if thresholds.filters.attack_level {
            segmented_rule(
                scene,
                (plot.x0, physical.y).into(),
                (plot.x1, physical.y).into(),
                palette.ink.with_alpha(0.8),
                stage.px(1.0),
                stage.px(5.0),
                stage.px(3.0),
            );
        }
        if let Some(cutoff) = reference.sharpness_cutoff {
            let point = heatmap_position(plot, &reference, cutoff, 0.0);
            rule(
                scene,
                (point.x, plot.y0).into(),
                (point.x, plot.y1).into(),
                palette.ink.with_alpha(0.95),
                stage.px(1.3),
            );
        }
        if let Some(cutoff) = reference.energy_cutoff {
            let point = heatmap_position(plot, &reference, 0.0, cutoff);
            rule(
                scene,
                (plot.x0, point.y).into(),
                (plot.x1, point.y).into(),
                palette.ink.with_alpha(0.95),
                stage.px(1.3),
            );
        }
        let points = heatmap_recent_points(&reference, self.recent_markers);
        for (index, marker) in points.iter().enumerate() {
            let newest = index + 1 == points.len();
            let age_alpha = 0.65 + 0.35 * (index + 1) as f32 / points.len().max(1) as f32;
            let color = if !marker.in_reference {
                palette.muted.with_alpha(0.25)
            } else if marker.eligible {
                palette.ink.with_alpha(age_alpha)
            } else {
                palette.ink.with_alpha(age_alpha)
            };
            let point = heatmap_position(plot, &reference, marker.sharpness, marker.energy);
            let radius = stage.px(if newest { 4.0 } else { 2.4 });
            let circle = Circle::new(point, radius);
            if marker.eligible {
                scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &circle);
            } else {
                scene.stroke(
                    &Stroke::new(stage.px(if newest { 1.4 } else { 0.9 })),
                    Affine::IDENTITY,
                    color,
                    None,
                    &circle,
                );
            }
        }
        if let (Some((point, _)), Some(label)) = (
            heatmap_marker_cluster(plot, &reference, self.recent_markers),
            self.labels.get("heatmap_cluster"),
        ) {
            let x = (point.x + stage.px(7.0))
                .min(plot.x1 - label.text.width() * stage.unit - stage.px(3.0))
                .max(plot.x0);
            let y = (point.y - stage.px(7.0)).max(plot.y0 + stage.px(12.0));
            label.text.draw_left(
                scene,
                Affine::translate((x, y)) * Affine::scale(stage.unit),
                palette.ink.with_alpha(0.95),
            );
        }
        if let Some(candidate) = self.heatmap_candidate() {
            let point = heatmap_position(plot, &reference, candidate.sharpness, candidate.energy);
            let radius = stage.px(4.8);
            for direction in [-1.0, 1.0] {
                rule(
                    scene,
                    (point.x - radius, point.y - direction * radius).into(),
                    (point.x + radius, point.y + direction * radius).into(),
                    palette.ink.with_alpha(0.98),
                    stage.px(1.5),
                );
            }
        }
        scene.stroke(
            &Stroke::new(stage.px(0.7)),
            Affine::IDENTITY,
            palette.muted.with_alpha(0.45),
            None,
            &plot,
        );
    }

    fn draw_spectrum(&self, scene: &mut Scene, plot: Rect, stage: &Stage, palette: ScenePalette) {
        grid(scene, plot, stage, palette);
        let peak = self
            .bins
            .iter()
            .map(|bin| bin.magnitude)
            .fold(1e-6_f32, f32::max);
        let mut raw = BezPath::new();
        let mut weighted = BezPath::new();
        let mut weighted_started = false;
        for (index, bin) in self.bins.iter().enumerate() {
            let x = plot.x0 + f64::from(spectrum_fraction(bin.frequency_hz)) * plot.width();
            let level = |magnitude: f32| {
                (((magnitude.max(1e-9) / peak).log10() * 20.0 + 48.0) / 48.0).clamp(0.0, 1.0)
            };
            let point = Point::new(x, plot.y1 - f64::from(level(bin.magnitude)) * plot.height());
            if index == 0 {
                raw.move_to(point);
            } else {
                raw.line_to(point);
            }
            if bin.selected > 0.0 {
                let left = plot.x0 + f64::from(spectrum_fraction(bin.low_hz)) * plot.width();
                let right = plot.x0 + f64::from(spectrum_fraction(bin.high_hz)) * plot.width();
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    palette.ink.with_alpha(0.08 + 0.08 * bin.selected),
                    None,
                    &Rect::new(left, plot.y0, right.max(left + stage.px(0.3)), plot.y1),
                );
                let point = Point::new(
                    x,
                    plot.y1 - f64::from(level(bin.weighted_magnitude)) * plot.height(),
                );
                if !weighted_started {
                    weighted.move_to(point);
                    weighted_started = true;
                } else {
                    weighted.line_to(point);
                }
            } else {
                weighted_started = false;
            }
        }
        scene.stroke(
            &Stroke::new(stage.px(0.8)),
            Affine::IDENTITY,
            palette.muted.with_alpha(0.65),
            None,
            &raw,
        );
        scene.stroke(
            &Stroke::new(stage.px(1.15)),
            Affine::IDENTITY,
            palette.ink.with_alpha(0.95),
            None,
            &weighted,
        );
        if let (Some(&first), Some(&last)) = (
            self.selected_bins.get(self.bin_page * BINS_PER_PAGE),
            self.selected_bins.get(
                ((self.bin_page + 1) * BINS_PER_PAGE)
                    .min(self.selected_bins.len())
                    .saturating_sub(1),
            ),
        ) {
            let x0 = plot.x0 + f64::from(spectrum_fraction(self.bins[first].low_hz)) * plot.width();
            let x1 = plot.x0 + f64::from(spectrum_fraction(self.bins[last].high_hz)) * plot.width();
            scene.stroke(
                &Stroke::new(stage.px(0.8)),
                Affine::IDENTITY,
                palette.ink.with_alpha(0.5),
                None,
                &Rect::new(x0, plot.y0, x1.max(x0 + stage.px(1.0)), plot.y1),
            );
        }
    }

    fn x(&self, plot: Rect, at: f32) -> f64 {
        let now = self.samples.back().map_or(0.0, |sample| sample.at);
        plot.x1 - f64::from(((now - at) / HISTORY_SECONDS).clamp(0.0, 1.0)) * plot.width()
    }

    fn trace(
        &self,
        scene: &mut Scene,
        plot: Rect,
        value: impl Fn(&Sample) -> f32,
        color: Color,
        stage: &Stage,
    ) {
        self.trace_stroke(scene, plot, value, color, Stroke::new(stage.px(1.0)));
    }

    fn trace_stroke(
        &self,
        scene: &mut Scene,
        plot: Rect,
        value: impl Fn(&Sample) -> f32,
        color: Color,
        stroke: Stroke,
    ) {
        let mut path = BezPath::new();
        for (index, sample) in self.samples.iter().enumerate() {
            let point = Point::new(
                self.x(plot, sample.at),
                plot.y1 - f64::from(value(sample).clamp(0.0, 1.0)) * plot.height(),
            );
            if index == 0 {
                path.move_to(point);
            } else {
                path.line_to(point);
            }
        }
        scene.stroke(&stroke, Affine::IDENTITY, color, None, &path);
    }

    fn events(&self, scene: &mut Scene, plot: Rect, detector: usize, color: Color, stage: &Stage) {
        let mut previous = None;
        for sample in &self.samples {
            let count = sample.counts[detector];
            if previous.is_some_and(|last| count > last) {
                let x = self.x(plot, sample.at);
                rule(
                    scene,
                    Point::new(x, plot.y0),
                    Point::new(x, plot.y1),
                    color,
                    stage.px(1.4),
                );
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    color,
                    None,
                    &Rect::new(
                        x - stage.px(2.0),
                        plot.y0,
                        x + stage.px(2.0),
                        plot.y0 + stage.px(3.5),
                    ),
                );
            }
            previous = Some(count);
        }
    }

    fn armed_intervals(&self, scene: &mut Scene, plot: Rect, color: Color) {
        let mut start = None;
        for sample in &self.samples {
            let x = self.x(plot, sample.at);
            if sample.armed {
                start.get_or_insert(x);
            } else if let Some(x0) = start.take() {
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    color,
                    None,
                    &Rect::new(x0, plot.y0, x, plot.y1),
                );
            }
        }
        if let Some(x0) = start {
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                color,
                None,
                &Rect::new(x0, plot.y0, plot.x1, plot.y1),
            );
        }
    }
}

fn subrect(panel: Rect, stage: &Stage, left: f64, top: f64, right: f64, height: f64) -> Rect {
    Rect::new(
        panel.x0 + stage.px(left),
        panel.y0 + stage.px(top),
        panel.x1 - stage.px(right),
        panel.y0 + stage.px(top + height),
    )
}

fn rule(scene: &mut Scene, a: Point, b: Point, color: Color, width: f64) {
    scene.stroke(
        &Stroke::new(width.max(0.45)),
        Affine::IDENTITY,
        color,
        None,
        &Line::new(a, b),
    );
}

fn frame(scene: &mut Scene, panel: Rect, stage: &Stage, palette: ScenePalette) {
    rule(
        scene,
        Point::new(panel.x0, panel.y0 - stage.px(4.0)),
        Point::new(panel.x1, panel.y0 - stage.px(4.0)),
        palette.muted.with_alpha(0.45),
        stage.px(0.7),
    );
    rule(
        scene,
        Point::new(panel.x0, panel.y0 - stage.px(4.0)),
        Point::new(panel.x0 + stage.px(42.0), panel.y0 - stage.px(4.0)),
        palette.ink.with_alpha(0.8),
        stage.px(1.5),
    );
}

fn grid(scene: &mut Scene, plot: Rect, stage: &Stage, palette: ScenePalette) {
    rule(
        scene,
        Point::new(plot.x0, plot.y1),
        Point::new(plot.x1, plot.y1),
        palette.muted.with_alpha(0.35),
        stage.px(0.7),
    );
    for index in 0..=8 {
        let x = plot.x0 + plot.width() * index as f64 / 8.0;
        rule(
            scene,
            Point::new(x, plot.y1),
            Point::new(x, plot.y0),
            palette.muted.with_alpha(0.1),
            stage.px(0.5),
        );
    }
}

fn slider_caption(parameter: Parameter) -> &'static str {
    match parameter {
        Parameter::Arrangement(setting) => setting.label(),
        Parameter::Percussion(_, setting) => match setting {
            PercussionSetting::TaperHz => "Fade out by",
            PercussionSetting::EnvelopeMs => "Smooth the level",
            PercussionSetting::BackgroundMs => "Track background",
            PercussionSetting::MergeMs => "Merge nearby rises",
            PercussionSetting::RearmMs => "Ready again after",
            PercussionSetting::FeatureMs => "Measure attack for",
            PercussionSetting::ConcentrationMs => "Sharpness time span",
            PercussionSetting::FftSize => "Fast frequency window",
            PercussionSetting::HopMs => "Measure every",
            PercussionSetting::MemorySeconds => "Keep recent attacks",
            PercussionSetting::MinReference => "Learn before ranking",
            _ => setting.label(),
        },
        _ => parameter.label(),
    }
}

fn decision_caption(reason: crate::visualizer::DecisionReason) -> &'static str {
    use crate::visualizer::DecisionReason as R;
    match reason {
        R::Eligible => "triggered",
        R::Provisional => "triggered while learning initial history",
        R::Numerical => "invalid measurement",
        R::Overlap => "attacks overlap",
        R::NegligibleGrowth => "too little new rise",
        R::AdmissionEnergy => "too little input energy",
        R::Prominence => "too little input contrast",
        R::PhysicalSharpness => "attack too smooth",
        R::OutputProminence => "too little trigger contrast",
        R::AttackLevel => "too little trigger energy",
        R::MixRms => "music too quiet",
        R::SharpnessPercentile => "below recent sharpness cutoff",
        R::EnergyPercentile => "below recent energy cutoff",
        R::Refractory => "too soon after previous trigger",
        R::Capacity => "too many attacks pending",
    }
}

fn rank_label(rank: Option<RankInterval>) -> String {
    rank.map_or_else(
        || "undefined".to_owned(),
        |rank| {
            if rank.upper - rank.lower > 0.999 {
                "all tied".to_owned()
            } else {
                format!("{:.0}–{:.0}%", rank.lower * 100.0, rank.upper * 100.0)
            }
        },
    )
}

fn attack_db(energy: f32, duration: f32) -> f32 {
    10.0 * (energy / duration.max(1e-6)).max(1e-12).log10()
}

fn stereo_envelope_amplitudes(point: &crate::visualizer::EnvelopePoint) -> [f32; 2] {
    if point.left_amplitude == 0.0 && point.right_amplitude == 0.0 && point.amplitude > 0.0 {
        // Older diagnostic points contain only the physical combined level.
        [point.amplitude; 2]
    } else {
        [point.left_amplitude, point.right_amplitude]
    }
}

fn stereo_backgrounds(event: &PercussionDecision) -> [f32; 2] {
    if event.background_stereo_power == [0.0; 2] && event.background_power > 0.0 {
        [event.background_power; 2]
    } else {
        event.background_stereo_power
    }
}

fn percussion_control_groups() -> [(&'static str, &'static str, &'static [PercussionSetting]); 6] {
    use PercussionSetting as P;
    [
        (
            "pipeline_frequencies",
            "CHOOSE FREQUENCIES",
            &[P::LowHz, P::HighHz, P::TaperHz, P::FftSize, P::HopMs],
        ),
        (
            "pipeline_candidates",
            "FIND ATTACKS",
            &[P::EnvelopeMs, P::BackgroundMs, P::MergeMs, P::RearmMs],
        ),
        (
            "pipeline_measure",
            "MEASURE EACH ATTACK",
            &[P::FeatureMs, P::ConcentrationMs],
        ),
        (
            "pipeline_input",
            "FILTER INPUTS FOR LEARNING",
            &[
                P::RejectOverlap,
                P::AdmissionGrowth,
                P::AdmissionEnergy,
                P::AdmissionProminence,
            ],
        ),
        (
            "pipeline_history",
            "RECENT HISTORY",
            &[P::MemorySeconds, P::MinReference],
        ),
        (
            "pipeline_trigger",
            "FILTER TRIGGERS",
            &[
                P::MinSharpness,
                P::MinOutputProminence,
                P::MinAttackDb,
                P::OutputRmsDb,
                P::SharpnessPercentile,
                P::EnergyPercentile,
                P::OutputIntervalMs,
            ],
        ),
    ]
}

fn heatmap_position(
    plot: Rect,
    reference: &PercussionReferenceSnapshot,
    sharpness: f32,
    energy: f32,
) -> Point {
    let db = attack_db(energy, reference.thresholds.feature_seconds.max(1e-9));
    Point::new(
        plot.x0 + f64::from(sharpness.clamp(0.0, 1.0)) * plot.width(),
        plot.y1
            - f64::from(
                ((db - reference.energy_min_db)
                    / (reference.energy_max_db - reference.energy_min_db).max(1e-9))
                .clamp(0.0, 1.0),
            ) * plot.height(),
    )
}

fn heatmap_recent_points(
    reference: &PercussionReferenceSnapshot,
    requested: usize,
) -> &[PercussionReferencePoint] {
    let length = reference.recent_count.min(reference.recent_points.len());
    &reference.recent_points[length.saturating_sub(requested)..length]
}

fn heatmap_marker_cluster(
    plot: Rect,
    reference: &PercussionReferenceSnapshot,
    requested: usize,
) -> Option<(Point, usize)> {
    let markers = heatmap_recent_points(reference, requested);
    let distance = plot.height() / HEATMAP_HEIGHT * 3.0;
    markers
        .iter()
        .map(|marker| {
            let point = heatmap_position(plot, reference, marker.sharpness, marker.energy);
            let count = markers
                .iter()
                .filter(|other| {
                    heatmap_position(plot, reference, other.sharpness, other.energy).distance(point)
                        <= distance
                })
                .count();
            (point, count)
        })
        .filter(|(_, count)| *count > 1)
        .max_by_key(|(_, count)| *count)
}

#[allow(clippy::too_many_arguments)]
fn segmented_rule(
    scene: &mut Scene,
    start: Point,
    end: Point,
    color: Color,
    width: f64,
    dash: f64,
    gap: f64,
) {
    let distance = start.distance(end);
    if distance <= 0.0 {
        return;
    }
    let direction = (end - start) / distance;
    let mut offset = 0.0;
    while offset < distance {
        rule(
            scene,
            start + direction * offset,
            start + direction * (offset + dash).min(distance),
            color,
            width,
        );
        offset += dash + gap;
    }
}

fn frequency_label(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:.1}k", hz / 1000.0)
    } else {
        format!("{hz:.0}")
    }
}

fn spectrum_fraction(hz: f32) -> f32 {
    ((hz.max(20.0) / 20.0).ln() / (16_000.0_f32 / 20.0).ln()).clamp(0.0, 1.0)
}

fn spectrum_frequency(fraction: f32) -> f32 {
    20.0 * (16_000.0_f32 / 20.0).powf(fraction.clamp(0.0, 1.0))
}

/// The outside graph anchors and inside control banks depend only on viewport
/// geometry. Infographics can overlap the graphs without moving the controls.
fn anchored_panels(stage: &Stage) -> Option<[Panel; 4]> {
    if stage.height < 200.0 {
        return None;
    }
    let available = (stage.width - stage.margin * 2.0) * 0.5 - stage.px(12.0);
    if available < stage.px(320.0) {
        return None;
    }
    let height_budget = (stage.height - stage.margin * 2.0 - stage.px(140.0).max(100.0)).max(1.0);
    let scale = (available / stage.px(672.0))
        .min(height_budget / stage.px(ONSET_PANEL_HEIGHT.max(SHAPE_PANEL_HEIGHT)))
        .min(1.0);
    let width = stage.px(440.0) * scale;
    let bank = stage.px(220.0) * scale;
    let gap = stage.px(12.0) * scale;
    let top = stage.height * 0.61;
    let right_bottom = stage.height - stage.margin - stage.px(140.0).max(100.0);
    let right_top = top
        .min(right_bottom - stage.px(ONSET_PANEL_HEIGHT) * scale)
        .max(stage.margin);
    let panel = |x: f64, height: f64, width: f64| {
        let height = stage.px(height) * scale;
        let min_y = stage.margin;
        let max_y = right_bottom - height;
        let y = top.clamp(min_y, max_y.max(min_y));
        Panel {
            rect: Rect::new(x, y, x + width, y + height),
            scale,
        }
    };
    let left = stage.margin;
    let right = stage.width - stage.margin - width;
    let control_unit = (stage.unit * scale)
        .max(0.6)
        .min((right_bottom - stage.margin) / CONTROL_PANEL_HEIGHT);
    let control_scale = control_unit / stage.unit;
    let control_bank = 220.0 * control_unit;
    let control_top = right_top
        .min(right_bottom - CONTROL_PANEL_HEIGHT * control_unit)
        .max(stage.margin);
    let shape = panel(left, SHAPE_PANEL_HEIGHT, width);
    let mut gap_controls = panel(left + width + gap, SHAPE_PANEL_HEIGHT, bank);
    gap_controls.rect = gap_controls
        .rect
        .with_origin(Point::new(gap_controls.rect.x0, shape.rect.y0));
    Some([
        shape,
        gap_controls,
        Panel {
            rect: Rect::new(
                right - gap - control_bank,
                control_top,
                right - gap,
                control_top + CONTROL_PANEL_HEIGHT * control_unit,
            ),
            scale: control_scale,
        },
        Panel {
            rect: Rect::new(
                right,
                right_top,
                right + width,
                right_top + stage.px(ONSET_PANEL_HEIGHT) * scale,
            ),
            scale,
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Size;

    fn event(id: u64, count: usize) -> PercussionDecision {
        let mut reference = PercussionReferenceSnapshot {
            count,
            ..PercussionReferenceSnapshot::default()
        };
        reference.sharpness_histogram[12] = count as u32;
        reference.energy_histogram[18] = count as u32;
        reference.sharpness_cutoff = Some(0.5);
        reference.energy_cutoff = Some(0.000_1);
        PercussionDecision {
            id,
            candidate_id: id,
            channel: 0,
            epoch: 0,
            onset_time: id as f64,
            completion_time: id as f64 + 0.024,
            emission_time: id as f64 + 0.024,
            sharpness: 0.5,
            energy: 0.000_1,
            absolute_energy: 0.000_2,
            growth: 0.1,
            prominence: 1.0,
            background_power: 0.000_01,
            background_stereo_power: [0.000_01; 2],
            peak_amplitude: 0.1,
            mix_rms_db: -20.0,
            admitted: true,
            eligible: true,
            provisional: false,
            rise_censored: false,
            overlapping: false,
            tolerance_used: true,
            reason: crate::visualizer::DecisionReason::Eligible,
            sharpness_rank: Some(RankInterval {
                lower: 0.0,
                upper: 1.0,
            }),
            energy_rank: Some(RankInterval {
                lower: 0.0,
                upper: 1.0,
            }),
            reference,
            envelope: [crate::visualizer::EnvelopePoint::default(); 64],
            envelope_len: 0,
            feature_seconds: 0.024,
            support_seconds: 0.010_667,
        }
    }

    #[test]
    fn stereo_envelope_diagnostics_keep_opposite_side_motion_and_frozen_backgrounds() {
        let mut saved = event(1, 3);
        saved.background_power = 0.02;
        saved.background_stereo_power = [0.04, 0.0];
        saved.envelope_len = 2;
        saved.envelope[0] = crate::visualizer::EnvelopePoint {
            seconds: 0.0,
            amplitude: 0.02_f32.sqrt(),
            power: 0.02,
            left_amplitude: 0.2,
            right_amplitude: 0.0,
            cumulative_growth: 0.0,
        };
        saved.envelope[1] = crate::visualizer::EnvelopePoint {
            seconds: saved.feature_seconds,
            amplitude: 0.02_f32.sqrt(),
            power: 0.02,
            left_amplitude: 0.1,
            right_amplitude: 0.03_f32.sqrt(),
            cumulative_growth: 0.015_f32.sqrt(),
        };
        assert_eq!(saved.envelope[0].amplitude, saved.envelope[1].amplitude);
        assert!(
            stereo_envelope_amplitudes(&saved.envelope[1])[0]
                < stereo_envelope_amplitudes(&saved.envelope[0])[0]
        );
        assert!(
            stereo_envelope_amplitudes(&saved.envelope[1])[1]
                > stereo_envelope_amplitudes(&saved.envelope[0])[1]
        );
        assert_eq!(stereo_backgrounds(&saved), [0.04, 0.0]);
        let mut overlay = AnalysisOverlay::default();
        overlay.decisions.push_back(saved.clone());
        overlay.inspected_id = Some(1);
        overlay.saved_reference = true;
        overlay.decisions.push_back(event(2, 15));
        assert_eq!(overlay.viewed_decision().unwrap().envelope, saved.envelope);
        assert_eq!(
            stereo_backgrounds(overlay.viewed_decision().unwrap()),
            [0.04, 0.0]
        );
    }

    #[test]
    fn historical_event_uses_its_saved_population_after_live_reference_changes() {
        let mut overlay = AnalysisOverlay::default();
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        overlay.right = Some(anchored_panels(&stage).unwrap()[3]);
        overlay.decisions.push_back(event(1, 3));
        overlay.decisions.push_back(event(2, 12));
        overlay.reference.count = 50;
        let saved = overlay.decisions[0].reference;
        let previous = overlay.event_buttons().unwrap()[0].center();
        assert!(matches!(
            overlay.pointer_down(previous),
            Some(AnalysisInteraction::Handled)
        ));
        assert_eq!(overlay.inspected_id, Some(1));
        assert_eq!(overlay.viewed_reference(), saved);
        overlay.reference.count = 100;
        overlay.decisions.push_back(event(3, 80));
        assert_eq!(overlay.viewed_reference(), saved);
        assert_eq!(
            overlay
                .viewed_decision()
                .unwrap()
                .sharpness_rank
                .unwrap()
                .upper,
            1.0
        );
        assert_eq!(overlay.selected_detector(), Detector::Kick);
        overlay.pointer_down(overlay.event_buttons().unwrap()[2].center());
        assert_eq!(overlay.viewed_decision().unwrap().id, 3);
        assert!(!overlay.saved_reference);
        assert_eq!(overlay.viewed_reference().count, 100);
        overlay.pointer_down(previous);
        assert!(overlay.saved_reference);
        assert_eq!(overlay.viewed_reference().count, 12);
        assert_eq!(
            rank_label(Some(RankInterval {
                lower: 0.0,
                upper: 1.0
            })),
            "all tied"
        );
        assert_eq!(rank_label(None), "undefined");
    }

    #[test]
    fn gap_controls_fit_beside_the_graph_section_they_adjust() {
        let mut fonts = Fonts::new().unwrap();
        let visualizer = Visualizer::new();
        for size in [Size::new(1920.0, 1080.0), Size::new(1280.0, 720.0)] {
            let stage = Stage::new(size);
            let mut overlay = AnalysisOverlay::default();
            overlay.update(
                0.0,
                visualizer.debug_history(),
                &mut fonts,
                &stage,
                &visualizer,
            );
            let panel = overlay.gap_controls.expect("gap controls are visible");
            let graph = overlay.left.expect("gap graphs are visible");
            assert_eq!(panel.rect.y0, graph.rect.y0);
            assert_eq!(panel.scale, graph.scale);
            let unit = stage.unit * panel.scale;
            let sliders: Vec<_> = overlay
                .sliders
                .iter()
                .filter_map(|slider| match slider.parameter {
                    Parameter::Arrangement(setting) => Some((slider, setting)),
                    _ => None,
                })
                .collect();
            assert_eq!(sliders.len(), ArrangementSetting::ALL.len());
            for setting in ArrangementSetting::ALL {
                assert_eq!(
                    sliders
                        .iter()
                        .filter(|(_, exposed)| *exposed == setting)
                        .count(),
                    1,
                    "each gap control appears once: {setting:?} at {size:?}"
                );
            }
            for (slider, setting) in &sliders {
                let (section_top, section_bottom) = match setting {
                    ArrangementSetting::ReferenceTime => (0.0, 165.0),
                    ArrangementSetting::ShapeDistance
                    | ArrangementSetting::TrendChange
                    | ArrangementSetting::ChangedFraction => (165.0, 286.0),
                    ArrangementSetting::MixLossDb
                    | ArrangementSetting::MixLossHold
                    | ArrangementSetting::GapRmsDb
                    | ArrangementSetting::GapRmsHold => (286.0, 533.0),
                    ArrangementSetting::GapHold | ArrangementSetting::DropRefractory => {
                        (533.0, 708.0)
                    }
                    ArrangementSetting::BassFastTime
                    | ArrangementSetting::BassSlowTime
                    | ArrangementSetting::BassHitThreshold
                    | ArrangementSetting::DropMinRmsDb => (708.0, SHAPE_PANEL_HEIGHT),
                };
                let top = (slider.rect.y0 - graph.rect.y0) / unit;
                let bottom = (slider.rect.y1 - graph.rect.y0) / unit;
                assert!(
                    top >= section_top - 1e-6 && bottom <= section_bottom + 1e-6,
                    "{setting:?} must stay beside its graph section at {size:?}"
                );
                assert!(panel.rect.contains(slider.rect.origin()));
                assert!(panel
                    .rect
                    .contains(Point::new(slider.rect.x1, slider.rect.y1)));
                assert!(slider.rect.x0 >= 0.0 && slider.rect.x1 <= size.width);
                assert!(slider.rect.y0 >= 0.0 && slider.rect.y1 <= size.height);
            }
        }
    }

    #[test]
    fn pipeline_shows_every_control_and_switch_without_pages() {
        let mut overlay = AnalysisOverlay::default();
        let mut fonts = Fonts::new().unwrap();
        let visualizer = Visualizer::new();
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        overlay.update(
            0.0,
            visualizer.debug_history(),
            &mut fonts,
            &stage,
            &visualizer,
        );
        let exposed = overlay
            .sliders
            .iter()
            .flat_map(|slider| {
                [
                    Some(slider.parameter),
                    slider.filter.map(|(parameter, _, _)| parameter),
                ]
            })
            .flatten()
            .collect::<Vec<_>>();
        for setting in PercussionSetting::CHANNEL {
            assert!(
                exposed.contains(&Parameter::Percussion(Detector::Kick, setting)),
                "missing {}",
                setting.label()
            );
        }
        for setting in [PercussionSetting::FftSize, PercussionSetting::HopMs] {
            assert!(exposed.contains(&Parameter::Percussion(Detector::Kick, setting)));
        }
        assert!(overlay.marker_count_buttons().is_some());
        assert_eq!(
            overlay
                .sliders
                .iter()
                .filter(|slider| matches!(slider.parameter, Parameter::Percussion(..)))
                .count(),
            24
        );
        let panel = overlay.onset_controls.unwrap().rect;
        assert!(overlay
            .sliders
            .iter()
            .filter(|slider| matches!(slider.parameter, Parameter::Percussion(..)))
            .all(|slider| panel.contains(slider.rect.origin())
                && panel.contains(Point::new(slider.rect.x1, slider.rect.y1))));
        assert!(overlay
            .sliders
            .iter()
            .all(|slider| !matches!(slider.parameter, Parameter::OnsetFlux(..))));
        assert_eq!(
            visualizer.detector_settings(),
            crate::visualizer::DetectorSettings::default()
        );
    }

    #[test]
    fn each_filter_switch_preserves_its_value_and_hover_help_fits() {
        let mut overlay = AnalysisOverlay::default();
        let mut fonts = Fonts::new().unwrap();
        let mut visualizer = Visualizer::new();
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        overlay.update(
            0.0,
            visualizer.debug_history(),
            &mut fonts,
            &stage,
            &visualizer,
        );
        let switches = overlay
            .sliders
            .iter()
            .filter_map(|slider| {
                slider
                    .filter
                    .map(|(toggle, _, rect)| (slider.parameter, toggle, rect.center()))
            })
            .collect::<Vec<_>>();
        assert_eq!(switches.len(), 11);
        for (scalar, toggle, center) in switches {
            let value = visualizer.parameter_value(scalar);
            let Some(AnalysisInteraction::Parameter(parameter, enabled)) =
                overlay.pointer_down(center)
            else {
                panic!("checkbox must be interactive");
            };
            assert_eq!(parameter, toggle);
            assert_eq!(enabled, 0.0);
            visualizer.set_parameter(parameter, enabled);
            overlay.update(
                0.0,
                visualizer.debug_history(),
                &mut fonts,
                &stage,
                &visualizer,
            );
            if scalar != toggle {
                assert_eq!(visualizer.parameter_value(scalar), value);
            }
            let hint = overlay
                .hover_hint
                .as_ref()
                .expect("checkbox has hover help");
            assert!(!hint.source.is_empty());
            assert!(
                hint.rect.x0 >= stage.margin && hint.rect.x1 <= stage.width - stage.margin + 1e-6
            );
            assert!(
                hint.rect.y0 >= stage.margin && hint.rect.y1 <= stage.height - stage.margin + 1e-6
            );
            let Some(AnalysisInteraction::Parameter(parameter, enabled)) =
                overlay.pointer_down(center)
            else {
                panic!("disabled checkbox must still work");
            };
            assert_eq!(enabled, 1.0);
            visualizer.set_parameter(parameter, enabled);
            overlay.update(
                0.0,
                visualizer.debug_history(),
                &mut fonts,
                &stage,
                &visualizer,
            );
        }
    }

    #[test]
    fn live_heatmap_hides_candidate_from_an_incompatible_feature_epoch() {
        let mut overlay = AnalysisOverlay::default();
        overlay.decisions.push_back(event(1, 3));
        overlay.current_epoch = 1;
        assert!(overlay.heatmap_candidate().is_none());
        overlay.saved_reference = true;
        assert_eq!(overlay.heatmap_candidate().unwrap().id, 1);
        overlay.saved_reference = false;
        overlay.current_epoch = 0;
        assert_eq!(overlay.heatmap_candidate().unwrap().id, 1);
    }

    #[test]
    fn heatmap_recent_count_uses_last_additions_including_expired_points() {
        let mut reference = PercussionReferenceSnapshot {
            recent_count: 40,
            ..PercussionReferenceSnapshot::default()
        };
        for (index, point) in reference.recent_points[..40].iter_mut().enumerate() {
            point.candidate_id = index as u64 + 1;
            point.in_reference = index >= 30;
        }
        let shown = heatmap_recent_points(&reference, 32);
        assert_eq!(shown.len(), 32);
        assert_eq!(shown[0].candidate_id, 9);
        assert_eq!(shown.last().unwrap().candidate_id, 40);
        assert_eq!(shown.iter().filter(|point| point.in_reference).count(), 10);
        assert_eq!(heatmap_recent_points(&reference, 64).len(), 40);
        assert_eq!(heatmap_recent_points(&reference, 4)[0].candidate_id, 37);
    }

    #[test]
    fn heatmap_axes_use_saved_horizon_and_effective_output_bounds() {
        let mut reference = PercussionReferenceSnapshot::default();
        reference.thresholds.feature_seconds = 0.044;
        reference.thresholds.min_sharpness = 0.225;
        reference.thresholds.min_attack_db = -38.0;
        reference.thresholds.sharpness_effective_cutoff = 0.49;
        reference.thresholds.energy_effective_cutoff = 0.044 * 10.0_f32.powf(-30.0 / 10.0);
        let plot = Rect::new(10.0, 20.0, 410.0, 220.0);
        let halfway = heatmap_position(plot, &reference, 0.5, 0.044 * 1e-4);
        assert!((halfway.x - 210.0).abs() < 1e-6 && (halfway.y - 120.0).abs() < 1e-5);
        let effective = heatmap_position(
            plot,
            &reference,
            reference.thresholds.sharpness_effective_cutoff,
            reference.thresholds.energy_effective_cutoff,
        );
        assert!((effective.x - 206.0).abs() < 1e-4 && (effective.y - 95.0).abs() < 1e-4);
        let saved = reference;
        reference.thresholds.feature_seconds = 0.024;
        assert_ne!(
            heatmap_position(plot, &reference, 0.5, 0.044 * 1e-4),
            halfway
        );
        assert_eq!(heatmap_position(plot, &saved, 0.5, 0.044 * 1e-4), halfway);
    }

    #[test]
    fn recent_marker_buttons_persist_without_changing_channel_or_reference() {
        let mut visualizer = Visualizer::new();
        let mut overlay = AnalysisOverlay::default();
        let mut fonts = Fonts::new().unwrap();
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        overlay.update(
            0.0,
            visualizer.debug_history(),
            &mut fonts,
            &stage,
            &visualizer,
        );
        let saved = overlay.reference;
        let more = overlay.marker_count_center(true).unwrap();
        let Some(AnalysisInteraction::Parameter(parameter, value)) = overlay.pointer_down(more)
        else {
            panic!("marker count control must edit a persisted parameter");
        };
        assert_eq!(
            parameter,
            Parameter::Percussion(Detector::Kick, PercussionSetting::RecentMarkers)
        );
        assert_eq!(value, 36.0);
        visualizer.set_parameter(parameter, value);
        overlay.update(
            0.0,
            visualizer.debug_history(),
            &mut fonts,
            &stage,
            &visualizer,
        );
        assert_eq!(overlay.recent_markers, 36);
        assert_eq!(overlay.selected_detector(), Detector::Kick);
        assert_eq!(overlay.reference, saved);
        let settings = visualizer.detector_settings();
        let restored: crate::visualizer::DetectorSettings =
            ron::from_str(&ron::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.percussion.recent_markers, 36);
    }

    #[test]
    fn instruments_keep_their_anchors_when_selector_cards_overlap() {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let selector_log = Rect::new(22.0, 404.0, 482.0, 675.0);
        let [shape, gap_controls, onset_controls, onsets] = anchored_panels(&stage).unwrap();
        assert!(shape.rect.intersect(selector_log).area() > 0.0);
        for panel in [shape, gap_controls] {
            assert_eq!(panel.rect.y0, shape.rect.y0);
            assert_eq!(panel.scale, 1.0);
        }
        assert_eq!(shape.rect.height(), SHAPE_PANEL_HEIGHT);
        assert_eq!(shape.rect.y1, stage.height - stage.margin - stage.px(140.0));
        assert_eq!(onset_controls.rect.y0, onsets.rect.y0);
        assert_eq!(onsets.rect.height(), ONSET_PANEL_HEIGHT);
        assert_eq!(
            onsets.rect.y1,
            stage.height - stage.margin - stage.px(140.0)
        );
        assert!(gap_controls.rect.x0 > shape.rect.x1);
        assert!(onset_controls.rect.x1 < onsets.rect.x0);
        assert_eq!(shape.rect.x0, stage.margin);
        assert_eq!(onsets.rect.x1, stage.width - stage.margin);
    }

    #[test]
    fn fixed_diagnostic_columns_fit_the_viewport() {
        for (width, height) in [
            (1920.0, 1080.0),
            (1280.0, 720.0),
            (1024.0, 768.0),
            (900.0, 1440.0),
            (2560.0, 1080.0),
        ] {
            let stage = Stage::new(Size::new(width, height));
            let panels = anchored_panels(&stage).expect("diagnostics and controls fit");
            for panel in panels {
                assert!(panel.rect.x0 >= 0.0 && panel.rect.x1 <= width);
                assert!(panel.rect.y0 >= 0.0 && panel.rect.y1 <= height);
            }
            for pair in panels.windows(2) {
                assert!(pair[0].rect.x1 < pair[1].rect.x0);
            }
            let fixed_footer = stage.height - stage.margin - stage.px(140.0).max(100.0);
            assert!(panels[3].rect.y1 <= fixed_footer + 1e-6);
        }
    }

    #[test]
    fn graph_click_cycles_channels_and_slider_drag_stays_on_its_parameter() {
        let mut overlay = AnalysisOverlay::default();
        let mut fonts = Fonts::new().unwrap();
        let visualizer = Visualizer::new();
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        overlay.update(
            0.0,
            visualizer.debug_history(),
            &mut fonts,
            &stage,
            &visualizer,
        );
        let graph = overlay.onset_graph_bounds().unwrap();
        for detector in [Detector::Snare, Detector::Hat, Detector::Kick] {
            assert!(matches!(
                overlay.pointer_down(graph.center()),
                Some(AnalysisInteraction::Handled)
            ));
            assert_eq!(overlay.selected_detector(), detector);
            overlay.update(
                0.0,
                visualizer.debug_history(),
                &mut fonts,
                &stage,
                &visualizer,
            );
        }
        let parameter = Parameter::Arrangement(ArrangementSetting::MixLossDb);
        let center = overlay.slider_center(parameter).unwrap();
        assert!(
            matches!(overlay.pointer_down(center), Some(AnalysisInteraction::Parameter(p, _)) if p == parameter)
        );
        assert!(overlay.dragging());
        let (dragged, value) = overlay
            .pointer_move(Some(Point::new(-100.0, center.y)))
            .unwrap();
        assert_eq!(dragged, parameter);
        assert_eq!(value, parameter.range().0);
        overlay.pointer_up();
        assert!(!overlay.dragging());
        assert!(overlay.pointer_move(Some(center)).is_none());
    }

    #[test]
    fn actual_bin_faders_page_and_reset_without_switching_detector() {
        let mut overlay = AnalysisOverlay::default();
        let mut fonts = Fonts::new().unwrap();
        let mut visualizer = Visualizer::new();
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        overlay.update(
            0.0,
            visualizer.debug_history(),
            &mut fonts,
            &stage,
            &visualizer,
        );
        let first = overlay
            .first_bin_parameter()
            .expect("kick bins exist before audio starts");
        let center = overlay.slider_center(first).unwrap();
        let Some(AnalysisInteraction::Parameter(parameter, gain)) = overlay.pointer_down(center)
        else {
            panic!("bin fader must start gain editing");
        };
        assert_eq!(parameter, first);
        assert!((gain - 0.5).abs() < 1e-5);
        visualizer.set_parameter(parameter, gain);
        assert_eq!(overlay.selected_detector(), Detector::Kick);
        let (_, muted) = overlay
            .pointer_move(Some(Point::new(center.x, center.y + 1000.0)))
            .unwrap();
        assert_eq!(muted, 0.0);
        overlay.pointer_up();
        assert!(matches!(
            overlay.pointer_down(overlay.bin_reset_center().unwrap()),
            Some(AnalysisInteraction::ResetBins(Detector::Kick))
        ));
        overlay.pointer_down(overlay.onset_graph_bounds().unwrap().center());
        overlay.update(
            0.0,
            visualizer.debug_history(),
            &mut fonts,
            &stage,
            &visualizer,
        );
        assert_eq!(overlay.selected_detector(), Detector::Snare);
        let before = overlay.first_bin_parameter().unwrap();
        overlay.pointer_down(overlay.bin_next_center().unwrap());
        overlay.update(
            0.0,
            visualizer.debug_history(),
            &mut fonts,
            &stage,
            &visualizer,
        );
        assert_ne!(overlay.first_bin_parameter().unwrap(), before);
        let spectrum = overlay.spectrum_bounds().unwrap();
        overlay.pointer_down(Point::new(
            spectrum.x0 + spectrum.width() * f64::from(spectrum_fraction(10_000.0)),
            spectrum.center().y,
        ));
        overlay.update(
            0.0,
            visualizer.debug_history(),
            &mut fonts,
            &stage,
            &visualizer,
        );
        let Parameter::OnsetBinGain(Detector::Snare, address) =
            overlay.first_bin_parameter().unwrap()
        else {
            panic!("snare bin page");
        };
        assert!(address.center_hz() > 8000.0);
        assert_eq!(overlay.selected_detector(), Detector::Snare);
    }
}
