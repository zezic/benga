//! Viewport-relative typography, musical graphs, and optional audio analyzers.

mod analysis;
mod color;
mod decorations;
mod graphs;
mod normal;
mod plots;
mod poi;
mod poi_graphs;
mod poi_motion;
mod selector;
mod stage;
mod text;

pub(super) use analysis::AnalysisInteraction;
pub use color::ScenePalette;
#[cfg(test)]
pub(super) use graphs::FormationPreview;
pub use normal::NormalScene;
pub use poi::PoiScene;
pub use selector::SelectorOverlay;
pub use stage::Stage;

use vello::{
    kurbo::{Point, Rect},
    Scene,
};

use crate::{
    render::{PlateMaskImage, RasterAsset},
    text::Fonts,
    theme::{Theme, ThemeColors},
    visualizer::Palette,
};

use super::{showcase::ShowcaseMode, Parameter, Rng, TriggerCounts, Visualizer};

pub struct Presentation {
    time: f32,
    stage: Option<Stage>,
    rng: Rng,
    normal: NormalScene,
    selector: SelectorOverlay,
    poi: PoiScene,
    colors: ScenePalette,
    last_triggers: Option<TriggerCounts>,
    poi_mode: bool,
    focus_blend: f32,
    micro: Vec<MicroFx>,
    analysis: analysis::AnalysisOverlay,
    analyzers_visible: bool,
    graphs: graphs::GraphCompositions,
    poi_graphs: poi_graphs::PoiGraphs,
    exclusion: Option<Rect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MicroKind {
    Tick,
    Cross,
    Bracket,
    Scan,
}

struct MicroFx {
    center: Point,
    age: f32,
    life: f32,
    kind: MicroKind,
}

impl Presentation {
    pub fn new(palette: &Palette, colors: ThemeColors) -> Self {
        let mut rng = Rng::new(0x7072_6573_656e_7421);
        let colors = ScenePalette::choose(palette, colors, &mut rng);
        Self {
            time: 0.0,
            stage: None,
            rng,
            normal: NormalScene::default(),
            selector: SelectorOverlay::default(),
            poi: PoiScene::default(),
            colors,
            last_triggers: None,
            poi_mode: false,
            focus_blend: 0.0,
            micro: Vec::new(),
            analysis: analysis::AnalysisOverlay::default(),
            analyzers_visible: false,
            graphs: graphs::GraphCompositions::default(),
            poi_graphs: poi_graphs::PoiGraphs::default(),
            exclusion: None,
        }
    }

    pub fn choose_colors(&mut self, palette: &Palette, colors: ThemeColors) {
        self.colors = ScenePalette::choose(palette, colors, &mut self.rng);
    }

    pub fn analyzers_visible(&self) -> bool {
        self.analyzers_visible
    }

    pub fn set_analyzers_visible(&mut self, visible: bool) {
        if self.analyzers_visible != visible {
            self.analyzers_visible = visible;
            self.analysis.cancel_interaction();
            self.graphs.clear();
            self.poi_graphs.clear();
        }
    }

    pub fn stage(&self) -> Option<Stage> {
        self.stage
    }

    pub(super) fn analysis_pointer_down(&mut self, point: Point) -> Option<AnalysisInteraction> {
        self.analyzers_visible
            .then(|| self.analysis.pointer_down(point))
            .flatten()
    }

    pub(super) fn analysis_pointer_move(
        &mut self,
        point: Option<Point>,
    ) -> Option<(Parameter, f32)> {
        self.analyzers_visible
            .then(|| self.analysis.pointer_move(point))
            .flatten()
    }

    pub(super) fn analysis_pointer_up(&mut self) {
        self.analysis.pointer_up();
    }

    pub(super) fn analysis_contains(&self, point: Point) -> bool {
        self.analyzers_visible && self.analysis.contains(point)
    }

    pub(super) fn analysis_dragging(&self) -> bool {
        self.analyzers_visible && self.analysis.dragging()
    }

