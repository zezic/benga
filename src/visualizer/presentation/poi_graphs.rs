//! Additional POI callouts: musical plots attached to stable mesh vertices.

use vello::{
    kurbo::{Point, Rect, Vec2},
    Scene,
};

use crate::text::Fonts;

use super::super::{detection::Spring, showcase::ShowcaseMode, Rng, TriggerCounts, Visualizer};
use super::{
    graphs::{Events, Lifetime},
    plots::{PlotKind, ScenePlot, AGGRESSION_MIN_RATIO, PLOT_GUTTER_HEIGHT, PLOT_GUTTER_WIDTH},
    poi::{card_anchor, draw_anchor, draw_leader},
    poi_motion::{advance_springs, Coast, GRAPH_COAST_SECONDS},
    ScenePalette, Stage,
};

const MAX_GRAPHS: usize = 2;

#[derive(Default)]
pub(super) struct PoiGraphs {
    graphs: Vec<AnchoredGraph>,
    context: Option<Context>,
    desired_count: usize,
    cooldown: f32,
    time: f32,
    last_triggers: Option<TriggerCounts>,
    last_audio_time: Option<f64>,
    epochs: Option<[u64; 3]>,
}

struct Context {
    model: Option<String>,
    target: String,
}

struct AnchoredGraph {
    plot: ScenePlot,
    kind: PlotKind,
    vertex: u16,
    anchor: Point,
    offset: Vec2,
    spring_x: Spring,
    spring_y: Spring,
    coast: Option<Coast>,
    focus_alpha: f32,
    width_units: f64,
    height_units: f64,
    rect: Rect,
    phase: f64,
    life: Lifetime,
    seed: u64,
    data_frozen: bool,
}

impl AnchoredGraph {
    fn retire(&mut self, freeze_data: bool) {
        if self.coast.is_none() {
            self.coast = Some(Coast::from_springs(
                &self.spring_x,
                &self.spring_y,
                GRAPH_COAST_SECONDS,
            ));
            self.life.exit = GRAPH_COAST_SECONDS;
        }
        self.life.retire();
        self.data_frozen |= freeze_data;
    }

    fn follow(&mut self, dt: f32, time: f32, visualizer: &Visualizer, stage: &Stage) {
        self.anchor = card_anchor(visualizer, stage, Some(self.vertex));
        let width = stage.px(self.width_units);
        let height = stage.px(self.height_units);
        let drift = Vec2::new(
            (f64::from(time) * 0.31 + self.phase).sin() * stage.px(9.0),
            (f64::from(time) * 0.24 + self.phase * 1.7).cos() * stage.px(7.0),
        );
        let target = clamp_center(
            self.anchor + self.offset * stage.unit + drift,
            width,
            height,
            stage,
        );
        self.spring_x.set_target(target.x as f32);
        self.spring_y.set_target(target.y as f32);
        advance_springs(&mut self.spring_x, &mut self.spring_y, dt);
        self.rect = centered(self.center(), width, height);
    }

    fn center(&self) -> Point {
        Point::new(
            f64::from(self.spring_x.value()),
            f64::from(self.spring_y.value()),
        )
    }
}

