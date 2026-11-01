//! Plot-only presentation graphics. Live plots share the analyzer's measured
//! data and heatmap geometry. Attack plots follow the newest registered decision,
//! preserving each event's original envelope and backgrounds between updates.

use std::collections::VecDeque;

use vello::{
    kurbo::{Affine, Circle, Line, Point, Rect, Stroke},
    peniko::{Color, Fill},
    Scene,
};

use crate::{
    text::{Fonts, Text, UiFace},
    visualizer::{
        DebugHistory, DropStatus, PercussionDecision, PercussionReferenceSnapshot,
        SpectralAggression, Visualizer,
    },
};

use super::{
    analysis::{
        heatmap_cloud_bounds, heatmap_feature_areas, heatmap_position, heatmap_recent_points,
        stereo_backgrounds, stereo_envelope_amplitudes,
    },
    ScenePalette, Stage,
};

const HISTORY_SECONDS: f32 = 8.0;
const PLOT_LEFT: f64 = 30.0;
const PLOT_RIGHT: f64 = 4.0;
const PLOT_TOP: f64 = 24.0;
const PLOT_BOTTOM: f64 = 22.0;
const TITLE_BASELINE: f64 = 14.0;
const Y_AXIS_X: f64 = 10.0;
const Y_TICK_GAP: f64 = 4.0;
const X_AXIS_BASELINE: f64 = 16.0;

/// Shared by both composition schedulers so their chosen chart ratios exclude
/// exactly the same compact heading and legend space used by this renderer.
pub(super) const PLOT_GUTTER_WIDTH: f64 = PLOT_LEFT + PLOT_RIGHT;
pub(super) const PLOT_GUTTER_HEIGHT: f64 = PLOT_TOP + PLOT_BOTTOM;
/// Aggression is a progress instrument, so every scheduler reserves a wide,
/// shallow body instead of presenting its one-dimensional value in a square.
pub(super) const AGGRESSION_MIN_RATIO: f64 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlotKind {
    Heatmap(usize),
    Attack(usize),
    SpectralShape,
    Aggression,
    Energy,
    Bass,
    Onsets,
}

struct Labels {
    title: Text,
    x: Text,
    y: Text,
    x_min: Text,
    x_max: Text,
    y_min: Text,
    y_max: Text,
    legend: Option<Text>,
}

/// Recompose a changing numeric endpoint using cached glyph layouts. Feature
/// horizons can vary between candidates, including early rejected attacks.
struct NumberLabel {
    digits: [Text; 10],
    caption: String,
}

impl NumberLabel {
    fn new(fonts: &mut Fonts) -> Self {
        Self {
            digits: std::array::from_fn(|digit| {
                fonts.layout_ui(&digit.to_string(), 12.0, UiFace::Light)
            }),
            caption: String::new(),
        }
    }

    fn set_horizon(&mut self, seconds: Option<f32>) {
        self.caption = seconds
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .map_or_else(String::new, |seconds| format!("{:.0}", seconds * 1000.0));
    }

    fn width(&self) -> f64 {
        self.caption
            .bytes()
            .map(|digit| self.digits[usize::from(digit - b'0')].width())
            .sum()
    }

    fn draw_right(&self, scene: &mut Scene, right: f64, baseline: f64, unit: f64, color: Color) {
        let mut x = right - self.width() * unit;
        for digit in self.caption.bytes() {
            let text = &self.digits[usize::from(digit - b'0')];
            text.draw_left(
                scene,
                Affine::translate((x, baseline)) * Affine::scale(unit),
                color,
            );
            x += text.width() * unit;
        }
    }
}

#[derive(Clone, Copy)]
struct HistoryPoint {
    at: f32,
    status: DropStatus,
}

#[derive(Clone, Copy)]
struct OnsetPoint {
    at: f64,
    channel: usize,
    eligible: bool,
}

pub(super) struct ScenePlot {
    kind: PlotKind,
    labels: Labels,
    reference: PercussionReferenceSnapshot,
    attack: Option<Box<PercussionDecision>>,
    attack_horizon: Option<NumberLabel>,
    samples: VecDeque<HistoryPoint>,
    onsets: Vec<OnsetPoint>,
    shape: DropStatus,
    aggression: SpectralAggression,
    recent_markers: usize,
    now: f64,
}