    /// Reserves screen space for an interactive overlay above the presentation.
    pub fn set_exclusion_bounds(&mut self, bounds: Option<Rect>) {
        self.exclusion = bounds;
        self.selector.set_exclusion_bounds(bounds);
    }

    /// Advance the presentation scene. Layout is rebuilt only when the stage
    /// size or the model changes.
    pub fn update(
        &mut self,
        dt: f32,
        viewport: (f32, f32),
        visualizer: &Visualizer,
        fonts: &mut Fonts,
        palette: &Palette,
        colors: ThemeColors,
    ) {
        self.time += dt;
        let stage = Stage::new(vello::kurbo::Size::new(
            f64::from(viewport.0),
            f64::from(viewport.1),
        ));
        if !self
            .stage
            .is_some_and(|current| same_stage(&current, &stage))
        {
            self.stage = Some(stage);
        }
        let model_id = visualizer.model_id();
        let mode = visualizer.showcase_mode();
        self.poi_mode = mode == ShowcaseMode::Poi;
        self.focus_blend = visualizer.showcase_focus_blend();
        let target = if mode == ShowcaseMode::Poi {
            visualizer.showcase_target_id()
        } else {
            None
        };
        self.colors.refresh(palette, colors);
        let counts = visualizer.trigger_counts();
        let previous = self.last_triggers;
        let hat_trigger = previous.is_some_and(|last| counts.hat > last.hat);
        let any_micro_trigger = previous.is_some_and(|last| {
            counts.kick > last.kick || counts.snare > last.snare || counts.drop > last.drop
        });
        self.last_triggers = Some(counts);
        let display_name = visualizer.weapon().map_or_else(
            || visualizer.scene_name(),
            |weapon| weapon.display_name.as_str(),
        );
        self.normal.update(
            dt,
            model_id,
            display_name,
            visualizer.weapon(),
            visualizer,
            fonts,
            &stage,
            &mut self.rng,
        );
        let selector_active = mode == ShowcaseMode::Normal && visualizer.showcase_selector_active();
        self.selector.update(
            dt,
            selector_active,
            visualizer.showcase_selected_part(),
            visualizer,
            fonts,
            &stage,
            &mut self.rng,
        );
        self.poi.update(
            dt,
            model_id,
            target,
            visualizer.weapon(),
            visualizer.comedy_spec(),
            visualizer,
            fonts,
            &stage,
            &mut self.rng,
            hat_trigger,
        );
        let mut occluders = self.selector.occluders(&stage);
        occluders.extend(self.poi.occluders(&stage));
        occluders.extend(self.exclusion);
        if self.analyzers_visible {
            self.analysis
                .update(dt, visualizer.debug_history(), fonts, &stage, visualizer);
        } else {
            self.poi_graphs
                .update(dt, visualizer, fonts, &stage, &mut self.rng);
            let mut graph_occluders = self.selector.occluders(&stage);
            graph_occluders.extend(self.poi.card_occluders(&stage));
            graph_occluders.extend(self.exclusion);
            self.graphs.update(
                dt,
                visualizer,
                fonts,
                &stage,
                &mut self.rng,
                &graph_occluders,
            );
            occluders.extend(self.graphs.occluders());
        }
        self.normal
            .set_fact_hidden(self.poi.shows_fact(), &mut self.rng);
        self.normal
            .suppress_colliding(&stage, &occluders, &mut self.rng);
        if any_micro_trigger && self.rng.unit() < 0.14 {
            self.spawn_micro(&stage);
        }
        for fx in &mut self.micro {
            fx.age += dt;
        }
        self.micro.retain(|fx| fx.age < fx.life);
    }