impl PoiGraphs {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn update(
        &mut self,
        dt: f32,
        visualizer: &Visualizer,
        fonts: &mut Fonts,
        stage: &Stage,
        rng: &mut Rng,
    ) {
        let dt = dt.max(0.0);
        self.time += dt;
        self.cooldown = (self.cooldown - dt).max(0.0);
        let counts = visualizer.trigger_counts();
        let events = Events::between(self.last_triggers, counts);
        let audio_time = visualizer.percussion_audio_time();
        let epochs =
            std::array::from_fn(|channel| visualizer.percussion_channel_status(channel).epoch);
        let reset = self.epochs.is_some_and(|last| last != epochs)
            || self.last_audio_time.is_some_and(|last| audio_time < last)
            || self.last_triggers.is_some_and(|last| {
                counts.kick < last.kick
                    || counts.snare < last.snare
                    || counts.hat < last.hat
                    || counts.drop < last.drop
            });
        self.last_triggers = Some(counts);
        self.last_audio_time = Some(audio_time);
        self.epochs = Some(epochs);
        let target = (visualizer.showcase_mode() == ShowcaseMode::Poi)
            .then(|| visualizer.showcase_target_id())
            .flatten();
        let changed = match (&self.context, target) {
            (Some(context), Some(target)) => {
                context.model.as_deref() != visualizer.model_id() || context.target != target
            }
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.context = target.map(|target| Context {
                model: visualizer.model_id().map(str::to_owned),
                target: target.to_owned(),
            });
            self.desired_count = if target.is_some() {
                1 + rng.range(MAX_GRAPHS)
            } else {
                0
            };
            self.cooldown = 0.0;
        }
        if changed || reset {
            for graph in &mut self.graphs {
                graph.retire(true);
            }
        }
        for graph in &mut self.graphs {
            if !graph.life.exiting() {
                graph.focus_alpha = focus_alpha(visualizer.showcase_focus_blend());
                graph.follow(dt, self.time, visualizer, stage);
            } else {
                // Preserve the old vertex anchor, but let its instrument keep
                // its release velocity as friction gradually slows it down.
                if let Some(coast) = &mut graph.coast {
                    coast.advance(dt, graph.rect.width(), graph.rect.height(), stage);
                    graph.rect =
                        centered(coast.position(), graph.rect.width(), graph.rect.height());
                }
            }
            if !graph.data_frozen {
                graph.plot.update(visualizer);
            }
            graph.life.advance(dt, events, false, rng);
            if graph.life.exiting() && graph.coast.is_none() {
                graph.retire(false);
            }
        }
        let before = self.graphs.len();
        self.graphs.retain(|graph| !graph.life.finished());
        if self.graphs.len() < before {
            self.cooldown = 0.25 + rng.unit() * 0.3;
        }
        if target.is_none() || self.cooldown > 0.0 {
            return;
        }
        if events.any() && self.desired_count < MAX_GRAPHS && rng.unit() < 0.25 {
            self.desired_count += 1;
        }
        self.fill(visualizer, fonts, stage, rng, events);
    }

    fn fill(
        &mut self,
        visualizer: &Visualizer,
        fonts: &mut Fonts,
        stage: &Stage,
        rng: &mut Rng,
        events: Events,
    ) {
        let Some(context) = &self.context else {
            return;
        };
        if self.graphs.len() >= self.desired_count.min(MAX_GRAPHS) {
            return;
        }
        let vertices = visualizer.poi_vertices(&context.target);
        if vertices.is_empty() {
            self.cooldown = 0.25;
            return;
        }
        while self.graphs.len() < self.desired_count.min(MAX_GRAPHS) {
            let vertex = vertices[rng.range(vertices.len())];
            let anchor = card_anchor(visualizer, stage, Some(vertex));
            let mut kind = choose_kind(visualizer, events, rng);
            for _ in 0..3 {
                if !self.graphs.iter().any(|graph| graph.kind == kind) {
                    break;
                }
                kind = choose_kind(visualizer, events, rng);
            }
            let ratio = if matches!(kind, PlotKind::Heatmap(_)) {
                [1.0, 2.0, 0.5][rng.range(3)]
            } else {
                1.6 + f64::from(rng.unit()) * 1.0
            };
            let mut placement = None;
            for width_units in [240.0 + f64::from(rng.unit()) * 80.0, 220.0, 185.0] {
                let ratio = if kind == PlotKind::Aggression {
                    AGGRESSION_MIN_RATIO.max(width_units / 32.0)
                } else {
                    ratio
                };
                let width = stage.px(width_units + PLOT_GUTTER_WIDTH);
                let height = stage.px(width_units / ratio + PLOT_GUTTER_HEIGHT);
                if let Some(rect) = place(anchor, width, height, stage, rng) {
                    placement = Some((
                        rect,
                        width_units + PLOT_GUTTER_WIDTH,
                        width_units / ratio + PLOT_GUTTER_HEIGHT,
                    ));
                    break;
                }
            }
            let Some((rect, width_units, height_units)) = placement else {
                self.cooldown = 0.2 + rng.unit() * 0.2;
                break;
            };
            let mut spring_x = Spring::default();
            let mut spring_y = Spring::default();
            spring_x.reset(rect.center().x as f32);
            spring_y.reset(rect.center().y as f32);
            self.graphs.push(AnchoredGraph {
                plot: ScenePlot::new(kind, visualizer, fonts),
                kind,
                vertex,
                anchor,
                offset: (rect.center() - anchor) / stage.unit,
                spring_x,
                spring_y,
                coast: None,
                focus_alpha: focus_alpha(visualizer.showcase_focus_blend()),
                width_units,
                height_units,
                rect,
                phase: f64::from(rng.unit()) * std::f64::consts::TAU,
                life: Lifetime::new(1, rng),
                seed: rng.next(),
                data_frozen: false,
            });
        }
    }