impl ScenePlot {
    pub fn new(kind: PlotKind, visualizer: &Visualizer, fonts: &mut Fonts) -> Self {
        let attack = if let PlotKind::Attack(channel) = kind {
            latest_attack(
                visualizer.percussion_decisions(),
                channel,
                visualizer.percussion_channel_status(channel).epoch,
            )
            .cloned()
            .map(Box::new)
        } else {
            None
        };
        let channel_name = |channel: usize| ["KICK", "SNARE", "HAT"][channel];
        let (title, x, y, x_min, x_max, legend) = match kind {
            PlotKind::Heatmap(channel) => (
                channel_name(channel).to_owned(),
                "SHARPNESS",
                "ENERGY / dBFS",
                "0".to_owned(),
                "1".to_owned(),
                None,
            ),
            PlotKind::Attack(channel) => (
                format!("{} ATTACK", channel_name(channel)),
                "TIME / ms",
                "AMPLITUDE",
                "0".to_owned(),
                String::new(),
                Some("L / R"),
            ),
            PlotKind::SpectralShape => (
                "SPECTRAL SHAPE".to_owned(),
                "FREQUENCY",
                "REL. LEVEL",
                frequency_caption(visualizer.spectral_band_edge_hz(0)),
                frequency_caption(visualizer.spectral_band_edge_hz(crate::visualizer::BAND_COUNT)),
                Some("LIVE / REF"),
            ),
            PlotKind::Aggression => (
                "AGGRESSION".to_owned(),
                "100% / 1x",
                "",
                "0% WARM".to_owned(),
                "200% / 2x".to_owned(),
                None,
            ),
            PlotKind::Energy => (
                "MIX ENERGY".to_owned(),
                "TIME",
                "dBFS",
                "-8 s".to_owned(),
                "NOW".to_owned(),
                Some("RMS / REF"),
            ),
            PlotKind::Bass => (
                "BASS RETURN".to_owned(),
                "TIME",
                "dB",
                "-8 s".to_owned(),
                "NOW".to_owned(),
                Some("FAST / SLOW"),
            ),
            PlotKind::Onsets => (
                "PERCUSSION".to_owned(),
                "TIME",
                "K / S / H",
                "-8 s".to_owned(),
                "NOW".to_owned(),
                None,
            ),
        };
        let (y_min, y_max) = match kind {
            PlotKind::Heatmap(channel) => {
                let reference = visualizer.percussion_reference(channel);
                (
                    format!("{:.0}", reference.energy_min_db),
                    format!("{:.0}", reference.energy_max_db),
                )
            }
            PlotKind::SpectralShape => ("-1".to_owned(), "+1".to_owned()),
            PlotKind::Energy => ("-60".to_owned(), "0".to_owned()),
            PlotKind::Bass => ("-80".to_owned(), "0".to_owned()),
            _ => (String::new(), String::new()),
        };
        let labels = Labels {
            title: fonts.layout_ui(&title, 16.0, UiFace::Light),
            x: fonts.layout_ui(x, 12.0, UiFace::Light),
            y: fonts.layout_ui(y, 12.0, UiFace::Light),
            x_min: fonts.layout_ui(&x_min, 12.0, UiFace::Light),
            x_max: fonts.layout_ui(&x_max, 12.0, UiFace::Light),
            y_min: fonts.layout_ui(&y_min, 12.0, UiFace::Light),
            y_max: fonts.layout_ui(&y_max, 12.0, UiFace::Light),
            legend: legend.map(|source| fonts.layout_ui(source, 12.0, UiFace::Light)),
        };
        let attack_horizon = if matches!(kind, PlotKind::Attack(_)) {
            let mut label = NumberLabel::new(fonts);
            label.set_horizon(attack.as_ref().map(|event| event.feature_seconds));
            Some(label)
        } else {
            None
        };
        let mut plot = Self {
            kind,
            labels,
            reference: PercussionReferenceSnapshot::default(),
            attack,
            attack_horizon,
            samples: VecDeque::new(),
            onsets: Vec::new(),
            shape: DropStatus::default(),
            aggression: SpectralAggression::default(),
            recent_markers: 32,
            now: 0.0,
        };
        plot.update(visualizer);
        plot
    }

    pub fn update(&mut self, visualizer: &Visualizer) {
        match self.kind {
            PlotKind::Attack(channel) => {
                self.update_attack(
                    visualizer.percussion_decisions(),
                    channel,
                    visualizer.percussion_channel_status(channel).epoch,
                );
            }
            PlotKind::Heatmap(channel) => {
                self.reference = visualizer.percussion_reference(channel);
                self.recent_markers = visualizer.detector_settings().percussion.recent_markers;
            }
            PlotKind::SpectralShape => {
                self.shape = visualizer
                    .debug_history()
                    .latest()
                    .map_or_else(DropStatus::default, |sample| sample.drop_status);
            }
            PlotKind::Aggression => self.aggression = visualizer.spectral_aggression(),
            PlotKind::Energy | PlotKind::Bass => self.update_history(visualizer.debug_history()),
            PlotKind::Onsets => {
                self.now = visualizer.percussion_audio_time();
                self.onsets.clear();
                self.onsets.extend(
                    visualizer
                        .percussion_decisions()
                        .iter()
                        .filter(|event| {
                            (0.0..=f64::from(HISTORY_SECONDS))
                                .contains(&(self.now - event.completion_time))
                        })
                        .map(|event| OnsetPoint {
                            at: event.completion_time,
                            channel: event.channel,
                            eligible: event.eligible,
                        }),
                );
            }
        }
    }

    fn update_attack(
        &mut self,
        decisions: &VecDeque<PercussionDecision>,
        channel: usize,
        epoch: u64,
    ) {
        let latest = latest_attack(decisions, channel, epoch);
        let identity = |event: &PercussionDecision| (event.epoch, event.id);
        if self.attack.as_deref().map(identity) == latest.map(identity) {
            return;
        }
        // Copy only at a new publication. Display refresh never remeasures an
        // attack, applies newer backgrounds, or clones the same large snapshot.
        self.attack = latest.cloned().map(Box::new);
        if let Some(label) = &mut self.attack_horizon {
            label.set_horizon(latest.map(|event| event.feature_seconds));
        }
    }

