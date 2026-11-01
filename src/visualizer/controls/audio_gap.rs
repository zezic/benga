//! The captured audio levels that arm and confirm model changes.

use vello::{
    kurbo::{Affine, BezPath, Line, Point, Rect, Stroke},
    peniko::{Color, Fill},
};

use crate::visualizer::{DebugHistory, ModelRotation, DEBUG_HISTORY_SECONDS};

use super::{Canvas, ControlState, Controls, LabelStyle};

const FLOOR_DB: f32 = -48.0;

struct Plot {
    rect: Rect,
    start: f32,
    ceiling: f32,
}

impl Plot {
    fn new(rect: Rect, latest: f32, peak: f32) -> Self {
        Self {
            rect,
            start: latest - DEBUG_HISTORY_SECONDS,
            ceiling: (peak.max(0.0) / 6.0).ceil() * 6.0,
        }
    }

    fn x(&self, at: f32) -> f64 {
        self.rect.x0
            + f64::from(((at - self.start) / DEBUG_HISTORY_SECONDS).clamp(0.0, 1.0))
                * self.rect.width()
    }

    fn y(&self, db: f32) -> f64 {
        self.rect.y1
            - f64::from(((db - FLOOR_DB) / (self.ceiling - FLOOR_DB)).clamp(0.0, 1.0))
                * self.rect.height()
    }
}

/// Counts survive analysis hops between displayed samples, including several
/// confirmed drops in a single frame. The first sample supplies the baseline.
fn confirmed_drops(samples: impl Iterator<Item = (f32, u32)>) -> impl Iterator<Item = (f32, u32)> {
    samples
        .scan(None, |previous, (at, count)| {
            let increase = previous.map_or(0, |previous| {
                count
                    .checked_sub(previous)
                    .expect("confirmed drop counts are monotonic within one history")
            });
            *previous = Some(count);
            Some((at, increase))
        })
        .filter(|(_, count)| *count > 0)
}

fn line(canvas: &mut Canvas<'_>, points: Line, color: Color, width: f64) {
    canvas.scene.stroke(
        &Stroke::new(width),
        Affine::scale(canvas.scale),
        color,
        None,
        &points,
    );
}

fn trace(
    canvas: &mut Canvas<'_>,
    plot: &Plot,
    levels: impl Iterator<Item = (f32, f32)>,
    color: Color,
    stroke: Stroke,
) {
    let mut path = BezPath::new();
    for (index, (at, db)) in levels.enumerate() {
        let point = Point::new(plot.x(at), plot.y(db));
        if index == 0 {
            path.move_to(point);
        } else {
            path.line_to(point);
        }
    }
    canvas
        .scene
        .stroke(&stroke, Affine::scale(canvas.scale), color, None, &path);
}

fn armed_intervals(canvas: &mut Canvas<'_>, plot: &Plot, history: &DebugHistory, opacity: f32) {
    let color = canvas.palette.ink.with_alpha(0.09 * opacity);
    let mut start = None;
    for sample in history.samples() {
        let x = plot.x(sample.at);
        if sample.drop_onset.armed {
            start.get_or_insert(x);
        } else if let Some(start) = start.take() {
            canvas.fill(Rect::new(start, plot.rect.y0, x, plot.rect.y1), color);
        }
    }
    if let Some(start) = start {
        canvas.fill(
            Rect::new(start, plot.rect.y0, plot.rect.x1, plot.rect.y1),
            color,
        );
    }
}