    pub(super) fn draw(
        &self,
        scene: &mut Scene,
        stage: &Stage,
        palette: ScenePalette,
        focus_blend: f32,
    ) {
        let blend = focus_alpha(focus_blend);
        for graph in &self.graphs {
            let (reveal, alpha) = graph.life.animation(0);
            // Fade the complete callout without dissolving its cells or traces.
            let alpha = reveal
                * alpha
                * if graph.life.exiting() {
                    graph.focus_alpha
                } else {
                    blend
                };
            graph
                .plot
                .draw(scene, graph.rect, stage, palette, 1.0, alpha, graph.seed);
            draw_leader(scene, stage, palette, graph.anchor, graph.rect, alpha);
            draw_anchor(scene, stage, palette, graph.anchor, alpha);
        }
    }

    #[cfg(test)]
    #[cfg(test)]
    pub(super) fn occluders(&self) -> Vec<Rect> {
        self.graphs.iter().map(|graph| graph.rect).collect()
    }

    #[cfg(test)]
    pub(super) fn count(&self) -> usize {
        self.graphs.len()
    }

    #[cfg(test)]
    pub(super) fn preview_spawn(
        &mut self,
        visualizer: &Visualizer,
        fonts: &mut Fonts,
        stage: &Stage,
        rng: &mut Rng,
    ) -> usize {
        self.clear();
        self.update(0.0, visualizer, fonts, stage, rng);
        self.desired_count = MAX_GRAPHS;
        self.fill(visualizer, fonts, stage, rng, Events::default());
        for graph in &mut self.graphs {
            graph.life.age = graph.life.entrance;
        }
        self.count()
    }
}

fn choose_kind(visualizer: &Visualizer, events: Events, rng: &mut Rng) -> PlotKind {
    let channel = if events.snare {
        1
    } else if events.hat && !events.kick {
        2
    } else if events.kick {
        0
    } else {
        rng.range(3)
    };
    match rng.range(100) {
        0..=54 => PlotKind::Heatmap(channel),
        55..=69 => {
            let epoch = visualizer.percussion_channel_status(channel).epoch;
            if visualizer.percussion_decisions().iter().any(|event| {
                event.channel == channel && event.epoch == epoch && event.envelope_len > 1
            }) {
                PlotKind::Attack(channel)
            } else {
                PlotKind::Heatmap(channel)
            }
        }
        70..=77 => PlotKind::SpectralShape,
        78..=84 => PlotKind::Energy,
        85..=90 => PlotKind::Bass,
        91..=95 => PlotKind::Onsets,
        _ => PlotKind::Aggression,
    }
}

fn focus_alpha(blend: f32) -> f32 {
    let blend = blend.clamp(0.0, 1.0);
    blend * blend * (3.0 - 2.0 * blend)
}

fn centered(center: Point, width: f64, height: f64) -> Rect {
    Rect::new(
        center.x - width * 0.5,
        center.y - height * 0.5,
        center.x + width * 0.5,
        center.y + height * 0.5,
    )
}