    fn update_history(&mut self, history: &DebugHistory) {
        let Some(latest) = history.latest() else {
            self.samples.clear();
            return;
        };
        if self.samples.back().is_some_and(|last| latest.at < last.at) {
            self.samples.clear();
        }
        self.now = f64::from(latest.at);
        let after = self
            .samples
            .back()
            .map_or(f32::NEG_INFINITY, |sample| sample.at);
        self.samples.extend(
            history
                .samples()
                .filter(|sample| sample.at > after && latest.at - sample.at <= HISTORY_SECONDS)
                .map(|sample| HistoryPoint {
                    at: sample.at,
                    status: sample.drop_status,
                }),
        );
        while self
            .samples
            .front()
            .is_some_and(|sample| latest.at - sample.at > HISTORY_SECONDS)
        {
            self.samples.pop_front();
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        scene: &mut Scene,
        rect: Rect,
        stage: &Stage,
        palette: ScenePalette,
        reveal: f32,
        alpha: f32,
        seed: u64,
    ) {
        if alpha <= 0.0 || reveal <= 0.0 {
            return;
        }
        let plot = plot_bounds(rect, stage);
        if plot.width() <= 0.0 || plot.height() <= 0.0 {
            return;
        }
        let palette = ScenePalette {
            ink: faded(palette.ink, alpha),
            accent: faded(palette.accent, alpha),
            muted: faded(palette.muted, alpha),
            ..palette
        };
        self.labels(scene, rect, plot, stage, palette, reveal, seed);
        if self.kind != PlotKind::Aggression {
            draw_grid(scene, plot, stage, palette, reveal);
        }
        match self.kind {
            PlotKind::Heatmap(_) => self.heatmap(scene, plot, stage, palette, reveal, seed),
            PlotKind::Attack(_) => self.envelope(scene, plot, stage, palette, reveal, seed),
            PlotKind::SpectralShape => {
                for (shape, color, dashed) in [
                    (&self.shape.reference_shape, faded(palette.muted, 0.7), true),
                    (&self.shape.shape, palette.ink, false),
                ] {
                    let points = shape.iter().enumerate().map(|(index, &value)| {
                        Point::new(
                            plot.x0 + index as f64 / (shape.len() - 1) as f64 * plot.width(),
                            plot.center().y
                                - f64::from(value.clamp(-1.0, 1.0)) * plot.height() * 0.5,
                        )
                    });
                    trace(scene, points, stage, color, reveal, seed, dashed);
                }
            }
            PlotKind::Aggression => self.aggression(scene, plot, stage, palette, reveal),
            PlotKind::Energy | PlotKind::Bass => {
                self.history(scene, plot, stage, palette, reveal, seed)
            }
            PlotKind::Onsets => self.onsets(scene, plot, stage, palette, reveal, seed),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn labels(
        &self,
        scene: &mut Scene,
        rect: Rect,
        plot: Rect,
        stage: &Stage,
        palette: ScenePalette,
        reveal: f32,
        seed: u64,
    ) {
        let alpha = (reveal * 2.0).min(1.0);
        let unit = stage.unit;
        let draw = |scene: &mut Scene, text: &Text, x: f64, y: f64, color: Color| {
            text.draw_left(
                scene,
                Affine::translate((x, y)) * Affine::scale(unit),
                faded(color, alpha),
            );
        };
        // Like the typography elsewhere in the scene, some headings briefly
        // flicker during their entrance/exit, then remain fully stable.
        let title_alpha = if reveal < 1.0 && seed & 1 != 0 {
            let pulse = ((reveal * 13.0).floor() as u64).wrapping_mul(0x9e37_79b9) ^ seed;
            if pulse & 3 == 0 {
                0.18
            } else {
                1.0
            }
        } else {
            1.0
        };
        draw(
            scene,
            &self.labels.title,
            plot.x0,
            rect.y0 + stage.px(TITLE_BASELINE),
            faded(palette.ink, title_alpha),
        );
        let baseline = plot.y1 + stage.px(X_AXIS_BASELINE);
        draw(scene, &self.labels.x_min, plot.x0, baseline, palette.muted);
        draw(
            scene,
            &self.labels.x_max,
            plot.x1 - self.labels.x_max.width() * unit,
            baseline,
            palette.muted,
        );
        if let Some(horizon) = &self.attack_horizon {
            horizon.draw_right(scene, plot.x1, baseline, unit, faded(palette.muted, alpha));
        }
        draw(
            scene,
            &self.labels.x,
            plot.center().x - self.labels.x.width() * unit * 0.5,
            baseline,
            palette.muted,
        );
        for (label, y) in [
            (&self.labels.y_max, plot.y0 + stage.px(8.0)),
            (&self.labels.y_min, plot.y1 + stage.px(3.0)),
        ] {
            draw(
                scene,
                label,
                plot.x0 - stage.px(Y_TICK_GAP) - label.width() * unit,
                y,
                palette.muted,
            );
        }
        self.labels.y.draw_left(
            scene,
            Affine::translate((rect.x0 + stage.px(Y_AXIS_X), plot.center().y))
                * Affine::rotate(-std::f64::consts::FRAC_PI_2)
                * Affine::scale(unit)
                * Affine::translate((-self.labels.y.width() * 0.5, 0.0)),
            faded(palette.muted, alpha),
        );
        if let Some(legend) = &self.labels.legend {
            draw(
                scene,
                legend,
                plot.x1 - legend.width() * unit - stage.px(5.0),
                plot.y0 + stage.px(15.0),
                palette.muted,
            );
        }
    }

    fn heatmap(
        &self,
        scene: &mut Scene,
        plot: Rect,
        stage: &Stage,
        palette: ScenePalette,
        reveal: f32,
        seed: u64,
    ) {
        let reference = &self.reference;
        for area in heatmap_feature_areas(plot, reference).into_iter().flatten() {
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                faded(palette.accent, 0.075 * reveal),
                None,
                &area,
            );
        }
        let peak = reference
            .joint_histogram
            .iter()
            .flatten()
            .copied()
            .max()
            .unwrap_or(1)
            .max(1) as f64;
        let cells = reference.joint_histogram.len();
        let width = plot.width() / cells as f64;
        let height = plot.height() / cells as f64;
        for (energy, row) in reference.joint_histogram.iter().enumerate() {
            for (sharpness, &count) in row.iter().enumerate() {
                if count == 0 {
                    continue;
                }
                let visible = segment_alpha(reveal, energy * cells + sharpness, seed);
                let intensity = 0.12 + 0.55 * ((f64::from(count) + 1.0).ln() / (peak + 1.0).ln());
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    faded(palette.ink, intensity as f32 * visible),
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
        if let Some(cloud) = heatmap_cloud_bounds(plot, reference) {
            let color = faded(palette.ink, 0.55 * reveal);
            if reference.cloud_boundary.is_some() {
                for y in [cloud.y0, cloud.y1] {
                    scene.stroke(
                        &dashed(stage, 0.9),
                        Affine::IDENTITY,
                        color,
                        None,
                        &Line::new((cloud.x0, y), (cloud.x1, y)),
                    );
                }
            }
            if reference.sharpness_cloud_boundary.is_some() {
                for x in [cloud.x0, cloud.x1] {
                    scene.stroke(
                        &dashed(stage, 0.9),
                        Affine::IDENTITY,
                        color,
                        None,
                        &Line::new((x, cloud.y0), (x, cloud.y1)),
                    );
                }
            }
        }
        let thresholds = reference.thresholds;
        if thresholds.filters.input_energy {
            let admission = heatmap_position(plot, reference, 0.0, thresholds.admission_energy);
            scene.stroke(
                &Stroke::new(stage.px(0.8)).with_dashes(0.0, [stage.px(1.0), stage.px(3.0)]),
                Affine::IDENTITY,
                faded(palette.muted, 0.7 * reveal),
                None,
                &Line::new((plot.x0, admission.y), (plot.x1, admission.y)),
            );
        }
        let floor = heatmap_position(
            plot,
            reference,
            thresholds.trigger_sharpness_floor,
            thresholds.trigger_energy_floor,
        );
        for (enabled, start, end) in [
            (
                thresholds.sharpness_floor_enabled(),
                Point::new(floor.x, plot.y0),
                Point::new(floor.x, plot.y1),
            ),
            (
                thresholds.energy_floor_enabled(),
                Point::new(plot.x0, floor.y),
                Point::new(plot.x1, floor.y),
            ),
        ] {
            if enabled {
                scene.stroke(
                    &dashed(stage, 1.0),
                    Affine::IDENTITY,
                    faded(palette.accent, 0.75 * reveal),
                    None,
                    &Line::new(start, end),
                );
            }
        }
        for (cutoff, vertical) in [
            (reference.sharpness_cutoff, true),
            (reference.energy_cutoff, false),
        ] {
            let Some(value) = cutoff else {
                continue;
            };
            let point = if vertical {
                heatmap_position(plot, reference, value, 0.0)
            } else {
                heatmap_position(plot, reference, 0.0, value)
            };
            let (start, end) = if vertical {
                (Point::new(point.x, plot.y0), Point::new(point.x, plot.y1))
            } else {
                (Point::new(plot.x0, point.y), Point::new(plot.x1, point.y))
            };
            rule(
                scene,
                start,
                end,
                faded(palette.ink, 0.7 * reveal),
                stage.px(1.1),
            );
        }
        let points = heatmap_recent_points(reference, self.recent_markers);
        for (index, marker) in points.iter().enumerate() {
            let visible = segment_alpha(reveal, index + 4096, seed);
            let color = faded(
                if marker.in_reference {
                    palette.accent
                } else {
                    palette.muted
                },
                visible * if marker.in_reference { 0.85 } else { 0.25 },
            );
            let point = heatmap_position(plot, reference, marker.sharpness, marker.energy);
            let radius = stage.px(if index + 1 == points.len() { 3.8 } else { 2.4 });
            let circle = Circle::new(point, radius);
            if marker.eligible {
                scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &circle);
            } else {
                scene.stroke(
                    &Stroke::new(stage.px(1.0)),
                    Affine::IDENTITY,
                    color,
                    None,
                    &circle,
                );
            }
        }
    }

    fn envelope(
        &self,
        scene: &mut Scene,
        plot: Rect,
        stage: &Stage,
        palette: ScenePalette,
        reveal: f32,
        seed: u64,
    ) {
        let Some(event) = self.attack.as_deref() else {
            return;
        };
        let points = &event.envelope[..event.envelope_len.min(event.envelope.len())];
        let backgrounds = stereo_backgrounds(event);
        let maximum = points
            .iter()
            .flat_map(stereo_envelope_amplitudes)
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
            scene.stroke(
                &dashed(stage, 0.8),
                Affine::IDENTITY,
                faded(palette.muted, 0.6 * reveal),
                None,
                &Line::new((plot.x0, y), (plot.x1, y)),
            );
        }
        for (side, color, dashed) in [(0, palette.ink, false), (1, palette.accent, true)] {
            trace(
                scene,
                points
                    .iter()
                    .map(|point| position(point.seconds, stereo_envelope_amplitudes(point)[side])),
                stage,
                color,
                reveal,
                seed,
                dashed,
            );
        }
    }

    fn aggression(
        &self,
        scene: &mut Scene,
        plot: Rect,
        stage: &Stage,
        palette: ScenePalette,
        reveal: f32,
    ) {
        let rail = aggression_rail(plot, stage);
        let amount = f64::from((self.aggression.value / 2.0).clamp(0.0, 1.0));
        let bar = Rect::new(
            rail.x0,
            rail.y0,
            rail.x0 + rail.width() * amount * f64::from(reveal),
            rail.y1,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            faded(palette.muted, 0.2 * reveal),
            None,
            &rail,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            faded(palette.accent, 0.85 * reveal),
            None,
            &bar,
        );
        for x in [rail.x0, rail.center().x, rail.x1] {
            rule(
                scene,
                Point::new(x, rail.y0 - stage.px(2.0)),
                Point::new(x, rail.y1 + stage.px(2.0)),
                faded(palette.ink, 0.5 * reveal),
                stage.px(0.9),
            );
        }
    }

    fn history(
        &self,
        scene: &mut Scene,
        plot: Rect,
        stage: &Stage,
        palette: ScenePalette,
        reveal: f32,
        seed: u64,
    ) {
        let bass = self.kind == PlotKind::Bass;
        let (low, high) = if bass { (-80.0, 0.0) } else { (-60.0, 0.0) };
        let position = |sample: &HistoryPoint, value: f32| {
            Point::new(
                plot.x1
                    - (self.now - f64::from(sample.at)) / f64::from(HISTORY_SECONDS) * plot.width(),
                plot.y1 - f64::from(((value - low) / (high - low)).clamp(0.0, 1.0)) * plot.height(),
            )
        };
        for (line, color, dashed) in [
            (0, palette.ink, false),
            (1, palette.muted, true),
            (2, palette.accent, true),
        ] {
            let points = self.samples.iter().map(|sample| {
                let status = sample.status;
                let value = if bass {
                    [
                        status.bass_fast_db,
                        status.bass_slow_db,
                        status.bass_onset_threshold_db,
                    ][line]
                } else {
                    [
                        status.rms_db,
                        status.envelope_db - status.energy_delta_db,
                        status.gap_rms_threshold_db,
                    ][line]
                };
                position(sample, value)
            });
            trace(scene, points, stage, color, reveal, seed, dashed);
        }
    }

    fn onsets(
        &self,
        scene: &mut Scene,
        plot: Rect,
        stage: &Stage,
        palette: ScenePalette,
        reveal: f32,
        seed: u64,
    ) {
        let row = plot.height() / 3.0;
        for event in &self.onsets {
            let x = plot.x1 - (self.now - event.at) / f64::from(HISTORY_SECONDS) * plot.width();
            let y = plot.y0 + (event.channel.min(2) as f64 + 1.0) * row;
            let height = row * if event.eligible { 0.8 } else { 0.35 };
            let color = if event.eligible {
                palette.accent
            } else {
                faded(palette.muted, 0.5)
            };
            rule(
                scene,
                Point::new(x, y),
                Point::new(x, y - height),
                faded(
                    color,
                    segment_alpha(reveal, (event.at * 1000.0) as usize, seed),
                ),
                stage.px(1.4),
            );
        }
    }
}

/// The layout scheduler adds these gutters around each chosen chart ratio.
pub(super) fn plot_bounds(rect: Rect, stage: &Stage) -> Rect {
    Rect::new(
        rect.x0 + stage.px(PLOT_LEFT),
        rect.y0 + stage.px(PLOT_TOP),
        rect.x1 - stage.px(PLOT_RIGHT),
        rect.y1 - stage.px(PLOT_BOTTOM),
    )
}

fn aggression_rail(plot: Rect, stage: &Stage) -> Rect {
    let half_height = stage.px(3.0).min(plot.height() * 0.5);
    Rect::new(
        plot.x0,
        plot.center().y - half_height,
        plot.x1,
        plot.center().y + half_height,
    )
}

fn latest_attack(
    decisions: &VecDeque<PercussionDecision>,
    channel: usize,
    epoch: u64,
) -> Option<&PercussionDecision> {
    decisions
        .iter()
        .rev()
        .find(|event| event.channel == channel && event.epoch == epoch)
}

fn frequency_caption(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:.0} kHz", hz / 1000.0)
    } else {
        format!("{hz:.0} Hz")
    }
}