    fn spawn_micro(&mut self, stage: &Stage) {
        let rect = if self.poi_mode {
            self.poi.random_card_rect(&mut self.rng)
        } else {
            None
        }
        .or_else(|| self.normal.random_text_rect(stage, &mut self.rng));
        let Some(rect) = rect else {
            return;
        };
        let center = Point::new(
            rect.x0 + rect.width() * f64::from(self.rng.unit()),
            rect.y0 + rect.height() * f64::from(self.rng.unit()),
        );
        let kind = match self.rng.range(4) {
            0 => MicroKind::Tick,
            1 => MicroKind::Cross,
            2 => MicroKind::Bracket,
            _ => MicroKind::Scan,
        };
        self.micro.push(MicroFx {
            center,
            age: 0.0,
            life: 0.24 + self.rng.unit() * 0.22,
            kind,
        });
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        raster_assets: &mut Vec<RasterAsset>,
        stage: &Stage,
        palette: ScenePalette,
        plate_mask: Option<&PlateMaskImage>,
    ) {
        if self.analyzers_visible {
            self.analysis.draw(scene, stage, palette);
        } else {
            self.graphs.draw(scene, stage, palette);
        }
        let normal_alpha = if self.poi_mode { 0.1 } else { 1.0 };
        self.normal.draw(
            scene,
            raster_assets,
            stage,
            palette,
            normal_alpha,
            self.time,
        );
        self.selector.draw(scene, stage, palette);
        self.poi
            .draw(scene, stage, palette, self.focus_blend, plate_mask);
        if !self.analyzers_visible {
            self.poi_graphs
                .draw(scene, stage, palette, self.focus_blend);
        }
        for fx in &self.micro {
            draw_micro(scene, stage, palette, fx);
        }
    }

    /// Interactive banks are composited through the normal-alpha top overlay.
    pub(super) fn draw_analysis_controls(
        &self,
        scene: &mut Scene,
        stage: &Stage,
        palette: ScenePalette,
        theme: &Theme,
    ) {
        if self.analyzers_visible {
            self.analysis.draw_controls(scene, stage, palette, theme);
        }
    }

    pub fn colors(&self) -> ScenePalette {
        self.colors
    }

    pub fn plate_instances(&self) -> &[crate::render::PlateInstance] {
        self.poi.plate_instances()
    }

    #[cfg(test)]
    pub fn debug_hit_marker(&mut self) {
        self.poi.debug_spawn_hit_marker(&mut self.rng);
    }

    #[cfg(test)]
    pub(super) fn preview_graphs(
        &mut self,
        visualizer: &Visualizer,
        fonts: &mut Fonts,
        stage: &Stage,
        seed: u64,
    ) -> usize {
        let mut occluders = self.selector.occluders(stage);
        occluders.extend(self.poi.card_occluders(stage));
        occluders.extend(self.exclusion);
        self.graphs
            .preview_spawn(visualizer, fonts, stage, &mut Rng::new(seed), &occluders);
        self.graphs.occluders().len()
    }

    #[cfg(test)]
    pub(super) fn preview_formation(
        &mut self,
        visualizer: &Visualizer,
        fonts: &mut Fonts,
        stage: &Stage,
        seed: u64,
        preview: FormationPreview,
    ) -> usize {
        let mut occluders = self.selector.occluders(stage);
        occluders.extend(self.poi.card_occluders(stage));
        occluders.extend(self.exclusion);
        self.graphs.preview_formation(
            visualizer,
            fonts,
            stage,
            &mut Rng::new(seed),
            &occluders,
            preview,
        )
    }

    #[cfg(test)]
    pub(super) fn poi_graph_count(&self) -> usize {
        self.poi_graphs.count()
    }

    #[cfg(test)]
    pub(super) fn preview_poi_graphs(
        &mut self,
        visualizer: &Visualizer,
        fonts: &mut Fonts,
        stage: &Stage,
        seed: u64,
    ) -> usize {
        self.poi_graphs
            .preview_spawn(visualizer, fonts, stage, &mut Rng::new(seed))
    }