fn clamp_center(center: Point, width: f64, height: f64, stage: &Stage) -> Point {
    let min_x = stage.margin + width * 0.5;
    let max_x = stage.width - stage.margin - width * 0.5;
    let min_y = stage.margin + height * 0.5;
    let max_y = stage.height - stage.margin - height * 0.5;
    Point::new(
        if min_x <= max_x {
            center.x.clamp(min_x, max_x)
        } else {
            stage.width * 0.5
        },
        if min_y <= max_y {
            center.y.clamp(min_y, max_y)
        } else {
            stage.height * 0.5
        },
    )
}

fn fits_viewport(rect: Rect, stage: &Stage) -> bool {
    rect.x0 >= stage.margin
        && rect.y0 >= stage.margin
        && rect.x1 <= stage.width - stage.margin
        && rect.y1 <= stage.height - stage.margin
}

fn place(anchor: Point, width: f64, height: f64, stage: &Stage, rng: &mut Rng) -> Option<Rect> {
    let air = stage.px(118.0);
    let dx = air + width * 0.5;
    let dy = air + height * 0.5;
    let mut candidates = Vec::new();
    for (x, y) in [
        (1.0, 0.0),
        (-1.0, 0.0),
        (0.0, 1.0),
        (0.0, -1.0),
        (1.0, 1.0),
        (-1.0, 1.0),
        (1.0, -1.0),
        (-1.0, -1.0),
    ] {
        for spread in [1.0, 1.4, 1.8] {
            let center = clamp_center(
                anchor + Vec2::new(dx * x * spread, dy * y * spread),
                width,
                height,
                stage,
            );
            let rect = centered(center, width, height);
            if fits_viewport(rect, stage) {
                candidates.push(rect);
            }
        }
    }
    if candidates.is_empty() {
        None
    } else {
        Some(candidates[rng.range(candidates.len())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        audio::timeline::{AudioInterval, AudioTime},
        config::ShutterFrames,
        theme::ThemeId,
        visualizer::{presentation::plots::plot_bounds, ModelMotion, ModelRotation, Palette},
    };
    use vello::kurbo::Size;

    fn frame(visualizer: &mut Visualizer, stage: &Stage, index: usize) {
        let start = AudioTime::ZERO.shifted(index as f64 / 60.0);
        visualizer.update(
            AudioInterval::new(start, start.shifted(1.0 / 60.0)),
            ShutterFrames::try_from(1.0).unwrap(),
            1.0 / 60.0,
            &Palette::from_theme(ThemeId::default()),
            (stage.width as f32, stage.height as f32),
            1.0,
            1.0,
        );
    }

    fn focused_visualizer(stage: &Stage) -> Visualizer {
        let mut visualizer = Visualizer::new();
        visualizer.load_models();
        for _ in 0..64 {
            if visualizer.model_id() == Some("rif_ak47") {
                break;
            }
            visualizer.next_model();
        }
        assert_eq!(visualizer.model_id(), Some("rif_ak47"));
        visualizer.set_model_motion(ModelMotion::Still);
        visualizer.set_model_rotation(ModelRotation::Locked);
        frame(&mut visualizer, stage, 0);
        assert!(visualizer.showcase.enter_focus(&mut Rng::new(7), 1.1));
        frame(&mut visualizer, stage, 1);
        assert!(!visualizer
            .poi_vertices(visualizer.showcase_target_id().unwrap())
            .is_empty());
        visualizer
    }

    #[test]
    fn live_callouts_follow_the_same_projected_vertex_as_the_focus_camera_moves(
    ) -> anyhow::Result<()> {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut visualizer = focused_visualizer(&stage);
        let mut fonts = Fonts::new()?;
        let mut rng = Rng::new(71);
        let mut graphs = PoiGraphs::default();
        assert_eq!(
            graphs.preview_spawn(&visualizer, &mut fonts, &stage, &mut rng),
            2
        );
        let vertices: Vec<_> = graphs.graphs.iter().map(|graph| graph.vertex).collect();
        let anchor = graphs.graphs[0].anchor;
        let center = graphs.graphs[0].rect.center();
        for index in 2..90 {
            frame(&mut visualizer, &stage, index);
            graphs.update(1.0 / 60.0, &visualizer, &mut fonts, &stage, &mut rng);
            assert_eq!(graphs.count(), 2);
            for (index, graph) in graphs.graphs.iter().enumerate() {
                assert_eq!(graph.vertex, vertices[index]);
                assert_eq!(
                    graph.anchor,
                    card_anchor(&visualizer, &stage, Some(graph.vertex))
                );
                assert!(!graph.life.exiting());
            }
        }
        assert!(
            graphs.graphs[0].anchor.distance(anchor) > 0.5,
            "the production camera must move the tracked vertex"
        );
        assert!(
            graphs.graphs[0].rect.center().distance(center) > 0.5,
            "the plot must follow, rather than only moving its leader"
        );
        Ok(())
    }

    #[test]
    fn focus_exit_keeps_the_anchor_while_the_instrument_coasts_and_fades() -> anyhow::Result<()> {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut visualizer = focused_visualizer(&stage);
        for index in 2..100 {
            frame(&mut visualizer, &stage, index);
        }
        let mut fonts = Fonts::new()?;
        let mut rng = Rng::new(72);
        let mut graphs = PoiGraphs::default();
        graphs.preview_spawn(&visualizer, &mut fonts, &stage, &mut rng);
        graphs.graphs[0].spring_x.impulse(120.0);
        let bounds = graphs.occluders();
        let anchors: Vec<_> = graphs.graphs.iter().map(|graph| graph.anchor).collect();
        visualizer.showcase.exit_focus();
        visualizer.next_model();
        frame(&mut visualizer, &stage, 100);
        graphs.update(0.05, &visualizer, &mut fonts, &stage, &mut rng);
        assert!(graphs.graphs[0].rect.x0 > bounds[0].x0);
        let release = graphs.graphs[0].coast.unwrap();
        assert!(release.velocity().x > 0.0 && release.velocity().x < 120.0);
        for (graph, anchor) in graphs.graphs.iter().zip(anchors) {
            assert!(graph.life.exiting());
            assert!(graph.data_frozen);
            assert_eq!(graph.anchor, anchor);
        }
        let colors = crate::theme::Theme::new(ThemeId::default()).colors;
        let palette =
            ScenePalette::choose(&Palette::from_theme(ThemeId::default()), colors, &mut rng);
        graphs.update(0.7, &visualizer, &mut fonts, &stage, &mut rng);
        assert!(
            graphs.count() > 0,
            "the release tail remains visible after focus has gone"
        );
        let mut scene = Scene::new();
        graphs.draw(&mut scene, &stage, palette, 0.0);
        assert!(!scene.encoding().draw_data.is_empty());
        graphs.update(0.5, &visualizer, &mut fonts, &stage, &mut rng);
        assert!(graphs.occluders().is_empty());
        assert_eq!(graphs.count(), 0);
        Ok(())
    }

    #[test]
    fn gain_changes_retire_the_old_feature_epoch_before_updating_plots() -> anyhow::Result<()> {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut visualizer = focused_visualizer(&stage);
        visualizer.consume_buf(&[[0.0; 2]; 2400], 1.0);
        let mut fonts = Fonts::new()?;
        let mut rng = Rng::new(74);
        let mut graphs = PoiGraphs::default();
        graphs.preview_spawn(&visualizer, &mut fonts, &stage, &mut rng);
        let epochs = graphs.epochs.unwrap();
        visualizer.consume_buf(&[[0.0; 2]; 2400], 1.0);
        graphs.update(0.05, &visualizer, &mut fonts, &stage, &mut rng);
        assert!(graphs
            .graphs
            .iter()
            .all(|graph| !graph.data_frozen && !graph.life.exiting()));
        let bounds = graphs.occluders();
        visualizer.consume_buf(&[[0.0; 2]; 2400], 0.5);
        graphs.update(0.05, &visualizer, &mut fonts, &stage, &mut rng);
        assert_ne!(graphs.epochs.unwrap(), epochs);
        for (rect, previous) in graphs.occluders().iter().zip(bounds) {
            assert!((rect.width() - previous.width()).abs() < 1e-8);
            assert!((rect.height() - previous.height()).abs() < 1e-8);
        }
        assert!(graphs
            .graphs
            .iter()
            .all(|graph| graph.data_frozen && graph.life.exiting()));
        Ok(())
    }

    #[test]
    fn initial_placement_fits_the_viewport_and_resizing_preserves_chart_ratios(
    ) -> anyhow::Result<()> {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut visualizer = focused_visualizer(&stage);
        let mut fonts = Fonts::new()?;
        let mut rng = Rng::new(73);
        let mut graphs = PoiGraphs::default();
        assert_eq!(
            graphs.preview_spawn(&visualizer, &mut fonts, &stage, &mut rng),
            2
        );
        assert!(graphs
            .graphs
            .iter()
            .all(|graph| fits_viewport(graph.rect, &stage)));
        let ratios: Vec<_> = graphs
            .graphs
            .iter()
            .map(|graph| {
                let chart = plot_bounds(graph.rect, &stage);
                chart.width() / chart.height()
            })
            .collect();
        let smaller = Stage::new(Size::new(1280.0, 720.0));
        for index in 2..182 {
            frame(&mut visualizer, &smaller, index);
            graphs.update(1.0 / 60.0, &visualizer, &mut fonts, &smaller, &mut rng);
            for (graph, ratio) in graphs.graphs.iter().zip(&ratios) {
                let chart = plot_bounds(graph.rect, &smaller);
                assert!((chart.width() / chart.height() - ratio).abs() < 1e-8);
                assert!(!graph.life.exiting());
            }
        }
        for graph in &graphs.graphs {
            assert!(graph.rect.x0 >= smaller.margin - 0.5 && graph.rect.y0 >= smaller.margin - 0.5);
            assert!(graph.rect.x1 <= smaller.width - smaller.margin + 0.5);
            assert!(graph.rect.y1 <= smaller.height - smaller.margin + 0.5);
        }
        graphs.clear();
        assert_eq!(graphs.count(), 0);
        assert!(graphs.occluders().is_empty());
        Ok(())
    }

    #[test]
    fn attached_graphs_keep_following_through_other_poi_cards_and_fully_occupied_space(
    ) -> anyhow::Result<()> {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut visualizer = focused_visualizer(&stage);
        let mut fonts = Fonts::new()?;
        let mut rng = Rng::new(75);
        let mut cards = super::super::poi::PoiScene::default();
        cards.update(
            1.0 / 60.0,
            visualizer.model_id(),
            visualizer.showcase_target_id(),
            visualizer.weapon(),
            visualizer.comedy_spec(),
            &visualizer,
            &mut fonts,
            &stage,
            &mut rng,
            false,
        );
        let mut graphs = PoiGraphs::default();
        graphs.preview_spawn(&visualizer, &mut fonts, &stage, &mut rng);
        let card = cards.card_occluders(&stage)[0];
        let graph = &mut graphs.graphs[0];
        graph.spring_x.reset(card.center().x as f32);
        graph.spring_y.reset(card.center().y as f32);
        graph.offset = (card.center() - graph.anchor) / stage.unit;
        graph.rect = centered(card.center(), graph.rect.width(), graph.rect.height());
        let start = graph.rect.center();
        let vertex = graph.vertex;
        let blocker = Rect::new(0.0, 0.0, stage.width, stage.height);
        let mut moved = 0.0;
        for index in 2..100 {
            frame(&mut visualizer, &stage, index);
            graphs.update(1.0 / 60.0, &visualizer, &mut fonts, &stage, &mut rng);
            let graph = &graphs.graphs[0];
            assert_eq!(graph.vertex, vertex);
            assert_eq!(graph.anchor, card_anchor(&visualizer, &stage, Some(vertex)));
            assert!(!graph.life.exiting());
            assert!(blocker.intersect(graph.rect).area() > 0.0);
            moved += graph.rect.center().distance(start);
        }
        assert!(
            moved > 1.0,
            "an overlapping card or occupied viewport must not pin the graph in place"
        );
        assert_eq!(graphs.count(), 2);
        Ok(())
    }
}