fn faded(color: Color, alpha: f32) -> Color {
    color.with_alpha(color.components[3] * alpha.clamp(0.0, 1.0))
}

fn dashed(stage: &Stage, width: f64) -> Stroke {
    Stroke::new(stage.px(width)).with_dashes(0.0, [stage.px(4.0), stage.px(3.0)])
}

fn rule(scene: &mut Scene, start: Point, end: Point, color: Color, width: f64) {
    scene.stroke(
        &Stroke::new(width),
        Affine::IDENTITY,
        color,
        None,
        &Line::new(start, end),
    );
}

fn draw_grid(scene: &mut Scene, plot: Rect, stage: &Stage, palette: ScenePalette, reveal: f32) {
    for index in 0..=4 {
        let fraction = index as f64 / 4.0;
        let x = plot.x0 + plot.width() * fraction;
        let y = plot.y0 + plot.height() * fraction;
        let progress = f64::from((reveal * 1.5 - index as f32 * 0.1).clamp(0.0, 1.0));
        let color = faded(
            palette.muted,
            if index == 0 || index == 4 { 0.5 } else { 0.17 },
        );
        rule(
            scene,
            Point::new(x, plot.y0),
            Point::new(x, plot.y0 + plot.height() * progress),
            color,
            stage.px(0.7),
        );
        rule(
            scene,
            Point::new(plot.x0, y),
            Point::new(plot.x0 + plot.width() * progress, y),
            color,
            stage.px(0.7),
        );
    }
}