pub(super) fn draw(
    controls: &mut Controls,
    canvas: &mut Canvas<'_>,
    rect: Rect,
    state: &ControlState<'_>,
    opacity: f32,
) {
    let palette = canvas.palette;
    let history = state.history;
    let latest = history.latest();
    let locked = state.config.model_rotation == ModelRotation::Locked;
    canvas.fill(rect, canvas.background.with_alpha(0.96 * opacity));
    canvas.rule(rect, palette.muted.with_alpha(0.45 * opacity));
    controls.label(
        canvas,
        Rect::new(rect.x0 + 8.0, rect.y0 + 4.0, rect.x0 + 70.0, rect.y0 + 18.0),
        "GAP / LEVEL",
        LabelStyle::Caption,
        palette.ink.with_alpha(0.85 * opacity),
    );
    let status = match (latest.map(|sample| sample.drop_onset.armed), locked) {
        (Some(true), false) => "ARMED",
        (Some(true), true) => "ARMED / LOCKED",
        (Some(false), false) => "WAIT",
        (Some(false), true) => "WAIT / LOCKED",
        (None, false) => "NO AUDIO",
        (None, true) => "NO AUDIO / LOCKED",
    };
    controls.label(
        canvas,
        Rect::new(rect.x0 + 78.0, rect.y0 + 4.0, rect.x1 - 8.0, rect.y0 + 18.0),
        status,
        LabelStyle::Caption,
        palette.ink.with_alpha(0.90 * opacity),
    );
    let column_width = (rect.width() - 16.0) / 3.0;
    for (index, (name, value, color)) in [
        (
            "RMS",
            latest.map(|sample| sample.drop_status.rms_db),
            palette.muted,
        ),
        (
            "ENV",
            latest.map(|sample| sample.drop_status.envelope_db),
            palette.ink,
        ),
        (
            "LOSS dB",
            latest.map(|sample| -sample.drop_status.energy_delta_db),
            palette.ink,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let x = rect.x0 + 8.0 + index as f64 * column_width;
        controls.label(
            canvas,
            Rect::new(x, rect.y0 + 20.0, x + column_width, rect.y0 + 32.0),
            name,
            LabelStyle::Caption,
            color.with_alpha(0.85 * opacity),
        );
        let value = value.map_or_else(
            || "--".to_owned(),
            |db| {
                if index == 2 {
                    format!("{db:.2}")
                } else {
                    format!("{db:.1}")
                }
            },
        );
        controls.label(
            canvas,
            Rect::new(x, rect.y0 + 32.0, x + column_width, rect.y0 + 49.0),
            &value,
            LabelStyle::ListValue,
            color.with_alpha(0.95 * opacity),
        );
    }
    let energy_summary = latest.map_or_else(
        || "NEED -- dB  ENERGY --".to_owned(),
        |sample| {
            let status = &sample.drop_status;
            format!(
                "NEED >{:.2} dB  ENERGY {}",
                status.mix_loss_threshold_db,
                if -status.energy_delta_db > status.mix_loss_threshold_db {
                    "PASS"
                } else {
                    "WAIT"
                },
            )
        },
    );
    controls.label(
        canvas,
        Rect::new(rect.x0 + 8.0, rect.y0 + 49.0, rect.x1 - 8.0, rect.y0 + 61.0),
        &energy_summary,
        LabelStyle::Caption,
        palette.muted.with_alpha(0.85 * opacity),
    );
    controls.label(
        canvas,
        Rect::new(rect.x0 + 8.0, rect.y0 + 61.0, rect.x1 - 8.0, rect.y0 + 73.0),
        "··· REF   -- CUT: ENV MUST BE BELOW",
        LabelStyle::Caption,
        palette.muted.with_alpha(0.85 * opacity),
    );

    let peak = history
        .samples()
        .map(|sample| {
            sample
                .drop_status
                .rms_db
                .max(sample.drop_status.envelope_db)
                .max(sample.drop_status.envelope_db - sample.drop_status.energy_delta_db)
        })
        .fold(0.0, f32::max);
    let plot = Plot::new(
        Rect::new(
            rect.x0 + 32.0,
            rect.y0 + 75.0,
            rect.x1 - 8.0,
            rect.y1 - 22.0,
        ),
        latest.map_or(0.0, |sample| sample.at),
        peak,
    );
    armed_intervals(canvas, &plot, history, opacity);
    for db in [FLOOR_DB, -24.0, plot.ceiling] {
        let y = plot.y(db);
        line(
            canvas,
            Line::new((plot.rect.x0, y), (plot.rect.x1, y)),
            palette.muted.with_alpha(0.20 * opacity),
            0.5,
        );
        controls.label(
            canvas,
            Rect::new(rect.x0 + 4.0, y - 7.0, plot.rect.x0 - 4.0, y + 7.0),
            &format!("{db:.0}"),
            LabelStyle::Caption,
            palette.muted.with_alpha(0.8 * opacity),
        );
    }
    trace(
        canvas,
        &plot,
        history
            .samples()
            .map(|sample| (sample.at, sample.drop_status.rms_db)),
        palette.muted.with_alpha(0.85 * opacity),
        Stroke::new(0.8),
    );
    // The energy delta records the reference used for this decision, before
    // learning updates it. Reconstructing it keeps the history exact.
    trace(
        canvas,
        &plot,
        history.samples().map(|sample| {
            (
                sample.at,
                sample.drop_status.envelope_db - sample.drop_status.energy_delta_db,
            )
        }),
        palette.muted.with_alpha(0.70 * opacity),
        Stroke::new(0.9).with_dashes(0.0, [1.0, 2.0]),
    );
    trace(
        canvas,
        &plot,
        history.samples().map(|sample| {
            let status = &sample.drop_status;
            (
                sample.at,
                status.envelope_db - status.energy_delta_db - status.mix_loss_threshold_db,
            )
        }),
        palette.ink.with_alpha(0.75 * opacity),
        Stroke::new(1.0).with_dashes(0.0, [4.0, 2.0]),
    );
    trace(
        canvas,
        &plot,
        history
            .samples()
            .map(|sample| (sample.at, sample.drop_status.envelope_db)),
        palette.ink.with_alpha(0.95 * opacity),
        Stroke::new(1.5),
    );
    for (at, count) in confirmed_drops(
        history
            .samples()
            .map(|sample| (sample.at, sample.drop_onset.count)),
    ) {
        let x = plot.x(at);
        line(
            canvas,
            Line::new((x, plot.rect.y0), (x, plot.rect.y1)),
            palette.ink.with_alpha(0.40 * opacity),
            0.8,
        );
        let mut marker = BezPath::new();
        marker.move_to((x - 3.0, plot.rect.y0));
        marker.line_to((x + 3.0, plot.rect.y0));
        marker.line_to((x, plot.rect.y0 + 5.0));
        marker.close_path();
        canvas.scene.fill(
            Fill::NonZero,
            Affine::scale(canvas.scale),
            palette.ink.with_alpha(0.9 * opacity),
            None,
            &marker,
        );
        if count > 1 {
            controls.label(
                canvas,
                Rect::new(x - 15.0, plot.rect.y0 + 5.0, x + 15.0, plot.rect.y0 + 19.0),
                &format!("x{count}"),
                LabelStyle::Value,
                palette.ink.with_alpha(0.85 * opacity),
            );
        }
    }
    for (text, bounds) in [
        (
            "-4s",
            Rect::new(
                plot.rect.x0,
                plot.rect.y1 + 3.0,
                plot.rect.x0 + 35.0,
                plot.rect.y1 + 17.0,
            ),
        ),
        (
            "NOW",
            Rect::new(
                plot.rect.x1 - 35.0,
                plot.rect.y1 + 3.0,
                plot.rect.x1,
                plot.rect.y1 + 17.0,
            ),
        ),
    ] {
        controls.label(
            canvas,
            bounds,
            text,
            LabelStyle::Caption,
            palette.muted.with_alpha(0.85 * opacity),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_plot_keeps_four_seconds_and_projects_dbfs() {
        let bounds = Rect::new(20.0, 10.0, 420.0, 170.0);
        let plot = Plot::new(bounds, 12.0, -3.0);
        assert_eq!(plot.x(8.0), bounds.x0);
        assert_eq!(plot.x(10.0), bounds.center().x);
        assert_eq!(plot.x(12.0), bounds.x1);
        assert_eq!(plot.x(11.0), 320.0, "a short history does not stretch");
        assert_eq!(plot.y(-48.0), bounds.y1);
        assert_eq!(plot.y(0.0), bounds.y0);
        assert_eq!(plot.y(-24.0), bounds.center().y);
        let boosted = Plot::new(bounds, 12.0, 7.0);
        assert_eq!(boosted.ceiling, 12.0);
        assert_eq!(boosted.y(12.0), bounds.y0);
        assert!(boosted.y(7.0) > bounds.y0);
    }

    #[test]
    fn confirmed_markers_keep_count_jumps_without_repeating_the_first_sample() {
        let samples = [(8.0, 4), (9.0, 4), (10.0, 5), (11.0, 5), (12.0, 7)];
        assert_eq!(
            confirmed_drops(samples.into_iter()).collect::<Vec<_>>(),
            [(10.0, 1), (12.0, 2)]
        );
    }
}