    #[cfg(test)]
    pub fn debug_analysis_count(&self) -> usize {
        if self.analyzers_visible {
            self.analysis.panel_count()
        } else {
            0
        }
    }

    #[cfg(test)]
    pub fn analysis_slider_center(&self, parameter: Parameter) -> Option<Point> {
        self.analysis.slider_center(parameter)
    }

    #[cfg(test)]
    pub fn analysis_graph_bounds(&self) -> Option<Rect> {
        self.analysis.onset_graph_bounds()
    }

    #[cfg(test)]
    pub fn analysis_selected_detector(&self) -> super::Detector {
        self.analysis.selected_detector()
    }

    #[cfg(test)]
    pub fn analysis_bin_reset_center(&self) -> Option<Point> {
        self.analysis.bin_reset_center()
    }

    #[cfg(test)]
    pub fn analysis_bin_next_center(&self) -> Option<Point> {
        self.analysis.bin_next_center()
    }

    #[cfg(test)]
    pub fn analysis_first_bin_parameter(&self) -> Option<Parameter> {
        self.analysis.first_bin_parameter()
    }

    #[cfg(test)]
    pub fn analysis_marker_count_center(&self, more: bool) -> Option<Point> {
        self.analysis.marker_count_center(more)
    }

    #[cfg(test)]
    pub fn analysis_previous_event_center(&self) -> Option<Point> {
        self.analysis.previous_event_center()
    }
}

fn draw_micro(scene: &mut Scene, stage: &Stage, palette: ScenePalette, fx: &MicroFx) {
    let progress = (fx.age / fx.life).clamp(0.0, 1.0);
    let alpha = (1.0 - progress) * 0.7;
    let scale = stage.px(10.0) * (0.7 + 0.5 * f64::from(1.0 - progress));
    let width = (stage.unit * 1.2).max(0.8);
    let color = palette.accent.with_alpha(alpha);
    let center = fx.center;
    match fx.kind {
        MicroKind::Tick => {
            scene.stroke(
                &vello::kurbo::Stroke::new(width),
                vello::kurbo::Affine::IDENTITY,
                color,
                None,
                &vello::kurbo::Line::new(
                    Point::new(center.x - scale, center.y),
                    Point::new(center.x + scale, center.y),
                ),
            );
        }
        MicroKind::Cross => {
            for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                scene.stroke(
                    &vello::kurbo::Stroke::new(width),
                    vello::kurbo::Affine::IDENTITY,
                    color,
                    None,
                    &vello::kurbo::Line::new(
                        Point::new(center.x + dx * scale * 0.3, center.y + dy * scale * 0.3),
                        Point::new(center.x + dx * scale, center.y + dy * scale),
                    ),
                );
            }
        }
        MicroKind::Bracket => {
            scene.stroke(
                &vello::kurbo::Stroke::new(width),
                vello::kurbo::Affine::IDENTITY,
                color,
                None,
                &vello::kurbo::Line::new(
                    Point::new(center.x - scale, center.y - scale),
                    Point::new(center.x + scale, center.y - scale),
                ),
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(width),
                vello::kurbo::Affine::IDENTITY,
                color,
                None,
                &vello::kurbo::Line::new(
                    Point::new(center.x - scale, center.y - scale),
                    Point::new(center.x - scale, center.y + scale),
                ),
            );
        }
        MicroKind::Scan => {
            scene.stroke(
                &vello::kurbo::Stroke::new(width),
                vello::kurbo::Affine::IDENTITY,
                color,
                None,
                &vello::kurbo::Line::new(
                    Point::new(center.x - scale * 1.6, center.y),
                    Point::new(center.x + scale * 1.6, center.y),
                ),
            );
        }
    }
}

fn same_stage(a: &Stage, b: &Stage) -> bool {
    (a.width - b.width).abs() <= f64::EPSILON
        && (a.height - b.height).abs() <= f64::EPSILON
        && (a.unit - b.unit).abs() <= f64::EPSILON
}