fn segment_alpha(reveal: f32, index: usize, seed: u64) -> f32 {
    let mut hash = seed.wrapping_add((index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15));
    hash = (hash ^ (hash >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    hash = (hash ^ (hash >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    hash ^= hash >> 31;
    let delay = (hash >> 40) as f32 / (1_u32 << 24) as f32 * 0.8;
    ((reveal - delay) / 0.2).clamp(0.0, 1.0)
}

#[allow(clippy::too_many_arguments)]
fn trace(
    scene: &mut Scene,
    points: impl Iterator<Item = Point>,
    stage: &Stage,
    color: Color,
    reveal: f32,
    seed: u64,
    is_dashed: bool,
) {
    let stroke = if is_dashed {
        dashed(stage, 1.0)
    } else {
        Stroke::new(stage.px(1.4))
    };
    let mut previous = None;
    for (index, point) in points.enumerate() {
        if let Some(start) = previous {
            scene.stroke(
                &stroke,
                Affine::IDENTITY,
                faded(color, segment_alpha(reveal, index, seed)),
                None,
                &Line::new(start, point),
            );
        }
        previous = Some(point);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Size;

    fn decision(id: u64, channel: usize, epoch: u64, eligible: bool) -> PercussionDecision {
        PercussionDecision {
            id,
            candidate_id: id,
            channel,
            epoch,
            onset_time: id as f64,
            completion_time: id as f64 + 0.024,
            emission_time: id as f64 + 0.024,
            sharpness: 0.5,
            energy: 0.000_1,
            absolute_energy: 0.000_2,
            growth: 0.1,
            prominence: 1.0,
            background_power: 0.000_01,
            background_stereo_power: [0.000_01, 0.000_02],
            peak_amplitude: 0.1,
            mix_rms_db: -20.0,
            admitted: true,
            eligible,
            provisional: false,
            overlapping: false,
            rise_censored: false,
            tolerance_used: false,
            reason: if eligible {
                crate::visualizer::DecisionReason::Eligible
            } else {
                crate::visualizer::DecisionReason::Refractory
            },
            sharpness_rank: None,
            energy_rank: None,
            reference: PercussionReferenceSnapshot::default(),
            envelope: [crate::visualizer::EnvelopePoint::default(); 64],
            envelope_len: 2,
            feature_seconds: 0.024,
            support_seconds: 0.010_667,
        }
    }

    #[test]
    fn live_attack_follows_latest_channel_decision_without_remeasuring_saved_evidence(
    ) -> anyhow::Result<()> {
        let mut decisions = VecDeque::from([
            decision(1, 0, 1, true),
            decision(2, 0, 2, true),
            decision(3, 1, 2, true),
            decision(4, 0, 2, false),
            decision(5, 0, 1, true),
        ]);
        decisions[3].envelope[1].left_amplitude = 0.2;
        assert_eq!(
            latest_attack(&decisions, 0, 2).unwrap().id,
            4,
            "rejected candidates are displayed just like the live analyzer"
        );
        assert!(latest_attack(&decisions, 2, 2).is_none());
        let visualizer = Visualizer::new();
        let mut fonts = Fonts::new()?;
        let mut plot = ScenePlot::new(PlotKind::Attack(0), &visualizer, &mut fonts);
        plot.update_attack(&decisions, 0, 2);
        let saved = plot.attack.as_deref().unwrap() as *const PercussionDecision;

        // Already-published evidence is immutable. Even if its source storage
        // changed, a display refresh must keep the same owned snapshot.
        decisions[3].envelope[1].left_amplitude = 0.9;
        decisions[3].background_stereo_power = [0.5, 0.8];
        decisions.push_back(decision(6, 1, 2, true));
        plot.update_attack(&decisions, 0, 2);
        let displayed = plot.attack.as_ref().expect("saved attack retained");
        assert_eq!(
            displayed.as_ref() as *const PercussionDecision,
            saved,
            "the same publication must not be cloned on every frame"
        );
        assert_eq!(displayed.id, 4);
        assert_eq!(displayed.envelope[1].left_amplitude, 0.2);
        assert_eq!(displayed.background_stereo_power, [0.000_01, 0.000_02]);
        assert_eq!(plot.attack_horizon.as_ref().unwrap().caption, "24");

        let mut next = decision(7, 0, 2, false);
        next.feature_seconds = 0.044;
        next.envelope[1].left_amplitude = 0.4;
        next.background_stereo_power = [0.002, 0.003];
        decisions.push_back(next);
        plot.update_attack(&decisions, 0, 2);
        let displayed = plot.attack.as_ref().unwrap();
        assert_eq!(displayed.id, 7);
        assert_eq!(displayed.envelope[1].left_amplitude, 0.4);
        assert_eq!(displayed.background_stereo_power, [0.002, 0.003]);
        assert_eq!(plot.attack_horizon.as_ref().unwrap().caption, "44");
        Ok(())
    }

    #[test]
    fn live_attack_clears_old_epoch_evidence_and_accepts_restarted_publications(
    ) -> anyhow::Result<()> {
        let visualizer = Visualizer::new();
        let mut fonts = Fonts::new()?;
        let mut plot = ScenePlot::new(PlotKind::Attack(0), &visualizer, &mut fonts);
        let mut decisions = VecDeque::from([decision(1, 0, 2, true)]);
        plot.update_attack(&decisions, 0, 2);
        assert!(plot.attack.is_some());

        plot.update_attack(&decisions, 0, 3);
        assert!(
            plot.attack.is_none(),
            "an old-epoch hit cannot leak into the new analysis"
        );
        assert!(plot.attack_horizon.as_ref().unwrap().caption.is_empty());

        let mut restarted = decision(1, 0, 3, false);
        restarted.feature_seconds = 0.004;
        decisions.push_back(restarted);
        plot.update_attack(&decisions, 0, 3);
        assert_eq!(plot.attack.as_ref().unwrap().epoch, 3);
        assert_eq!(plot.attack_horizon.as_ref().unwrap().caption, "4");

        decisions.clear();
        plot.update_attack(&decisions, 0, 3);
        assert!(plot.attack.is_none());
        assert!(plot.attack_horizon.as_ref().unwrap().caption.is_empty());
        Ok(())
    }

    #[test]
    fn live_history_tracks_new_measurements_expires_old_data_and_resets() -> anyhow::Result<()> {
        let mut visualizer = Visualizer::new();
        let mut first = visualizer.debug_sample(1.0);
        first.drop_status.rms_db = -24.0;
        visualizer.debug_history.samples.push_back(first);
        let mut fonts = Fonts::new()?;
        let mut plot = ScenePlot::new(PlotKind::Energy, &visualizer, &mut fonts);
        assert_eq!(plot.samples.len(), 1);
        assert_eq!(plot.samples.back().unwrap().status.rms_db, -24.0);

        let mut next = visualizer.debug_sample(2.0);
        next.drop_status.rms_db = -12.0;
        visualizer.debug_history.samples.push_back(next);
        plot.update(&visualizer);
        plot.update(&visualizer);
        assert_eq!(
            plot.samples.len(),
            2,
            "display refresh must not duplicate samples"
        );
        assert_eq!(plot.samples.back().unwrap().status.rms_db, -12.0);

        let latest = visualizer.debug_sample(11.0);
        visualizer.debug_history.samples.push_back(latest);
        plot.update(&visualizer);
        assert_eq!(
            plot.samples.len(),
            1,
            "the chart retains only its eight-second window"
        );
        assert_eq!(plot.samples.front().unwrap().at, 11.0);

        visualizer.reset_audio();
        plot.update(&visualizer);
        assert!(plot.samples.is_empty());
        let restarted = visualizer.debug_sample(0.5);
        visualizer.debug_history.samples.push_back(restarted);
        plot.update(&visualizer);
        assert_eq!(plot.samples.front().unwrap().at, 0.5);
        Ok(())
    }

    #[test]
    fn plot_gutters_preserve_all_three_heatmap_ratios() {
        for viewport in [Size::new(1920.0, 1080.0), Size::new(1280.0, 720.0)] {
            let stage = Stage::new(viewport);
            for ratio in [0.5, 1.0, 2.0] {
                let chart_width = stage.px(240.0);
                let outer = Rect::new(
                    0.0,
                    0.0,
                    chart_width + stage.px(PLOT_GUTTER_WIDTH),
                    chart_width / ratio + stage.px(PLOT_GUTTER_HEIGHT),
                );
                let chart = plot_bounds(outer, &stage);
                assert!((chart.width() / chart.height() - ratio).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn compact_gutters_keep_axis_ink_inside_and_ticks_apart_at_minimum_chart_width(
    ) -> anyhow::Result<()> {
        fn ink(text: &Text) -> Option<Rect> {
            if text.width() == 0.0 {
                return None;
            }
            let plan = text.plan();
            let baseline = plan.lines[0].baseline;
            Some(Rect::new(
                0.0,
                plan.ink_top - baseline,
                text.width(),
                plan.ink_bottom - baseline,
            ))
        }
        fn inside(outer: Rect, inner: Rect) -> bool {
            inner.x0 >= outer.x0
                && inner.y0 >= outer.y0
                && inner.x1 <= outer.x1
                && inner.y1 <= outer.y1
        }
        fn disjoint(a: Rect, b: Rect) -> bool {
            a.x1 <= b.x0 || b.x1 <= a.x0 || a.y1 <= b.y0 || b.y1 <= a.y0
        }
        let visualizer = Visualizer::new();
        let mut fonts = Fonts::new()?;
        let light_font =
            include_bytes!("../../../assets/fonts/Iosevka/IosevkaTightSoft-Light-subset.ttf")
                .as_slice();
        for viewport in [Size::new(1920.0, 1080.0), Size::new(1280.0, 720.0)] {
            let stage = Stage::new(viewport);
            for kind in [
                PlotKind::Heatmap(0),
                PlotKind::Heatmap(1),
                PlotKind::Heatmap(2),
                PlotKind::Attack(0),
                PlotKind::Attack(1),
                PlotKind::Attack(2),
                PlotKind::SpectralShape,
                PlotKind::Aggression,
                PlotKind::Energy,
                PlotKind::Bass,
                PlotKind::Onsets,
            ] {
                let plot = ScenePlot::new(kind, &visualizer, &mut fonts);
                let ratio = match kind {
                    PlotKind::Heatmap(_) => 2.0,
                    PlotKind::Aggression => AGGRESSION_MIN_RATIO,
                    _ => 2.75,
                };
                let outer = Rect::new(
                    0.0,
                    0.0,
                    stage.px(180.0 + PLOT_GUTTER_WIDTH),
                    stage.px(180.0 / ratio + PLOT_GUTTER_HEIGHT),
                );
                let chart = plot_bounds(outer, &stage);
                let heading_plan = plot.labels.title.plan();
                assert!(
                    heading_plan.runs.iter().all(|run| {
                        run.font_size == 16.0 && run.font.data.as_ref() == light_font
                    }),
                    "{kind:?} heading must use the bundled thin face at 16px"
                );
                let heading_ink = (Affine::translate((chart.x0, stage.px(TITLE_BASELINE)))
                    * Affine::scale(stage.unit))
                .transform_rect_bbox(ink(&plot.labels.title).unwrap());
                assert!(
                    inside(outer, heading_ink),
                    "{kind:?} title escapes its reserved header"
                );
                assert!(
                    chart.y0 - heading_ink.y1 >= stage.px(9.8),
                    "{kind:?} title needs about ten units of clear air above the grid"
                );
                let footer_plan = plot.labels.x.plan();
                let footer_bottom = chart.y1
                    + stage.px(X_AXIS_BASELINE)
                    + (footer_plan.ink_bottom - footer_plan.lines[0].baseline) * stage.unit;
                assert!(
                    outer.y1 + heading_ink.y0 - footer_bottom >= stage.px(4.0),
                    "{kind:?} neighboring modules must keep their heading and axis ink apart"
                );
                let y_ink = ink(&plot.labels.y).map(|bounds| {
                    (Affine::translate((stage.px(Y_AXIS_X), chart.center().y))
                        * Affine::rotate(-std::f64::consts::FRAC_PI_2)
                        * Affine::scale(stage.unit)
                        * Affine::translate((-plot.labels.y.width() * 0.5, 0.0)))
                    .transform_rect_bbox(bounds)
                });
                if let Some(bounds) = y_ink {
                    assert!(
                        inside(outer, bounds),
                        "{kind:?} rotated legend escapes {bounds:?}"
                    );
                }
                for (tick, baseline) in [
                    (&plot.labels.y_max, chart.y0 + stage.px(8.0)),
                    (&plot.labels.y_min, chart.y1 + stage.px(3.0)),
                ] {
                    if let Some(bounds) = ink(tick) {
                        let bounds = (Affine::translate((
                            chart.x0 - stage.px(Y_TICK_GAP) - tick.width() * stage.unit,
                            baseline,
                        )) * Affine::scale(stage.unit))
                        .transform_rect_bbox(bounds);
                        assert!(inside(outer, bounds), "{kind:?} tick escapes {bounds:?}");
                        if let Some(y_ink) = y_ink {
                            assert!(
                                disjoint(bounds, y_ink),
                                "{kind:?} axis and endpoint overlap: {bounds:?} {y_ink:?}"
                            );
                        }
                    }
                }
                let baseline = chart.y1 + stage.px(X_AXIS_BASELINE);
                let mut x_ink = Vec::new();
                for (tick, x) in [
                    (&plot.labels.x_min, chart.x0),
                    (
                        &plot.labels.x,
                        chart.center().x - plot.labels.x.width() * stage.unit * 0.5,
                    ),
                    (
                        &plot.labels.x_max,
                        chart.x1 - plot.labels.x_max.width() * stage.unit,
                    ),
                ] {
                    if let Some(bounds) = ink(tick) {
                        let bounds = (Affine::translate((x, baseline)) * Affine::scale(stage.unit))
                            .transform_rect_bbox(bounds);
                        assert!(
                            inside(outer, bounds),
                            "{kind:?} x legend escapes {bounds:?}"
                        );
                        assert!(
                            x_ink.iter().all(|previous| disjoint(*previous, bounds)),
                            "{kind:?} x labels overlap"
                        );
                        x_ink.push(bounds);
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn aggression_is_a_thin_progress_rail_with_percent_labels_below_it() -> anyhow::Result<()> {
        let visualizer = Visualizer::new();
        let mut fonts = Fonts::new()?;
        let plot = ScenePlot::new(PlotKind::Aggression, &visualizer, &mut fonts);
        assert_eq!(
            plot.labels.y.width(),
            0.0,
            "a progress meter has no vertical axis"
        );
        for size in [Size::new(1920.0, 1080.0), Size::new(1280.0, 720.0)] {
            let stage = Stage::new(size);
            for width in [180.0, 224.0, 256.0] {
                let outer = Rect::new(
                    0.0,
                    0.0,
                    stage.px(width + PLOT_GUTTER_WIDTH),
                    stage.px(width / AGGRESSION_MIN_RATIO + PLOT_GUTTER_HEIGHT),
                );
                let chart = plot_bounds(outer, &stage);
                let rail = aggression_rail(chart, &stage);
                assert!((rail.height() - stage.px(6.0)).abs() < 1e-6);
                assert!(chart.height() <= stage.px(32.0) + 1e-6);
                assert!(chart.width() / chart.height() >= AGGRESSION_MIN_RATIO - 1e-6);
                for label in [&plot.labels.x_min, &plot.labels.x, &plot.labels.x_max] {
                    let plan = label.plan();
                    let ink_top = chart.y1
                        + stage.px(X_AXIS_BASELINE)
                        + (plan.ink_top - plan.lines[0].baseline) * stage.unit;
                    assert!(
                        ink_top > rail.y1 + stage.px(2.0),
                        "percentage labels must clear the rail and its reference tick"
                    );
                }
            }
        }
        Ok(())
    }
}
