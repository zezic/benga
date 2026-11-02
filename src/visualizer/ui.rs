mod performance;
mod pointer_click;

use pointer_click::{ClickKind, ClickSequence};
use std::time::{Duration, Instant};
use vello::{
    kurbo::{Affine, Point, Rect, Size},
    peniko::{Color, Fill, Mix},
    Scene,
};

use super::{
    controls::{ControlAction, ControlState, Controls},
    presentation::{AnalysisInteraction, Presentation},
    splash::Splash,
    DetectorSettings, Frame as VisualizerFrame, LineWidth, ModelComposite, ModelMotion, Palette,
    Parameter, Visualizer, VisualizerTuning,
};
use crate::{
    audio::{timeline::AudioInterval, AudioCursor},
    config::{
        Config, CursorVisibility, PreampDb, PresentationOpacity, ShutterFrames,
        VisualizerTextScale, Zoom,
    },
    frame_schedule::{Adjustment, FrameRateLimit, FrameTime},
    render::{Frame, PlateInstance, PlateMaskImage, RasterAsset},
    text::Fonts,
    theme::{Theme, ThemeColors, ThemeId, ThemePreferences, Tone},
    AudioEvent,
};

/// Window-relative drawing state for the presentation and its controls.
#[derive(Clone, Copy, PartialEq)]
pub struct View {
    pub(super) viewport: Size,
    zoom: Zoom,
    ui_scale: f64,
}

impl View {
    pub fn fit(viewport: Size) -> Self {
        assert!(
            viewport.width > 0.0 && viewport.height > 0.0,
            "a drawable viewport must have an area"
        );
        Self {
            viewport,
            zoom: Zoom::default(),
            ui_scale: 1.0,
        }
    }

    pub fn set_zoom(&mut self, zoom: Zoom) {
        self.zoom = zoom;
    }

    /// Physical pixels per logical UI point, independent of window size.
    pub fn ui_scale(&self) -> f64 {
        self.ui_scale
    }

    pub fn set_ui_scale(&mut self, scale: f64) {
        assert!(
            scale.is_finite() && scale > 0.0,
            "the UI scale must be finite and positive"
        );
        self.ui_scale = scale;
    }

    pub fn zoom_factor(&self) -> f64 {
        self.zoom.factor()
    }
}

/// The presentation, its optional analyzers, and the audio cursor.
pub struct Ui {
    frame_metrics: super::frame_metrics::FrameMetrics,
    performance: performance::PerformanceHud,
    renderer_stats_visible: bool,
    startup: Splash,
    pointer: Option<Point>,
    presentation_click: ClickSequence,
    text_scale: VisualizerTextScale,
    presentation_opacity: PresentationOpacity,
    detectors: DetectorSettings,
    visualizer: Visualizer,
    preamp: PreampDb,
    audio: AudioCursor,
    theme: Theme,
    fonts: Fonts,
    presentation: Presentation,
    poi_scene: Scene,
    controls: Controls,
    theme_preferences: ThemePreferences,
    theme_model_revision: super::ModelRevision,
    theme_rng: super::Rng,
    plate_mask: Option<PlateMaskImage>,
}

impl Ui {
    #[cfg(test)]
    pub(super) fn benchmark_visualizer_mut(&mut self) -> &mut Visualizer {
        &mut self.visualizer
    }

    fn model_composite(theme: ThemeId) -> ModelComposite {
        if theme.is_light() {
            ModelComposite::Multiplicative
        } else {
            ModelComposite::Additive
        }
    }

    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let selected = if config.theme_preferences.is_disliked(config.theme) {
            config
                .theme_preferences
                .navigate(config.theme, Adjustment::Increase)
                .expect("at least one theme is available")
        } else {
            config.theme
        };
        let mut fonts = Fonts::new()?;
        let mut visualizer = Visualizer::new();
        visualizer.set_model_composite(Self::model_composite(selected));
        visualizer.set_detector_settings(config.detectors.clone());
        visualizer.set_depth_fade(config.depth_fade);
        visualizer.set_near_opacity(config.near_opacity);
        visualizer.set_tuning(config.tuning.clone());
        visualizer.set_crease_threshold(config.crease_threshold);
        visualizer.set_line_width(config.line_width);
        visualizer.set_model_motion(config.model_motion);
        visualizer.set_model_rotation(config.model_rotation);
        visualizer.set_motion_opacity(config.motion_opacity);
        visualizer.set_motion_fade_power(config.motion_fade_power);
        visualizer.set_diagnosis_suppressed(true);
        let theme_model_revision = visualizer.model_revision();
        let theme = Theme::new(selected);
        let presentation = Presentation::new(&Palette::from_theme(selected), theme.colors);
        let startup = Splash::new(&mut fonts)?;
        Ok(Self {
            frame_metrics: super::frame_metrics::FrameMetrics::default(),
            performance: performance::PerformanceHud::default(),
            renderer_stats_visible: false,
            startup,
            pointer: None,
            presentation_click: ClickSequence::default(),
            text_scale: config.visualizer_text_scale,
            presentation_opacity: config.presentation_opacity,
            detectors: config.detectors.clone(),
            visualizer,
            preamp: config.preamp_db,
            audio: AudioCursor::new(),
            theme,
            fonts,
            presentation,
            poi_scene: Scene::new(),
            controls: Controls::new(Instant::now()),
            theme_preferences: config.theme_preferences.clone(),
            theme_model_revision,
            theme_rng: super::Rng::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system clock follows Unix epoch")
                    .as_nanos() as u64,
            ),
            plate_mask: None,
        })
    }

    pub fn audio_event(&mut self, event: AudioEvent) {
        match event {
            AudioEvent::Config { samplerate } => {
                self.audio.set_samplerate(samplerate);
                self.visualizer.set_samplerate(samplerate);
            }
            AudioEvent::Buffer { buf, captured_at } => self.consume_buf(&buf, captured_at),
        }
    }

    /// Draw over the complete app, in the normal-alpha top overlay. The
    /// returned flag stays true through the fade so controls can wait until
    /// the visualizer is visible. Call with a cleared scene each frame.
    pub fn draw_startup(&mut self, scene: &mut Scene, view: &View, now: Instant) -> bool {
        self.startup.draw(
            scene,
            view,
            self.theme.colors,
            Palette::from_theme(self.theme.selected()).neutral[0],
            now,
        )
    }

    /// Profiling and other synthetic runs should start with the visualizer.
    pub fn dismiss_startup(&mut self) {
        self.startup.dismiss();
    }

    pub fn startup_visible(&self) -> bool {
        self.startup.visible()
    }

    pub fn titlebar_logo_opacity(&self, now: Instant) -> f32 {
        self.startup.titlebar_logo_opacity(now)
    }

    pub fn scene_colors(&self) -> (ThemeColors, Color) {
        (self.theme.colors, self.presentation.colors().ink)
    }

    /// Borrowed output audio follows the standalone capture processing path.
    pub fn consume_buf(&mut self, buf: &[[f32; 2]], captured_at: Instant) {
        self.audio.push(buf.len(), captured_at);
        self.visualizer.consume_buf(buf, self.preamp.gain());
    }

    pub fn analyzers_visible(&self) -> bool {
        self.presentation.analyzers_visible()
    }

    pub fn toggle_analyzers(&mut self) {
        self.set_analyzers_visible(!self.analyzers_visible());
    }

    pub fn set_analyzers_visible(&mut self, visible: bool) {
        self.presentation_click.reset();
        self.presentation.set_analyzers_visible(visible);
        self.controls.activate(Instant::now());
    }

    pub fn renderer_stats_visible(&self) -> bool {
        self.renderer_stats_visible
    }

    pub fn toggle_renderer_stats(&mut self) {
        self.set_renderer_stats_visible(!self.renderer_stats_visible);
    }

    /// Developer overlays are session-only and do not alter saved preferences.
    pub fn set_renderer_stats_visible(&mut self, visible: bool) {
        self.renderer_stats_visible = visible;
        self.presentation_click.reset();
        self.controls.activate(Instant::now());
    }

    /// Supplies the GPU coverage mask used by moving contrast plates.
    pub fn set_plate_mask(&mut self, mask: Option<PlateMaskImage>) {
        self.plate_mask = mask;
    }

    pub fn set_pointer(&mut self, pointer: Option<Point>) {
        self.controls.pointer(pointer, Instant::now());
        if pointer.is_none() {
            self.presentation_click.reset();
            self.presentation.analysis_pointer_up();
        }
        if self.pointer == pointer {
            return;
        }
        self.pointer = pointer;
        if self.analysis_interactive() {
            if let Some((parameter, value)) = self.presentation.analysis_pointer_move(pointer) {
                self.set_parameter(parameter, value);
            }
        } else {
            self.presentation.analysis_pointer_up();
        }
    }

    /// Live detector settings, including analyzer edits, for config sync.
    pub fn detector_settings(&self) -> DetectorSettings {
        self.detectors.clone()
    }

    pub fn visualizer_tuning(&self) -> VisualizerTuning {
        self.visualizer.tuning()
    }

    fn set_parameter(&mut self, parameter: Parameter, value: f32) {
        self.visualizer.set_parameter(parameter, value);
        self.set_detector_settings(self.visualizer.detector_settings());
    }

    pub fn end_slider_drag(&mut self) {
        self.presentation.analysis_pointer_up();
    }

    pub fn set_theme(&mut self, selected: ThemeId) {
        if self.theme.selected() != selected {
            self.presentation
                .choose_colors(&Palette::from_theme(selected), Theme::new(selected).colors);
            self.theme.select(selected);
        }
    }

    pub fn validate_gpu_models(&mut self, capacity: usize) -> anyhow::Result<()> {
        self.visualizer.validate_gpu_models(capacity)
    }

    pub fn gpu_ribbon_status(&self) -> &'static str {
        self.visualizer.gpu_ribbon_status()
    }

    pub fn scene_name(&self) -> &str {
        self.visualizer.scene_name()
    }

    /// Host wall-clock work and completed present-call cadence; no GPU readback.
    pub fn record_frame_performance(
        &mut self,
        at: Instant,
        interval: Duration,
        cpu_work: Duration,
        downsize: f32,
        model_extent: Option<[u32; 2]>,
    ) {
        self.frame_metrics.record_rendered(
            at,
            interval,
            cpu_work,
            self.gpu_ribbon_status(),
            downsize,
            model_extent,
        );
    }

    /// Preserve the history while breaking the plot across hidden/unavailable frames.
    pub fn pause_frame_performance(&mut self) {
        self.frame_metrics.mark_break();
    }

    pub fn draw_performance(&mut self, scene: &mut Scene, view: &View, now: Instant) -> bool {
        if !self.renderer_stats_visible {
            return false;
        }
        self.performance.draw(
            scene,
            &mut self.fonts,
            &self.frame_metrics,
            view.viewport,
            view.ui_scale(),
            self.theme.colors.background,
            self.presentation.colors().ink,
            self.visualizer.gpu_ribbon_status(),
            now,
        );
        true
    }

    pub fn set_preamp(&mut self, preamp: PreampDb) {
        self.preamp = preamp;
    }

    pub fn set_presentation_opacity(&mut self, opacity: PresentationOpacity) {
        self.presentation_opacity = opacity;
        if !self.analysis_interactive() {
            self.presentation.analysis_pointer_up();
        }
    }

    pub fn set_detector_settings(&mut self, settings: DetectorSettings) {
        if self.detectors != settings {
            self.detectors = settings.clone();
            self.visualizer.set_detector_settings(settings);
        }
    }

    pub fn navigate_model(&mut self, adjustment: Adjustment) {
        self.visualizer.navigate_model(adjustment);
        self.auto_theme(Instant::now());
    }

    pub fn set_model_rotation(&mut self, rotation: super::ModelRotation) {
        self.visualizer.set_model_rotation(rotation);
    }

    pub fn selected_theme(&self) -> ThemeId {
        self.theme.selected()
    }

    pub fn theme_preferences(&self) -> &ThemePreferences {
        &self.theme_preferences
    }

    pub fn theme_preferences_mut(&mut self) -> &mut ThemePreferences {
        &mut self.theme_preferences
    }

    pub fn navigate_theme(&mut self, adjustment: Adjustment) {
        if let Some(theme) = self
            .theme_preferences
            .navigate(self.theme.selected(), adjustment)
        {
            self.choose_theme(theme);
        }
    }

    pub fn choose_tone(&mut self, tone: Tone) {
        if let Some(theme) = self.theme_preferences.remembered(tone) {
            self.choose_theme(theme);
        }
    }

    pub fn choose_theme(&mut self, theme: ThemeId) {
        self.theme_preferences.remember_manual(theme);
        self.set_theme(theme);
    }

    pub fn dislike_theme(&mut self) {
        let current = self.theme.selected();
        if self.theme_preferences.dislike(current) {
            let next = self
                .theme_preferences
                .navigate(current, Adjustment::Increase)
                .expect("disliking leaves an available theme");
            self.choose_theme(next);
        }
    }

    pub fn set_control_modifiers(&mut self, held: bool) {
        self.controls.modifiers(held, Instant::now());
    }

    pub fn click_controls(&mut self, view: &View, point: Point) -> Option<ControlAction> {
        self.click_controls_at(view, point, Instant::now())
    }

    fn click_controls_at(
        &mut self,
        view: &View,
        point: Point,
        now: Instant,
    ) -> Option<ControlAction> {
        let over_controls = self.controls.contains(point);
        let action = self.controls.click(point, now);
        if over_controls {
            self.presentation_click.reset();
            self.presentation.analysis_pointer_up();
            return action;
        }
        if self.analysis_interactive() {
            if let Some(interaction) = self.presentation.analysis_pointer_down(point) {
                self.presentation_click.reset();
                match interaction {
                    AnalysisInteraction::Parameter(parameter, value) => {
                        self.set_parameter(parameter, value)
                    }
                    AnalysisInteraction::ResetBins(detector) => {
                        self.visualizer.reset_onset_bins(detector);
                        self.set_detector_settings(self.visualizer.detector_settings());
                    }
                    AnalysisInteraction::Handled => {}
                }
                return None;
            }
            if self.presentation.analysis_contains(point) {
                self.presentation_click.reset();
                return None;
            }
        }
        match self
            .presentation_click
            .record(point, now, 6.0 * view.ui_scale())
        {
            ClickKind::Single => None,
            ClickKind::Double => Some(ControlAction::Fullscreen),
        }
    }

    pub fn reset_presentation_clicks(&mut self) {
        self.presentation_click.reset();
    }

    pub fn scroll_controls(&mut self, delta: f64) {
        self.controls.scroll(delta, Instant::now());
    }

    pub fn cursor_visible(&self, visibility: CursorVisibility) -> bool {
        self.pointer.is_none()
            || visibility == CursorVisibility::Always
            || self.controls.cursor_visible()
            || (self.analysis_interactive()
                && (self.presentation.analysis_dragging()
                    || self
                        .pointer
                        .is_some_and(|point| self.presentation.analysis_contains(point))))
    }

    fn analysis_interactive(&self) -> bool {
        self.analyzers_visible() && self.presentation_opacity.factor() > 0.01
    }

    pub fn controls_opacity(&self) -> f32 {
        self.controls.opacity()
    }

    pub fn reveal_controls(&mut self) {
        self.controls.activate(Instant::now());
    }

    /// Draws normal-alpha detector controls and the toolbar above the scene.
    /// Returns true when either contributes to the top overlay texture.
    pub fn draw_controls(
        &mut self,
        scene: &mut Scene,
        view: &View,
        config: &Config,
        fullscreen: bool,
        now: Instant,
    ) -> bool {
        scene.reset();
        // Keep interactive controls above statistics, including when UI scaling
        // puts the developer buttons inside the graph's screen bounds.
        let performance_visible = self.draw_performance(scene, view, now);
        let diagnostics_visible = self.draw_analysis_controls(scene);
        let analyzers_visible = self.analyzers_visible();
        let toolbar_visible = self.controls.draw(
            &mut self.fonts,
            scene,
            view,
            ControlState {
                config,
                theme: self.theme.selected(),
                model: self.visualizer.scene_name(),
                fullscreen,
                analyzers_visible,
                renderer_stats_visible: self.renderer_stats_visible,
                palette: self.presentation.colors(),
                background: self.theme.colors.background,
                history: self.visualizer.debug_history(),
            },
            now,
        );
        self.presentation.set_exclusion_bounds(if toolbar_visible {
            self.controls.exclusion_bounds()
        } else {
            None
        });
        diagnostics_visible || toolbar_visible || performance_visible
    }

    fn draw_analysis_controls(&self, scene: &mut Scene) -> bool {
        if !self.analysis_interactive() {
            return false;
        }
        let Some(stage) = self.presentation.stage() else {
            return false;
        };
        // These backplates must cover the model rather than add to or multiply
        // its color. Fade the bank as a group, independently of the toolbar.
        scene.push_layer(
            Fill::NonZero,
            Mix::Normal,
            self.presentation_opacity.factor(),
            Affine::IDENTITY,
            &Rect::new(0.0, 0.0, stage.width, stage.height),
        );
        self.presentation.draw_analysis_controls(
            scene,
            &stage,
            self.presentation.colors(),
            &self.theme,
        );
        scene.pop_layer();
        true
    }

    fn auto_theme(&mut self, now: Instant) {
        let revision = self.visualizer.model_revision();
        if revision != self.theme_model_revision {
            self.theme_model_revision = revision;
            if self.theme_preferences.auto_colors && self.controls.cooldown(now) == 0.0 {
                if let Some(theme) = self
                    .theme_preferences
                    .random(self.theme.selected(), self.theme_rng.unit())
                {
                    self.set_theme(theme);
                }
            }
        }
    }

    pub fn set_line_width(&mut self, width: LineWidth) {
        self.visualizer.set_line_width(width);
    }

    pub fn set_model_motion(&mut self, motion: ModelMotion) {
        self.visualizer.set_model_motion(motion);
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        view: &View,
        _limit: FrameRateLimit,
        frame_time: FrameTime,
    ) -> UiFrame<'_> {
        let frame_interval = self.audio.frame(frame_time.start(), frame_time.end());
        self.draw_at_audio_time(scene, view, frame_time, frame_interval)
    }

    /// Draw a recorded frame at an explicit sample interval, bypassing the
    /// live capture cursor's buffering and complete-bin availability policy.
    /// Feed the interval's samples through `consume_buf` before calling this.
    pub fn draw_at_audio_time(
        &mut self,
        scene: &mut Scene,
        view: &View,
        frame_time: FrameTime,
        frame_interval: AudioInterval,
    ) -> UiFrame<'_> {
        let dt = frame_time.seconds();
        let composite = Self::model_composite(self.theme.selected());
        self.visualizer.set_model_composite(composite);
        self.theme.advance(dt);
        let palette = Palette::from_theme(self.theme.selected());
        self.visualizer.update(
            frame_interval,
            ShutterFrames::default(),
            dt,
            &palette,
            (view.viewport.width as f32, view.viewport.height as f32),
            view.zoom_factor() as f32,
            view.ui_scale() as f32 * self.text_scale.factor(),
        );
        self.auto_theme(frame_time.end());
        self.presentation.update(
            dt,
            (view.viewport.width as f32, view.viewport.height as f32),
            &self.visualizer,
            &mut self.fonts,
            &self.visualizer.palette(),
            self.theme.colors,
        );
        let mut raster_assets = Vec::new();
        self.poi_scene.reset();
        if let Some(stage) = self.presentation.stage() {
            self.presentation.draw(
                scene,
                &mut raster_assets,
                &stage,
                self.presentation.colors(),
            );
            self.presentation.draw_poi(
                &mut self.poi_scene,
                &stage,
                self.presentation.colors(),
                self.plate_mask.as_ref(),
            );
        }
        UiFrame {
            background: Frame::Solid(self.theme.colors.background),
            overlay: self.visualizer.frame(),
            foreground: true,
            poi_overlay: (!self.poi_scene.encoding().draw_data.is_empty())
                .then_some(&self.poi_scene),
            overlay_composite: Some(composite),
            overlay_opacity: self.presentation_opacity.factor(),
            plate_instances: self.presentation.plate_instances(),
            plate_ink: self.presentation.colors().ink,
            raster_assets,
        }
    }
}

/// Background, main infographics, model, and foreground POI callouts.
pub struct UiFrame<'a> {
    pub background: Frame<'a>,
    /// Retained vector sources referenced by the foreground scene.
    pub raster_assets: Vec<RasterAsset>,
    pub overlay: VisualizerFrame<'a>,
    /// Whether the foreground scene carries anything to rasterize.
    pub foreground: bool,
    /// POI callouts rasterized separately above the model wireframe.
    pub poi_overlay: Option<&'a Scene>,
    /// Composition mode for presentation text; `None` keeps normal blending.
    pub overlay_composite: Option<ModelComposite>,
    /// Multiplier applied to both presentation graphic layers.
    pub overlay_opacity: f32,
    /// Moving plate coverage requests for the GPU mask pass.
    pub plate_instances: &'a [PlateInstance],
    /// Ink color of the moving plate masks.
    pub plate_ink: Color,
}

#[cfg(test)]
mod cache_regression_tests;
#[cfg(test)]
mod graph_previews;
#[cfg(test)]
mod transition_profile_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::RasterAssetCache;
    use crate::visualizer::{ArrangementSetting, Detector, PercussionSetting};
    use std::time::Duration;

    #[test]
    fn performance_history_records_while_hidden_and_survives_visibility_toggles(
    ) -> anyhow::Result<()> {
        let mut ui = Ui::new(&Config::default())?;
        let now = Instant::now();
        let view = View::fit(Size::new(1280.0, 720.0));
        let mut scene = Scene::new();
        ui.record_frame_performance(
            now,
            Duration::from_millis(16),
            Duration::from_millis(4),
            2.0,
            Some([640, 360]),
        );
        assert_eq!(ui.frame_metrics.samples().count(), 1);
        assert!(!ui.renderer_stats_visible());
        assert!(!ui.draw_performance(&mut scene, &view, now));
        assert!(scene.encoding().draw_data.is_empty());
        ui.set_renderer_stats_visible(true);
        assert!(ui.draw_performance(&mut scene, &view, now));
        ui.set_renderer_stats_visible(false);
        let next = now + Duration::from_millis(32);
        ui.record_frame_performance(
            next,
            Duration::from_millis(32),
            Duration::from_millis(12),
            1.0,
            Some([1280, 720]),
        );
        scene.reset();
        assert!(!ui.draw_performance(&mut scene, &view, next));
        assert!(scene.encoding().draw_data.is_empty());
        let samples: Vec<_> = ui.frame_metrics.samples().collect();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].downsize, 2.0);
        assert_eq!(samples[0].model_extent, Some([640, 360]));
        assert_eq!(samples[1].downsize, 1.0);
        assert_eq!(samples[1].model_extent, Some([1280, 720]));
        ui.set_renderer_stats_visible(true);
        assert!(ui.draw_performance(&mut scene, &view, next));
        // Explicitly enabled graphs remain visible after the normal controls fade.
        assert!(ui.draw_controls(
            &mut scene,
            &view,
            &Config::default(),
            false,
            next + Duration::from_secs(15)
        ));
        assert_eq!(ui.frame_metrics.samples().count(), 2);
        Ok(())
    }

    #[test]
    fn performance_pause_keeps_the_old_trace_and_breaks_the_new_segment() -> anyhow::Result<()> {
        let mut ui = Ui::new(&Config::default())?;
        let now = Instant::now();
        ui.record_frame_performance(
            now,
            Duration::from_millis(16),
            Duration::from_millis(4),
            2.0,
            Some([640, 360]),
        );
        ui.pause_frame_performance();
        assert!(ui.frame_metrics.snapshot(now).is_none());
        assert_eq!(ui.frame_metrics.samples().count(), 1);
        let resumed = now + Duration::from_secs(2);
        ui.record_frame_performance(
            resumed,
            Duration::from_millis(20),
            Duration::from_millis(5),
            4.0,
            Some([320, 180]),
        );
        assert_eq!(ui.frame_metrics.samples().count(), 2);
        assert!(ui.frame_metrics.samples().last().unwrap().break_before);
        assert_eq!(ui.frame_metrics.snapshot(resumed).unwrap().fps, 50.0);
        Ok(())
    }

    fn drawn_presentation_ui() -> (Ui, View) {
        let mut ui = Ui::new(&Config::default()).expect("the bundled fonts and models load");
        ui.set_analyzers_visible(true);
        let view = View::fit(Size::new(1920.0, 1080.0));
        draw_presentation_frame(&mut ui, &view);
        (ui, view)
    }

    fn draw_presentation_frame(ui: &mut Ui, view: &View) {
        let now = Instant::now();
        let mut scene = Scene::new();
        let _ = ui.draw(
            &mut scene,
            view,
            FrameRateLimit::Fps120,
            FrameTime::new(now, now + Duration::from_millis(16)),
        );
    }

    #[test]
    fn recorded_frames_follow_sample_intervals_at_arbitrary_frame_rates() -> anyhow::Result<()> {
        use crate::audio::timeline::AudioTime;

        const SAMPLERATE: usize = 48_000;
        for fps in [59.0, 29.97] {
            let mut config = Config::default();
            config.theme_preferences.auto_colors = false;
            config.model_rotation = crate::visualizer::ModelRotation::Locked;
            config.model_motion = ModelMotion::Swept;
            let mut ui = Ui::new(&config)?;
            ui.audio_event(AudioEvent::Config {
                samplerate: SAMPLERATE,
            });
            let view = View::fit(Size::new(640.0, 360.0));
            let mut scene = Scene::new();
            let origin = Instant::now();
            let mut previous_sample = 0;

            for index in 0..12 {
                let start = origin + Duration::from_secs_f64(index as f64 / fps);
                let end = origin + Duration::from_secs_f64((index + 1) as f64 / fps);
                let next_sample = ((index + 1) as f64 * SAMPLERATE as f64 / fps) as u64;
                ui.consume_buf(
                    &vec![[0.0; 2]; (next_sample - previous_sample) as usize],
                    origin + Duration::from_secs_f64(previous_sample as f64 / SAMPLERATE as f64),
                );
                let interval = AudioInterval::new(
                    AudioTime::from_samples(previous_sample, SAMPLERATE),
                    AudioTime::from_samples(next_sample, SAMPLERATE),
                );
                scene.reset();
                let frame =
                    ui.draw_at_audio_time(&mut scene, &view, FrameTime::new(start, end), interval);
                let latest_pose_at = frame
                    .overlay
                    .gpu_models
                    .first()
                    .expect("GPU projection records each frame")
                    .at;
                assert_eq!(
                    latest_pose_at,
                    interval.end().seconds_since(AudioTime::ZERO),
                    "{fps} FPS frame {index} must use its sample interval without waiting for a complete millisecond"
                );
                assert!(
                    (ui.visualizer.time - (index + 1) as f32 / fps as f32).abs() < 1e-6,
                    "animation must advance by the recorded frame duration"
                );
                previous_sample = next_sample;
            }
        }
        Ok(())
    }

    #[test]
    fn presentation_accent_stays_fixed_across_scene_modes() -> anyhow::Result<()> {
        use crate::visualizer::{showcase::ShowcaseMode, Rng};

        let mut config = Config::default();
        config.theme_preferences.auto_colors = false;
        let mut ui = Ui::new(&config)?;
        ui.visualizer.load_models();
        for _ in 0..64 {
            if ui.visualizer.model_id() == Some("rif_ak47") {
                break;
            }
            ui.visualizer.next_model();
        }
        assert_eq!(ui.visualizer.model_id(), Some("rif_ak47"));
        let view = View::fit(Size::new(1920.0, 1080.0));
        let accent = ui.presentation.colors().accent;
        draw_presentation_frame(&mut ui, &view);
        assert_eq!(ui.presentation.colors().accent, accent);
        let mut rng = Rng::new(7);
        for _ in 0..8 {
            assert!(ui.visualizer.showcase.enter_focus(&mut rng, 1.1));
            draw_presentation_frame(&mut ui, &view);
            assert_eq!(ui.visualizer.showcase_mode(), ShowcaseMode::Poi);
            assert_eq!(
                ui.presentation.colors().accent,
                accent,
                "POI entry changed the accent"
            );
            ui.visualizer.showcase.exit_focus();
            draw_presentation_frame(&mut ui, &view);
            assert_eq!(ui.visualizer.showcase_mode(), ShowcaseMode::Normal);
            assert_eq!(
                ui.presentation.colors().accent,
                accent,
                "POI exit changed the accent"
            );
        }
        ui.navigate_model(Adjustment::Increase);
        draw_presentation_frame(&mut ui, &view);
        assert_eq!(
            ui.presentation.colors().accent,
            accent,
            "changing models with automatic colors disabled keeps the accent"
        );
        Ok(())
    }

    #[test]
    fn presentation_chooses_one_accent_for_each_theme() -> anyhow::Result<()> {
        let mut config = Config::default();
        config.theme_preferences.auto_colors = false;
        let mut ui = Ui::new(&config)?;
        let view = View::fit(Size::new(1920.0, 1080.0));
        draw_presentation_frame(&mut ui, &view);
        for name in ["Nord Light", "Gruvbox Dark", "Nord Light"] {
            let theme = ThemeId::all().find(|theme| theme.name() == name).unwrap();
            let palette = Palette::from_theme(theme);
            ui.set_theme(theme);
            let chosen = ui.presentation.colors();
            assert_eq!(
                chosen.accent,
                palette.vivid(chosen.accent_index),
                "choose from the incoming theme"
            );
            for _ in 0..120 {
                draw_presentation_frame(&mut ui, &view);
                assert_eq!(
                    ui.presentation.colors().accent_index,
                    chosen.accent_index,
                    "crossfading must keep the chosen accent slot"
                );
            }
            let settled = ui.presentation.colors().accent;
            for (actual, expected) in settled.components.into_iter().zip(chosen.accent.components) {
                assert!((actual - expected).abs() < 1e-6);
            }
            ui.set_theme(theme);
            assert_eq!(
                ui.presentation.colors().accent,
                settled,
                "selecting the current theme keeps its accent"
            );
        }
        Ok(())
    }

    #[test]
    fn analyzers_start_hidden_and_the_modifier_button_toggles_them() -> anyhow::Result<()> {
        let config = Config::default();
        let mut ui = Ui::new(&config)?;
        let view = View::fit(Size::new(1920.0, 1080.0));
        let now = Instant::now();
        let mut scene = Scene::new();
        draw_presentation_frame(&mut ui, &view);
        assert!(!ui.analyzers_visible());
        assert!(!ui.analysis_interactive());
        ui.set_control_modifiers(true);
        ui.draw_controls(&mut scene, &view, &config, false, now);
        let button = ui
            .controls
            .target_center(|action| matches!(action, ControlAction::ToggleAnalyzers));
        assert!(matches!(
            ui.click_controls_at(&view, button, now),
            Some(ControlAction::ToggleAnalyzers)
        ));
        ui.toggle_analyzers();
        draw_presentation_frame(&mut ui, &view);
        assert!(ui.analyzers_visible());
        assert!(ui.analysis_interactive());
        ui.draw_controls(&mut scene, &view, &config, false, now);
        assert!(matches!(
            ui.click_controls_at(&view, button, now),
            Some(ControlAction::ToggleAnalyzers)
        ));
        ui.toggle_analyzers();
        assert!(!ui.analysis_interactive());
        Ok(())
    }

    #[test]
    fn renderer_stats_start_hidden_and_the_modifier_button_toggles_them_independently(
    ) -> anyhow::Result<()> {
        let config = Config::default();
        let mut ui = Ui::new(&config)?;
        let view = View::fit(Size::new(1920.0, 1080.0));
        let now = Instant::now();
        let mut scene = Scene::new();
        assert!(!ui.renderer_stats_visible());
        assert!(!ui.draw_performance(&mut scene, &view, now));
        assert!(scene.encoding().draw_data.is_empty());
        ui.set_control_modifiers(true);
        ui.draw_controls(&mut scene, &view, &config, false, now);
        let stats = ui
            .controls
            .target_center(|action| matches!(action, ControlAction::ToggleRendererStats));
        assert!(matches!(
            ui.click_controls_at(&view, stats, now),
            Some(ControlAction::ToggleRendererStats)
        ));
        ui.toggle_renderer_stats();
        assert!(ui.renderer_stats_visible());
        assert!(!ui.analyzers_visible());
        ui.toggle_analyzers();
        assert!(ui.analyzers_visible());
        assert!(ui.renderer_stats_visible());
        ui.set_control_modifiers(false);
        assert!(!matches!(
            ui.click_controls_at(&view, stats, now),
            Some(ControlAction::ToggleRendererStats)
        ));
        scene.reset();
        assert!(ui.draw_performance(&mut scene, &view, now));
        ui.set_control_modifiers(true);
        ui.draw_controls(&mut scene, &view, &config, false, now);
        assert!(matches!(
            ui.click_controls_at(&view, stats, now),
            Some(ControlAction::ToggleRendererStats)
        ));
        ui.toggle_renderer_stats();
        assert!(!ui.renderer_stats_visible());
        assert!(ui.analyzers_visible());
        scene.reset();
        assert!(!ui.draw_performance(&mut scene, &view, now));
        assert!(scene.encoding().draw_data.is_empty());
        assert!(!Ui::new(&config)?.renderer_stats_visible());
        Ok(())
    }

    #[test]
    fn hiding_analyzers_cancels_drags_and_releases_their_hit_areas() {
        let (mut ui, view) = drawn_presentation_ui();
        let parameter = Parameter::Arrangement(ArrangementSetting::ShapeDistance);
        let slider = ui
            .presentation
            .analysis_slider_center(parameter)
            .expect("shape slider is visible");
        let graph = ui.presentation.analysis_graph_bounds().unwrap().center();
        let now = Instant::now();
        ui.set_pointer(Some(slider));
        assert!(ui.click_controls_at(&view, slider, now).is_none());
        assert!(ui.presentation.analysis_dragging());
        ui.set_analyzers_visible(false);
        assert!(!ui.presentation.analysis_dragging());
        assert!(!ui.presentation.analysis_contains(graph));
        let settings = ui.detector_settings();
        ui.set_pointer(Some(Point::new(slider.x + 100.0, slider.y)));
        assert_eq!(ui.detector_settings(), settings);
        assert!(ui
            .click_controls_at(&view, graph, now + Duration::from_millis(10))
            .is_none());
        assert!(matches!(
            ui.click_controls_at(&view, graph, now + Duration::from_millis(30)),
            Some(ControlAction::Fullscreen)
        ));
        assert_eq!(ui.presentation.analysis_selected_detector(), Detector::Kick);
    }

    #[test]
    fn presentation_onset_graph_cycles_channels_without_background_actions() {
        let (mut ui, view) = drawn_presentation_ui();
        let graph = ui.presentation.analysis_graph_bounds().unwrap().center();
        let now = Instant::now();
        let settings = ui.detector_settings();
        ui.visualizer.consume_buf(&[[0.2, -0.2]; 2048], 1.0);
        let revision = ui.visualizer.analysis_revision();
        assert!(revision > 0);
        assert_eq!(ui.presentation.analysis_selected_detector(), Detector::Kick);
        for (index, expected) in [Detector::Snare, Detector::Hat, Detector::Kick]
            .into_iter()
            .enumerate()
        {
            assert!(ui
                .click_controls_at(&view, graph, now + Duration::from_millis(index as u64 * 10))
                .is_none());
            assert_eq!(ui.presentation.analysis_selected_detector(), expected);
        }
        assert_eq!(ui.visualizer.analysis_revision(), revision);
        assert_eq!(ui.detector_settings(), settings);
        let background = Point::new(10.0, 10.0);
        assert!(ui
            .click_controls_at(&view, background, now + Duration::from_millis(40))
            .is_none());
        assert!(matches!(
            ui.click_controls_at(&view, background, now + Duration::from_millis(50)),
            Some(ControlAction::Fullscreen)
        ));
    }

    #[test]
    fn presentation_detector_slider_drags_update_live_and_saved_settings() {
        let (mut ui, view) = drawn_presentation_ui();
        let mut settings = ui.detector_settings();
        settings.percussion.channels[0].filters.sharpness_percentile = true;
        settings.percussion.channels[0].sharpness_percentile = 60.0;
        ui.set_detector_settings(settings);
        draw_presentation_frame(&mut ui, &view);
        let now = Instant::now();
        for parameter in [
            Parameter::Arrangement(ArrangementSetting::MixLossDb),
            Parameter::Percussion(Detector::Kick, PercussionSetting::SharpnessPercentile),
        ] {
            let center = ui.presentation.analysis_slider_center(parameter).unwrap();
            let before = ui.visualizer.parameter_value(parameter);
            assert!(ui.click_controls_at(&view, center, now).is_none());
            let pressed = ui.visualizer.parameter_value(parameter);
            assert_ne!(pressed, before, "the slider must edit its live parameter");
            assert!(ui.presentation.analysis_dragging());
            ui.set_pointer(Some(Point::new(center.x + 100.0, center.y)));
            let dragged = ui.visualizer.parameter_value(parameter);
            assert!(dragged > pressed);
            ui.end_slider_drag();
            assert!(!ui.presentation.analysis_dragging());
            ui.set_pointer(Some(Point::new(center.x - 100.0, center.y)));
            assert_eq!(ui.visualizer.parameter_value(parameter), dragged);
        }
        let graph = ui.presentation.analysis_graph_bounds().unwrap().center();
        ui.click_controls_at(&view, graph, now);
        draw_presentation_frame(&mut ui, &view);
        let snare_gate = ui
            .presentation
            .analysis_slider_center(Parameter::Percussion(
                Detector::Snare,
                PercussionSetting::OutputRmsDb,
            ))
            .unwrap();
        ui.click_controls_at(&view, snare_gate, now);
        ui.end_slider_drag();
        let detectors = ui.detector_settings();
        assert_eq!(detectors, ui.visualizer.detector_settings());
        assert_ne!(
            detectors.percussion.channels[1].output_rms_db,
            DetectorSettings::default().percussion.channels[1].output_rms_db
        );
        let mut config = Config::default();
        config.detectors = detectors.clone();
        let restored: Config = ron::from_str(&ron::to_string(&config).unwrap()).unwrap();
        assert_eq!(
            restored.detectors, detectors,
            "presentation tuning survives config save/load"
        );
    }

    #[test]
    fn heatmap_marker_count_controls_update_saved_settings_and_respect_opacity() {
        let (mut ui, view) = drawn_presentation_ui();
        let now = Instant::now();
        let more = ui.presentation.analysis_marker_count_center(true).unwrap();
        assert!(ui.click_controls_at(&view, more, now).is_none());
        assert_eq!(ui.detector_settings().percussion.recent_markers, 36);
        assert_eq!(
            ui.visualizer.detector_settings().percussion.recent_markers,
            36
        );
        draw_presentation_frame(&mut ui, &view);
        let less = ui.presentation.analysis_marker_count_center(false).unwrap();
        ui.click_controls_at(&view, less, now);
        assert_eq!(ui.detector_settings().percussion.recent_markers, 32);
        ui.set_presentation_opacity(PresentationOpacity::try_from(0.0).unwrap());
        ui.click_controls_at(&view, more, now);
        assert_eq!(ui.detector_settings().percussion.recent_markers, 32);
        assert!(!ui.presentation.analysis_dragging());
    }

    #[test]
    fn presentation_physical_masks_clamp_persist_and_follow_channel() {
        let (mut ui, view) = drawn_presentation_ui();
        let now = Instant::now();
        let low = ui
            .presentation
            .analysis_slider_center(Parameter::Percussion(
                Detector::Kick,
                PercussionSetting::LowHz,
            ))
            .expect("kick exposes its low frequency");
        ui.click_controls_at(&view, low, now);
        ui.set_pointer(Some(Point::new(low.x + 1000.0, low.y)));
        ui.end_slider_drag();
        let settings = ui.detector_settings().percussion.channels[0];
        assert!(settings.low_hz <= settings.high_hz - 10.0 + 1e-4);
        assert!((settings.high_hz - settings.low_hz - 10.0).abs() < 1e-4);

        draw_presentation_frame(&mut ui, &view);
        let high = ui
            .presentation
            .analysis_slider_center(Parameter::Percussion(
                Detector::Kick,
                PercussionSetting::HighHz,
            ))
            .expect("kick exposes its high frequency");
        ui.click_controls_at(&view, high, now);
        ui.set_pointer(Some(Point::new(high.x - 1000.0, high.y)));
        let settings = ui.detector_settings().percussion.channels[0];
        assert!(settings.high_hz >= settings.low_hz + 10.0 - 1e-4);
        ui.set_pointer(Some(Point::new(high.x + 1000.0, high.y)));
        ui.end_slider_drag();
        let tuned = ui.detector_settings();
        assert_eq!(tuned.percussion.channels[0].high_hz, 24_000.0);
        assert_ne!(
            tuned.percussion.channels[0].low_hz,
            DetectorSettings::default().percussion.channels[0].low_hz
        );

        let mut config = Config::default();
        config.detectors = tuned.clone();
        let restored: Config = ron::from_str(&ron::to_string(&config).unwrap()).unwrap();
        assert_eq!(
            restored.detectors.percussion.channels[0],
            tuned.percussion.channels[0]
        );

        let graph = ui.presentation.analysis_graph_bounds().unwrap().center();
        for channel in [Detector::Snare, Detector::Hat, Detector::Kick] {
            ui.click_controls_at(&view, graph, now);
            draw_presentation_frame(&mut ui, &view);
            assert_eq!(ui.presentation.analysis_selected_detector(), channel);
            for setting in [PercussionSetting::LowHz, PercussionSetting::HighHz] {
                assert!(
                    ui.presentation
                        .analysis_slider_center(Parameter::Percussion(channel, setting))
                        .is_some(),
                    "each channel exposes its own physical mask"
                );
            }
        }
        assert_eq!(ui.detector_settings(), tuned);
    }

    #[test]
    fn presentation_bin_faders_edit_persist_and_reset_only_the_selected_detector() {
        let (mut ui, view) = drawn_presentation_ui();
        ui.reveal_controls();
        let mut controls_scene = Scene::new();
        assert!(ui.draw_controls(
            &mut controls_scene,
            &view,
            &Config::default(),
            false,
            Instant::now()
        ));
        let now = Instant::now();
        let kick_parameter = ui
            .presentation
            .analysis_first_bin_parameter()
            .expect("kick bins are available before capture");
        let kick_fader = ui
            .presentation
            .analysis_slider_center(kick_parameter)
            .unwrap();
        assert!(
            !ui.controls.contains(kick_fader),
            "the toolbar must not cover bin faders"
        );
        assert_eq!(ui.visualizer.parameter_value(kick_parameter), 0.0637074);
        assert!(ui.click_controls_at(&view, kick_fader, now).is_none());
        assert!(ui.presentation.analysis_dragging());
        ui.set_pointer(Some(Point::new(kick_fader.x, kick_fader.y + 1000.0)));
        ui.end_slider_drag();
        assert_eq!(ui.visualizer.parameter_value(kick_parameter), 0.0);
        assert_eq!(ui.detector_settings(), ui.visualizer.detector_settings());

        let graph = ui.presentation.analysis_graph_bounds().unwrap().center();
        ui.click_controls_at(&view, graph, now);
        draw_presentation_frame(&mut ui, &view);
        assert_eq!(
            ui.presentation.analysis_selected_detector(),
            Detector::Snare
        );
        let snare_parameter = ui.presentation.analysis_first_bin_parameter().unwrap();
        let fader = ui
            .presentation
            .analysis_slider_center(snare_parameter)
            .unwrap();
        ui.click_controls_at(&view, fader, now);
        ui.set_pointer(Some(Point::new(fader.x, fader.y + 1000.0)));
        ui.end_slider_drag();
        assert_eq!(ui.visualizer.parameter_value(snare_parameter), 0.0);

        let mut config = Config::default();
        config.detectors = ui.detector_settings();
        let restored: Config = ron::from_str(&ron::to_string(&config).unwrap()).unwrap();
        assert_eq!(restored.detectors.bin_gains, config.detectors.bin_gains);
        let reset = ui.presentation.analysis_bin_reset_center().unwrap();
        assert!(
            !ui.controls.contains(reset),
            "the toolbar must not cover bin reset"
        );
        assert!(ui.click_controls_at(&view, reset, now).is_none());
        assert_eq!(ui.visualizer.parameter_value(snare_parameter), 1.0);
        assert_eq!(ui.visualizer.parameter_value(kick_parameter), 0.0);
        assert_eq!(ui.detector_settings(), ui.visualizer.detector_settings());
    }

    #[test]
    fn presentation_bin_paging_does_not_cycle_channels_or_edit_gains() {
        let (mut ui, view) = drawn_presentation_ui();
        let now = Instant::now();
        let graph = ui.presentation.analysis_graph_bounds().unwrap().center();
        ui.click_controls_at(&view, graph, now);
        draw_presentation_frame(&mut ui, &view);
        let first = ui.presentation.analysis_first_bin_parameter().unwrap();
        let settings = ui.detector_settings();
        let next = ui.presentation.analysis_bin_next_center().unwrap();
        assert!(ui.click_controls_at(&view, next, now).is_none());
        draw_presentation_frame(&mut ui, &view);
        assert_eq!(
            ui.presentation.analysis_selected_detector(),
            Detector::Snare
        );
        assert_ne!(
            ui.presentation.analysis_first_bin_parameter().unwrap(),
            first
        );
        assert_eq!(ui.detector_settings(), settings);
        assert!(!ui.presentation.analysis_dragging());
    }

    #[test]
    fn presentation_diagnostic_hover_and_drag_keep_the_cursor_visible() {
        let config = Config::default();
        let (mut ui, view) = drawn_presentation_ui();
        let slider = ui
            .presentation
            .analysis_slider_center(Parameter::Arrangement(ArrangementSetting::GapHold))
            .unwrap();
        ui.set_pointer(Some(slider));
        let mut scene = Scene::new();
        let hidden_at = Instant::now() + Duration::from_secs(11);
        assert!(ui.draw_controls(&mut scene, &view, &config, false, hidden_at));
        assert_eq!(
            ui.controls_opacity(),
            0.0,
            "the toolbar can hide while detector controls remain visible"
        );
        assert!(
            !scene.encoding().draw_data.is_empty(),
            "diagnostics must keep the normal-alpha overlay active"
        );
        assert!(ui.cursor_visible(CursorVisibility::AutoHide));
        ui.click_controls_at(&view, slider, hidden_at);
        assert!(ui.presentation.analysis_dragging());
        ui.set_pointer(Some(Point::new(960.0, 400.0)));
        assert!(ui.cursor_visible(CursorVisibility::AutoHide));
        ui.end_slider_drag();
        assert!(!ui.cursor_visible(CursorVisibility::AutoHide));
        ui.set_pointer(None);
        assert!(!ui.presentation.analysis_dragging());
    }

    #[test]
    fn transparent_presentation_diagnostics_release_drag_and_ignore_pointer_input() {
        let config = Config::default();
        let (mut ui, view) = drawn_presentation_ui();
        let parameter = Parameter::Arrangement(ArrangementSetting::MixLossDb);
        let slider = ui.presentation.analysis_slider_center(parameter).unwrap();
        ui.click_controls_at(&view, slider, Instant::now());
        assert!(ui.presentation.analysis_dragging());
        ui.set_presentation_opacity(PresentationOpacity::try_from(0.0).unwrap());
        assert!(!ui.presentation.analysis_dragging());
        let value = ui.visualizer.parameter_value(parameter);
        ui.set_pointer(Some(Point::new(slider.x + 100.0, slider.y)));
        assert_eq!(ui.visualizer.parameter_value(parameter), value);

        let now = Instant::now() + Duration::from_secs(11);
        let mut scene = Scene::new();
        assert!(!ui.draw_controls(&mut scene, &view, &config, false, now));
        assert!(
            scene.encoding().draw_data.is_empty(),
            "zero-opacity diagnostics must not leave a backplate"
        );
        assert!(!ui.cursor_visible(CursorVisibility::AutoHide));
        let graph = ui.presentation.analysis_graph_bounds().unwrap().center();
        assert!(ui.click_controls_at(&view, graph, now).is_none());
        assert!(matches!(
            ui.click_controls_at(&view, graph, now + Duration::from_millis(50)),
            Some(ControlAction::Fullscreen)
        ));
        assert_eq!(ui.presentation.analysis_selected_detector(), Detector::Kick);
    }

    #[test]
    fn background_click_pairs_use_logical_distance_and_reset_with_presentation_context(
    ) -> anyhow::Result<()> {
        let mut ui = Ui::new(&Config::default())?;
        let mut view = View::fit(Size::new(1920.0, 1080.0));
        view.set_ui_scale(2.0);
        let now = Instant::now();
        let first = Point::new(20.0, 20.0);
        let second = Point::new(31.0, 20.0);
        for pair in 0..2 {
            let at = now + Duration::from_millis(pair * 100);
            assert!(ui.click_controls_at(&view, first, at).is_none());
            assert!(matches!(
                ui.click_controls_at(&view, second, at + Duration::from_millis(50)),
                Some(ControlAction::Fullscreen)
            ));
        }
        let at = now + Duration::from_millis(300);
        assert!(ui.click_controls_at(&view, first, at).is_none());
        ui.set_pointer(None);
        assert!(ui
            .click_controls_at(&view, first, at + Duration::from_millis(10))
            .is_none());
        ui.toggle_analyzers();
        assert!(ui
            .click_controls_at(&view, first, at + Duration::from_millis(20))
            .is_none());
        ui.reset_presentation_clicks();
        assert!(ui
            .click_controls_at(&view, first, at + Duration::from_millis(30))
            .is_none());
        Ok(())
    }

    #[test]
    fn visible_controls_hints_and_popups_interrupt_background_click_pairs() -> anyhow::Result<()> {
        let config = Config::default();
        let mut ui = Ui::new(&config)?;
        let view = View::fit(Size::new(1920.0, 1080.0));
        let now = Instant::now();
        let mut scene = Scene::new();
        ui.draw_controls(&mut scene, &view, &config, false, now);
        let auto = ui
            .controls
            .target_center(|action| matches!(action, ControlAction::AutoColors));
        let tone = ui
            .controls
            .target_center(|action| matches!(action, ControlAction::Tone(_)));
        let disabled_contrast = Point::new(auto.x, tone.y);
        let hint = Point::new(
            auto.x,
            ui.controls.exclusion_bounds().expect("controls visible").y0 + 1.0,
        );
        let boost = ui
            .controls
            .target_center(|action| matches!(action, ControlAction::Boost(_)));
        ui.set_pointer(Some(boost));
        ui.draw_controls(&mut scene, &view, &config, false, now);
        let graph = Point::new(
            boost.x,
            ui.controls.exclusion_bounds().expect("graph visible").y0 + 20.0,
        );
        let background = Point::new(10.0, 10.0);
        for (index, point) in [disabled_contrast, hint, graph, auto]
            .into_iter()
            .enumerate()
        {
            let at = now + Duration::from_millis(index as u64 * 50);
            assert!(ui.click_controls_at(&view, background, at).is_none());
            assert!(!matches!(
                ui.click_controls_at(&view, point, at + Duration::from_millis(10)),
                Some(ControlAction::Fullscreen)
            ));
            assert!(ui
                .click_controls_at(&view, background, at + Duration::from_millis(20))
                .is_none());
            assert!(matches!(
                ui.click_controls_at(&view, background, at + Duration::from_millis(30)),
                Some(ControlAction::Fullscreen)
            ));
        }
        Ok(())
    }

    #[test]
    fn hidden_controls_wake_without_receiving_stale_actions() -> anyhow::Result<()> {
        let config = Config::default();
        let mut ui = Ui::new(&config)?;
        let view = View::fit(Size::new(1920.0, 1080.0));
        let now = Instant::now();
        let mut scene = Scene::new();
        ui.draw_controls(&mut scene, &view, &config, false, now);
        let auto = ui
            .controls
            .target_center(|action| matches!(action, ControlAction::AutoColors));
        let hidden_at = now + Duration::from_secs(11);
        assert!(!ui.draw_controls(&mut scene, &view, &config, false, hidden_at));
        assert!(ui.click_controls_at(&view, auto, hidden_at).is_none());
        assert!(matches!(
            ui.click_controls_at(&view, auto, hidden_at + Duration::from_millis(50)),
            Some(ControlAction::Fullscreen)
        ));
        let next = now + Duration::from_secs(12);
        assert!(ui.click_controls_at(&view, auto, next).is_none());
        assert!(ui.draw_controls(
            &mut scene,
            &view,
            &config,
            false,
            next + Duration::from_millis(100)
        ));
        assert!(matches!(
            ui.click_controls_at(&view, auto, next + Duration::from_millis(150)),
            Some(ControlAction::AutoColors)
        ));
        assert!(ui
            .click_controls_at(
                &view,
                Point::new(10.0, 10.0),
                next + Duration::from_millis(200)
            )
            .is_none());
        Ok(())
    }

    pub(super) fn write_preview(
        path: &std::path::Path,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> anyhow::Result<()> {
        use std::{fs::File, io::BufWriter};
        let mut encoder = png::Encoder::new(BufWriter::new(File::create(path)?), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(pixels)?;
        Ok(())
    }

    pub(super) fn presentation_pixels(
        ui: &mut Ui,
        view: &View,
        frames: usize,
        feed_audio: bool,
        controls_config: Option<&Config>,
        mut before_frame: impl FnMut(usize, &mut Ui),
    ) -> anyhow::Result<Vec<u8>> {
        use vello::{kurbo::Size, util::RenderContext, wgpu, RenderParams};

        let width = view.viewport.width as u32;
        let height = view.viewport.height as u32;
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let mut context = RenderContext::new();
        let id = pollster::block_on(crate::render::prepare_device(
            &mut context,
            None,
            wgpu::DeviceDescriptor::default(),
        ))?;
        let device = &context.devices[id];
        let make_texture = |label: &str, usage: wgpu::TextureUsages| {
            device.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let background = make_texture(
            "Presentation background",
            wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let foreground = make_texture(
            "Presentation foreground",
            wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let poi = make_texture(
            "Presentation POI callouts",
            wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let poi_view = poi.create_view(&wgpu::TextureViewDescriptor::default());
        let top = make_texture(
            "Presentation controls",
            wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let top_view = top.create_view(&wgpu::TextureViewDescriptor::default());
        let target = make_texture(
            "Presentation target",
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let background_view = background.create_view(&wgpu::TextureViewDescriptor::default());
        let foreground_view = foreground.create_view(&wgpu::TextureViewDescriptor::default());
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

        let mut renderer = crate::render::renderer(&device.device)?;
        let mut plate_mask =
            crate::render::PlateMask::new(&device.device, &mut renderer, width, height);
        ui.set_plate_mask(Some(plate_mask.image()));

        if feed_audio {
            ui.audio_event(AudioEvent::Config { samplerate: 48_000 });
        }
        let now = Instant::now();
        let mut scene = Scene::new();
        let mut top_scene = Scene::new();
        let mut top_visible = false;
        let mut last = None;
        for frame in 0..frames {
            let mut input = Vec::with_capacity(800);
            let start = frame * 800;
            for index in 0..800 {
                let sample = start + index;
                let time = sample as f32 / 48_000.0;
                let kick = if (0..10).contains(&(sample % 9_600)) {
                    0.8
                } else {
                    0.0
                };
                let tone = 0.2 * (std::f32::consts::TAU * 220.0 * time).sin()
                    + 0.1 * (std::f32::consts::TAU * 900.0 * time).sin();
                let level = match frame % 240 {
                    0..60 => 2.0,
                    60..120 => 0.04,
                    120..180 => 3.0,
                    _ => 1.0,
                };
                input
                    .push([level * (tone + kick * (std::f32::consts::TAU * 64.0 * time).sin()); 2]);
            }
            before_frame(frame, ui);
            if feed_audio {
                ui.consume_buf(&input, now + Duration::from_secs_f64(frame as f64 / 60.0));
            }
            scene.reset();
            let frame_time = FrameTime::new(
                now + Duration::from_secs_f64(frame as f64 / 60.0),
                now + Duration::from_secs_f64((frame + 1) as f64 / 60.0),
            );
            if let Some(config) = controls_config {
                top_visible =
                    ui.draw_controls(&mut top_scene, view, config, false, frame_time.end());
            } else {
                top_scene.reset();
                top_visible = ui.draw_analysis_controls(&mut top_scene);
            }
            last = Some(ui.draw(&mut scene, view, FrameRateLimit::Fps120, frame_time));
        }
        let frame = last.expect("the presentation produced a frame");
        let mut raster_assets = RasterAssetCache::new();
        raster_assets.prepare(&mut renderer, device, &frame.raster_assets)?;
        let params = RenderParams {
            base_color: Color::TRANSPARENT,
            width,
            height,
            antialiasing_method: crate::render::ANTIALIASING,
        };
        if let Frame::Rebuilt(background)
        | Frame::Retained(background)
        | Frame::Unchanged(background) = frame.background
        {
            renderer.render_to_texture(
                &device.device,
                &device.queue,
                background,
                &background_view,
                &params,
            )?;
        }
        plate_mask.render(
            &device.device,
            &device.queue,
            frame.plate_instances,
            frame.plate_ink,
        );
        renderer.mark_override_image_dirty(&plate_mask.image().image);
        renderer.render_to_texture(
            &device.device,
            &device.queue,
            &scene,
            &foreground_view,
            &params,
        )?;
        if let Some(poi_scene) = frame.poi_overlay {
            renderer.render_to_texture(
                &device.device,
                &device.queue,
                poi_scene,
                &poi_view,
                &params,
            )?;
        }
        if top_visible {
            renderer.render_to_texture(
                &device.device,
                &device.queue,
                &top_scene,
                &top_view,
                &params,
            )?;
        }
        let mut visualizer_renderer = crate::visualizer::Renderer::new(&device.device, format);
        let mut encoder = device
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        if let Frame::Solid(color) = frame.background {
            crate::render::clear_background(&mut encoder, &background_view, color);
        }
        visualizer_renderer.draw(
            device,
            &mut encoder,
            &background_view,
            &target_view,
            Size::new(f64::from(width), f64::from(height)),
            frame.overlay,
            Some(&foreground_view),
            frame.overlay_composite,
            frame.overlay_opacity,
            frame.poi_overlay.map(|_| &poi_view),
            top_visible.then_some(&top_view),
        );
        let row_bytes = width * 4;
        let padded_row_bytes = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = device.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Presentation readback"),
            size: u64::from(padded_row_bytes * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row_bytes),
                    rows_per_image: Some(height),
                },
            },
            target.size(),
        );
        device.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
        device.device.poll(wgpu::PollType::wait_indefinitely())?;
        rx.recv()??;
        let mapped = readback.slice(..).get_mapped_range()?;
        let pixels = mapped
            .chunks(padded_row_bytes as usize)
            .flat_map(|row| row[..row_bytes as usize].iter().copied())
            .collect();
        drop(mapped);
        readback.unmap();
        Ok(pixels)
    }

    #[test]
    fn presentation_draws_an_animated_foreground_without_the_hud() {
        let mut ui = Ui::new(&Config::default()).expect("the bundled fonts and models load");
        let view = View::fit(Size::new(1280.0, 720.0));
        let now = Instant::now();
        let frame_time = FrameTime::new(now, now + Duration::from_millis(16));
        let mut scene = Scene::new();

        let expected_composite = Some(Ui::model_composite(ui.theme.selected()));
        let first = ui.draw(&mut scene, &view, FrameRateLimit::Fps120, frame_time);
        assert!(matches!(first.background, Frame::Solid(_)));
        assert!(first.foreground);
        assert_eq!(first.overlay_composite, expected_composite);
        assert!((first.overlay_opacity - 1.0).abs() < 1e-6);

        let second = ui.draw(&mut scene, &view, FrameRateLimit::Fps120, frame_time);
        assert!(second.foreground);
        assert!(matches!(second.background, Frame::Solid(_)));
    }

    #[test]
    fn poi_callouts_have_a_separate_scene_from_main_infographics() -> anyhow::Result<()> {
        use crate::visualizer::{ModelRotation, Rng};

        let mut config = Config::default();
        config.theme_preferences.auto_colors = false;
        config.model_motion = ModelMotion::Still;
        config.model_rotation = ModelRotation::Locked;
        let mut ui = Ui::new(&config)?;
        ui.visualizer.load_models();
        for _ in 0..64 {
            if ui.visualizer.model_id() == Some("rif_ak47") {
                break;
            }
            ui.visualizer.next_model();
        }
        assert_eq!(ui.visualizer.model_id(), Some("rif_ak47"));
        let view = View::fit(Size::new(1920.0, 1080.0));
        let now = Instant::now();
        let mut scene = Scene::new();
        let first = ui.draw(
            &mut scene,
            &view,
            FrameRateLimit::Fps120,
            FrameTime::new(now, now + Duration::from_secs_f64(1.0 / 60.0)),
        );
        assert!(first.poi_overlay.is_none());
        assert!(!scene.encoding().draw_data.is_empty());

        assert!(ui.visualizer.showcase.enter_focus(&mut Rng::new(7), 1.1));
        for frame_index in 1..121 {
            scene.reset();
            let start = now + Duration::from_secs_f64(frame_index as f64 / 60.0);
            let frame = ui.draw(
                &mut scene,
                &view,
                FrameRateLimit::Fps120,
                FrameTime::new(start, start + Duration::from_secs_f64(1.0 / 60.0)),
            );
            if frame_index == 120 {
                assert!(
                    frame
                        .poi_overlay
                        .is_some_and(|poi| !poi.encoding().draw_data.is_empty()),
                    "POI cards are submitted in their own scene above the model"
                );
                assert!(
                    !scene.encoding().draw_data.is_empty(),
                    "main infographics remain in the scene behind the model"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn auto_colors_skip_cooldown_and_preserve_manual_preferences() -> anyhow::Result<()> {
        let config = Config::default();
        let mut ui = Ui::new(&config)?;
        ui.theme_preferences.auto_colors = true;
        ui.theme_preferences.contrast = crate::theme::ThemeContrast::Always;
        let now = Instant::now();
        let manual = ui.theme_preferences.clone();
        ui.controls.pointer(Some(Point::new(1.0, 1.0)), now);
        ui.visualizer.navigate_model(Adjustment::Increase);
        ui.auto_theme(now + Duration::from_secs(5));
        assert_eq!(ui.selected_theme(), config.theme);
        ui.auto_theme(now + Duration::from_secs(11));
        assert_eq!(
            ui.selected_theme(),
            config.theme,
            "suppressed changes are not queued"
        );
        ui.visualizer.navigate_model(Adjustment::Increase);
        ui.auto_theme(now + Duration::from_secs(11));
        assert_ne!(ui.selected_theme().tone(), config.theme.tone());
        assert_eq!(ui.theme_preferences, manual);
        ui.choose_tone(config.theme.tone());
        assert_eq!(
            ui.selected_theme(),
            manual.remembered(config.theme.tone()).unwrap()
        );
        Ok(())
    }

    #[test]
    fn automatic_change_inside_cooldown_is_skipped_after_a_delayed_redraw() -> anyhow::Result<()> {
        let config = Config::default();
        let mut ui = Ui::new(&config)?;
        ui.theme_preferences.auto_colors = true;
        let now = Instant::now();
        ui.controls.pointer(Some(Point::new(1.0, 1.0)), now);
        ui.visualizer.navigate_model(Adjustment::Increase);
        let mut scene = Scene::new();
        let view = View::fit(Size::new(1280.0, 720.0));
        ui.draw(
            &mut scene,
            &view,
            FrameRateLimit::Fps60,
            FrameTime::new(
                now + Duration::from_millis(9980),
                now + Duration::from_millis(9990),
            ),
        );
        assert_eq!(ui.selected_theme(), config.theme);
        ui.draw(
            &mut scene,
            &view,
            FrameRateLimit::Fps60,
            FrameTime::new(
                now + Duration::from_secs(30),
                now + Duration::from_millis(30016),
            ),
        );
        assert_eq!(ui.selected_theme(), config.theme);
        Ok(())
    }

    #[test]
    fn automatic_contrast_changes_keep_each_frame_color_composition_consistent(
    ) -> anyhow::Result<()> {
        let config = Config::default();
        let mut ui = Ui::new(&config)?;
        ui.theme_preferences.auto_colors = true;
        ui.theme_preferences.contrast = crate::theme::ThemeContrast::Always;
        let now = Instant::now();
        ui.controls
            .pointer(Some(Point::new(1.0, 1.0)), now - Duration::from_secs(11));
        ui.visualizer.navigate_model(Adjustment::Increase);
        let view = View::fit(Size::new(1280.0, 720.0));
        let mut scene = Scene::new();
        let original = Ui::model_composite(config.theme);
        let frame = ui.draw(
            &mut scene,
            &view,
            FrameRateLimit::Fps60,
            FrameTime::new(now, now + Duration::from_millis(16)),
        );
        assert_eq!(frame.overlay.model_composite, original);
        assert_eq!(frame.overlay_composite, Some(original));
        assert_ne!(ui.selected_theme().tone(), config.theme.tone());
        let next = Ui::model_composite(ui.selected_theme());
        let frame = ui.draw(
            &mut scene,
            &view,
            FrameRateLimit::Fps60,
            FrameTime::new(
                now + Duration::from_millis(16),
                now + Duration::from_millis(32),
            ),
        );
        assert_eq!(frame.overlay.model_composite, next);
        assert_eq!(frame.overlay_composite, Some(next));
        Ok(())
    }

    #[test]
    fn startup_and_manual_choices_never_select_disliked_themes() -> anyhow::Result<()> {
        let mut config = Config::default();
        config.theme_preferences.dislike(config.theme);
        let mut ui = Ui::new(&config)?;
        assert_ne!(ui.selected_theme(), config.theme);
        ui.navigate_theme(Adjustment::Decrease);
        assert!(!ui.theme_preferences.is_disliked(ui.selected_theme()));
        let previous = ui.selected_theme();
        ui.dislike_theme();
        assert!(ui.theme_preferences.is_disliked(previous));
        assert_ne!(ui.selected_theme(), previous);
        Ok(())
    }

    #[test]
    fn presentation_history_exposes_the_boosted_rms_used_by_the_gap_gate() -> anyhow::Result<()> {
        let mut config = Config::default();
        config.preamp_db = PreampDb::try_from(6.0)?;
        let mut ui = Ui::new(&config)?;
        ui.audio_event(AudioEvent::Config { samplerate: 48_000 });
        let now = Instant::now();
        let input: Vec<_> = (0..48_000)
            .map(|index| {
                let value =
                    0.02 * (std::f32::consts::TAU * 1_000.0 * index as f32 / 48_000.0).sin();
                [value; 2]
            })
            .collect();
        ui.consume_buf(&input, now);
        let mut scene = Scene::new();
        let view = View::fit(Size::new(1920.0, 1080.0));
        ui.draw(
            &mut scene,
            &view,
            FrameRateLimit::Fps60,
            FrameTime::new(now, now + Duration::from_secs(1)),
        );
        let sample = ui
            .visualizer
            .debug_history()
            .latest()
            .expect("presentation records analysis history");
        let expected = 20.0 * (0.02 * config.preamp_db.gain() / std::f32::consts::SQRT_2).log10();
        assert!((sample.drop_status.rms_db - expected).abs() < 0.1);
        assert!(sample.drop_status.envelope_db < config.detectors.drop_gap_threshold);
        assert!(!ui.analyzers_visible());
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn renders_analyzer_boost_gap_graph() -> anyhow::Result<()> {
        let directory = std::env::var_os("BENGA_GAP_PREVIEW_DIR").map(std::path::PathBuf::from);
        if let Some(directory) = &directory {
            std::fs::create_dir_all(directory)?;
        }
        for (width, height, light) in [(1920, 1080, false), (1920, 1080, true), (800, 600, false)] {
            let mut config = Config::default();
            if light {
                config.theme = ThemeId::all()
                    .find(|theme| theme.is_light())
                    .expect("light themes exist");
            }
            let mut ui = Ui::new(&config)?;
            let view = View::fit(Size::new(f64::from(width), f64::from(height)));
            let mut top = Scene::new();
            ui.draw_controls(&mut top, &view, &config, false, Instant::now());
            let boost = ui
                .controls
                .target_center(|action| matches!(action, ControlAction::Boost(_)));
            ui.set_pointer(Some(boost));
            let pixels = presentation_pixels(&mut ui, &view, 240, true, Some(&config), |_, _| {})?;
            if let Some(directory) = &directory {
                write_preview(
                    &directory.join(format!("gap-{width}x{height}-{light}.png")),
                    width,
                    height,
                    &pixels,
                )?;
            }
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn renders_developer_renderer_controls() -> anyhow::Result<()> {
        let directory = std::env::var_os("BENGA_DEVELOPER_PREVIEW_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "target/developer-controls-preview".into());
        std::fs::create_dir_all(&directory)?;
        for (width, height, scale) in [
            (1920, 1080, 1.0),
            (800, 600, 1.0),
            (1280, 720, 2.0),
            (360, 640, 1.0),
        ] {
            for visible in [false, true] {
                let config = Config::default();
                let mut ui = Ui::new(&config)?;
                ui.dismiss_startup();
                ui.set_control_modifiers(true);
                ui.set_renderer_stats_visible(visible);
                let now = Instant::now();
                for index in 0..180 {
                    ui.record_frame_performance(
                        now - Duration::from_secs(3) + Duration::from_secs_f64(index as f64 / 60.0),
                        Duration::from_secs_f64(1.0 / 60.0),
                        Duration::from_millis(4),
                        2.0,
                        Some([width / 2, height / 2]),
                    );
                }
                let mut view = View::fit(Size::new(f64::from(width), f64::from(height)));
                view.set_ui_scale(scale);
                let pixels =
                    presentation_pixels(&mut ui, &view, 30, false, Some(&config), |_, _| {})?;
                assert_eq!(pixels.len(), width as usize * height as usize * 4);
                let button = ui
                    .controls
                    .target_center(|action| matches!(action, ControlAction::ToggleRendererStats));
                assert!(matches!(
                    ui.click_controls(&view, button),
                    Some(ControlAction::ToggleRendererStats)
                ));
                write_preview(
                    &directory.join(format!("developer-{width}x{height}-{scale}-{visible}.png")),
                    width,
                    height,
                    &pixels,
                )?;
            }
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn renders_presentation_controls() -> anyhow::Result<()> {
        let directory =
            std::env::var_os("BENGA_CONTROLS_PREVIEW_DIR").map(std::path::PathBuf::from);
        if let Some(directory) = &directory {
            std::fs::create_dir_all(directory)?;
        }
        for (width, height, scale, light, manager) in [
            (1920, 1080, 1.0, false, false),
            (800, 600, 1.0, false, false),
            (1280, 720, 2.0, false, false),
            (1920, 1080, 1.0, true, false),
            (1920, 1080, 1.0, false, true),
            (800, 600, 1.0, false, true),
        ] {
            let mut config = Config::default();
            config.theme_preferences.auto_colors = true;
            if light {
                config.theme = ThemeId::all()
                    .find(|theme| theme.is_light())
                    .expect("light themes exist");
            }
            if manager {
                for theme in ThemeId::all()
                    .filter(|theme| *theme != config.theme)
                    .take(12)
                {
                    config.theme_preferences.dislike(theme);
                }
                config.presentation_opacity = PresentationOpacity::try_from(0.0)?;
            }
            let mut ui = Ui::new(&config)?;
            let mut view = View::fit(Size::new(f64::from(width), f64::from(height)));
            view.set_ui_scale(scale);
            if manager {
                let mut controls_scene = Scene::new();
                ui.draw_controls(&mut controls_scene, &view, &config, false, Instant::now());
                let dislike = ui
                    .controls
                    .target_center(|action| matches!(action, ControlAction::Dislike));
                ui.set_pointer(Some(dislike));
                ui.draw_controls(&mut controls_scene, &view, &config, false, Instant::now());
                let manage = ui
                    .controls
                    .target_center(|action| matches!(action, ControlAction::Manage));
                ui.set_pointer(Some(manage));
                ui.click_controls(&view, manage);
                ui.set_control_modifiers(true);
            }
            let pixels =
                presentation_pixels(&mut ui, &view, 90, false, Some(&config), |frame, ui| {
                    if !manager && frame == 1 {
                        ui.visualizer
                            .set_parameter(Parameter::SelectorEntryKicks, 1.0);
                        ui.visualizer.handle_action(
                            crate::visualizer::Action::Kick(0.6),
                            0.0,
                            0.75,
                        );
                    }
                })?;
            if let Some(directory) = &directory {
                write_preview(
                    &directory.join(format!(
                        "controls-{width}x{height}-{scale}-{light}-{manager}.png"
                    )),
                    width,
                    height,
                    &pixels,
                )?;
            }
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn renders_audio_analysis_presentation() -> anyhow::Result<()> {
        use crate::visualizer::ArrangementSetting;

        let Some(directory) = std::env::var_os("BENGA_ANALYSIS_PREVIEW_DIR") else {
            return Ok(());
        };
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory)?;
        let selected_case = std::env::var("BENGA_ANALYSIS_PREVIEW_CASE").ok();
        for (name, width, height, selector, controls) in [
            ("normal", 1920, 1080, false, false),
            ("selector", 1920, 1080, true, false),
            ("controls", 1280, 720, false, true),
            ("tall", 1024, 768, false, false),
            ("light", 1920, 1080, false, false),
            ("light-hover", 1920, 1080, false, false),
            ("bins", 1920, 1080, false, false),
            ("advanced", 1920, 1080, false, false),
            ("quality", 1920, 1080, false, false),
            ("analysis", 1920, 1080, false, false),
            ("history", 1920, 1080, false, false),
            ("heatmap", 1920, 1080, false, false),
            ("cloud-threshold", 1920, 1080, false, false),
            ("cloud-threshold-light", 1920, 1080, false, false),
            ("expired", 1920, 1080, false, false),
            ("disabled", 1920, 1080, false, false),
            ("checkbox", 1920, 1080, false, false),
            ("stereo", 1920, 1080, false, false),
            ("energy-loss", 1920, 1080, false, false),
            ("energy-loss-light", 1280, 720, false, false),
            ("gap-timing", 1920, 1080, false, false),
            ("gap-timing-light", 1280, 720, false, false),
            ("lf-envelopes", 1920, 1080, false, false),
            ("lf-envelopes-light", 1280, 720, false, false),
            ("rms-gap", 1920, 1080, false, false),
            ("rms-gap-light", 1280, 720, false, false),
            ("loss-gap", 1920, 1080, false, false),
            ("loss-gap-light", 1280, 720, false, false),
        ] {
            if selected_case
                .as_deref()
                .is_some_and(|selected| selected != name)
            {
                continue;
            }
            let mut config = Config::default();
            if name.starts_with("cloud-threshold") {
                for channel in &mut config.detectors.percussion.channels {
                    channel.cloud_enabled = true;
                    channel.cloud_sharpness_enabled = true;
                    channel.cloud_top_percent = 25.0;
                    channel.cloud_sharpness_top_percent = 40.0;
                    channel.cloud_min_count = 2;
                    channel.filters.attack_level = false;
                    channel.filters.min_sharpness = false;
                    channel.filters.energy_percentile = false;
                    channel.filters.sharpness_percentile = false;
                }
            }
            if name.starts_with("energy-loss") {
                for setting in [
                    ArrangementSetting::MixLossDb,
                    ArrangementSetting::ShapeDistance,
                    ArrangementSetting::TrendChange,
                    ArrangementSetting::ChangedFraction,
                    ArrangementSetting::GapHold,
                ] {
                    config.detectors.arrangement.set(setting, setting.range().0);
                }
                config.detectors.arrangement.reference_time = 20.0;
            }
            if name.starts_with("gap-timing") {
                let arrangement = &mut config.detectors.arrangement;
                arrangement.mix_loss_db = 2.0;
                arrangement.shape_distance = 0.08;
                arrangement.trend_change = 0.03;
                arrangement.changed_fraction = 0.10;
                arrangement.gap_hold = 0.25;
                arrangement.reference_time = 5.0;
            }
            if name.starts_with("rms-gap") {
                // Uniform attenuation preserves shape. The independent RMS
                // timer must arm despite strict relative shape requirements.
                let arrangement = &mut config.detectors.arrangement;
                arrangement.shape_distance = 0.6;
                arrangement.trend_change = 0.3;
                arrangement.changed_fraction = 1.0;
                arrangement.gap_rms_db = -40.0;
                arrangement.gap_rms_hold = 0.5;
            }
            if name.starts_with("loss-gap") {
                let arrangement = &mut config.detectors.arrangement;
                arrangement.mix_loss_db = 2.0;
                arrangement.mix_loss_hold = 0.35;
                arrangement.gap_hold = 4.0;
                arrangement.shape_distance = 0.6;
                arrangement.trend_change = 0.3;
                arrangement.changed_fraction = 1.0;
                arrangement.gap_rms_db = -80.0;
            }
            let mut ui = Ui::new(&config)?;
            ui.set_analyzers_visible(true);
            if [
                "light",
                "light-hover",
                "cloud-threshold-light",
                "energy-loss-light",
                "gap-timing-light",
                "lf-envelopes-light",
                "rms-gap-light",
                "loss-gap-light",
            ]
            .contains(&name)
            {
                ui.set_theme(ron::from_str("\"Tomorrow\"")?);
            }
            ui.visualizer
                .set_parameter(Parameter::SelectorEntryKicks, 32.0);
            ui.visualizer.set_parameter(Parameter::SnareFlipEvery, 32.0);
            let view = View::fit(Size::new(f64::from(width), f64::from(height)));
            let now = Instant::now();
            let mut noise_state = 0x7136_8ab9_u32;
            let mut timely_lf_return = false;
            let pixels = presentation_pixels(
                &mut ui,
                &view,
                if name == "bins" { 588 } else { 600 },
                false,
                controls.then_some(&config),
                |frame, ui| {
                    let mut input = Vec::with_capacity(800);
                    for index in 0..800 {
                        let time = (frame * 800 + index) as f32 / 48_000.0;
                        noise_state ^= noise_state << 13;
                        noise_state ^= noise_state >> 17;
                        noise_state ^= noise_state << 5;
                        let noise = noise_state as f32 / u32::MAX as f32 * 2.0 - 1.0;
                        let kick_age = time % 0.5;
                        let kick_phase = std::f32::consts::TAU
                            * (48.0 * kick_age + 100.0 * 0.025 * (1.0 - (-kick_age / 0.025).exp()));
                        let kick = 0.8 * (-kick_age / 0.06).exp() * kick_phase.sin();
                        let snare_age = (time + 0.25) % 1.0;
                        let snare = 0.35
                            * (-snare_age / 0.04).exp()
                            * (noise + (std::f32::consts::TAU * 190.0 * time).sin() * 0.5);
                        let hat_age = time % 0.125;
                        let hat = 0.15 * (-hat_age / 0.012).exp() * noise;
                        let quiet = (3.0..5.0).contains(&time);
                        let backing = 0.05 * (std::f32::consts::TAU * 700.0 * time).sin()
                            + 0.03 * (std::f32::consts::TAU * 1400.0 * time).sin();
                        let sample = if quiet {
                            backing * 0.3
                        } else {
                            backing + kick + snare + hat
                        };
                        if name.starts_with("loss-gap") {
                            // Uniform attenuation leaves the spectral shape
                            // unchanged and stays above the absolute RMS path.
                            let gain = if (4.5..7.0).contains(&time) {
                                0.5
                            } else if time >= 7.0 {
                                0.5 + 0.5 * ((time - 7.0) / 0.12).clamp(0.0, 1.0)
                            } else {
                                1.0
                            };
                            let signal = gain
                                * (0.24 * (std::f32::consts::TAU * 90.0 * time).sin()
                                    + 0.156 * (std::f32::consts::TAU * 150.0 * time).sin()
                                    + 0.05 * (std::f32::consts::TAU * 2200.0 * time).sin());
                            input.push([signal, -signal]);
                        } else if name.starts_with("rms-gap") {
                            let gain = if (3.0..6.0).contains(&time) {
                                0.02
                            } else if time >= 6.0 {
                                0.02 + 0.98 * ((time - 6.0) / 0.12).clamp(0.0, 1.0)
                            } else {
                                1.0
                            };
                            let signal = gain
                                * (0.24 * (std::f32::consts::TAU * 90.0 * time).sin()
                                    + 0.156 * (std::f32::consts::TAU * 150.0 * time).sin()
                                    + 0.05 * (std::f32::consts::TAU * 2200.0 * time).sin());
                            input.push([signal, -signal]);
                        } else if name.starts_with("lf-envelopes") {
                            // A broad 120 ms bass return must confirm without
                            // relying on one-hop spectral flux or coverage.
                            let bass_gain = if (3.0..6.0).contains(&time) {
                                0.002
                            } else if time >= 6.0 {
                                let ramp = ((time - 6.0) / 0.12).clamp(0.0, 1.0);
                                0.002 + (0.24 - 0.002) * ramp
                            } else {
                                0.24
                            };
                            let bass = bass_gain
                                * ((std::f32::consts::TAU * 90.0 * time).sin()
                                    + 0.65 * (std::f32::consts::TAU * 150.0 * time).sin());
                            let signal = bass
                                + 0.05 * (std::f32::consts::TAU * 2200.0 * time).sin()
                                + noise * 0.01;
                            input.push([signal, -signal]);
                        } else if name.starts_with("gap-timing") {
                            // First separate the spectral and energy evidence;
                            // only the final section changes them together.
                            let changed_shape = (3.0..3.8).contains(&time) || time >= 8.0;
                            let gain = if time >= 8.0 {
                                0.35
                            } else if (6.0..6.8).contains(&time) {
                                0.5
                            } else {
                                1.0
                            };
                            let (low, high) = if changed_shape {
                                (0.005, 0.20)
                            } else {
                                (0.20, 0.005)
                            };
                            let tone = gain
                                * (low * (std::f32::consts::TAU * 90.0 * time).sin()
                                    + 0.03 * (std::f32::consts::TAU * 880.0 * time).sin()
                                    + high * (std::f32::consts::TAU * 6000.0 * time).sin());
                            input.push([tone; 2]);
                        } else if name.starts_with("energy-loss") {
                            // A small uniform level dip exercises the 0.25 dB
                            // energy check without confirming a spectral gap.
                            let amplitude = if time < 7.0 { 0.25 } else { 0.24 };
                            let tone = amplitude * (std::f32::consts::TAU * 220.0 * time).sin();
                            input.push([tone; 2]);
                        } else if name == "stereo" {
                            // Independent in-band carriers exchange energy.
                            // The physical combined level stays steady while
                            // left and right attack envelopes move oppositely.
                            let within = (time - 0.25).rem_euclid(0.5);
                            let proportion = if within < 0.12 {
                                (within / 0.006).min(1.0)
                            } else {
                                (1.0 - (within - 0.12) / 0.080).max(0.0)
                            };
                            let pan = proportion * std::f32::consts::FRAC_PI_2;
                            input.push([
                                0.6 * pan.cos() * (std::f32::consts::TAU * 2500.0 * time).sin(),
                                0.6 * pan.sin() * (std::f32::consts::TAU * 3300.0 * time).sin(),
                            ]);
                        } else {
                            input.push([sample * 0.5; 2]);
                        }
                    }
                    if (!selector || frame < 540) && (name != "expired" || frame < 540) {
                        ui.consume_buf(&input, now + Duration::from_secs_f64(frame as f64 / 60.0));
                    }
                    if name.starts_with("lf-envelopes") {
                        let snapshot = ui.visualizer.debug_snapshot();
                        if frame == 330 {
                            assert!(
                                snapshot.drop_onset.armed,
                                "bassless bridge must arm the gap"
                            );
                        }
                        if (360..372).contains(&frame) {
                            let status = snapshot.drop_status;
                            timely_lf_return |= snapshot.drop_hits == 1
                                && status.bass_fast_db > status.bass_slow_db
                                && status.bass_fast_db >= status.bass_onset_threshold_db;
                        }
                    }
                    if name.starts_with("rms-gap") {
                        let snapshot = ui.visualizer.debug_snapshot();
                        let status = snapshot.drop_status;
                        if frame == 195 {
                            assert!(
                                !snapshot.drop_onset.armed,
                                "RMS must stay low for its full hold"
                            );
                            assert!(status.gap_rms_condition_met);
                            assert!(status.gap_rms_held_seconds < status.gap_rms_hold_seconds);
                        }
                        if frame == 218 {
                            assert!(snapshot.drop_onset.armed, "RMS alone must arm the gap");
                            assert!(
                                !status.gap_conditions_met,
                                "relative shape path must remain false"
                            );
                            assert_eq!(status.gap_rms_hold_seconds, 0.5);
                            assert_eq!(status.gap_rms_threshold_db, -40.0);
                        }
                        if (360..372).contains(&frame) {
                            timely_lf_return |= snapshot.drop_hits == 1
                                && status.bass_fast_db > status.bass_slow_db;
                        }
                    }
                    if name.starts_with("loss-gap") {
                        let snapshot = ui.visualizer.debug_snapshot();
                        let status = snapshot.drop_status;
                        if frame == 285 {
                            assert!(
                                !snapshot.drop_onset.armed,
                                "loss must complete its own hold"
                            );
                            assert!(status.mix_loss_condition_met);
                            assert!(status.mix_loss_held_seconds < status.mix_loss_hold_seconds);
                        }
                        if frame == 310 {
                            assert!(snapshot.drop_onset.armed, "loss alone must arm the gap");
                            assert!(!status.gap_conditions_met, "shape path must remain false");
                            assert!(
                                !status.gap_rms_condition_met,
                                "absolute RMS path must remain false"
                            );
                            assert_eq!(status.mix_loss_hold_seconds, 0.35);
                        }
                        if (420..432).contains(&frame) {
                            timely_lf_return |= snapshot.drop_hits == 1
                                && status.bass_fast_db > status.bass_slow_db;
                        }
                    }
                    if name.starts_with("gap-timing") && [225, 390, 475].contains(&frame) {
                        let snapshot = ui.visualizer.debug_snapshot();
                        let status = snapshot.drop_status;
                        let spectral = status.shape_distance > status.shape_distance_threshold
                            && (status.trend_change > status.trend_change_threshold
                                || status.changed_fraction >= status.changed_fraction_threshold);
                        let energy = -status.energy_delta_db > status.mix_loss_threshold_db;
                        assert!(
                            !snapshot.drop_onset.armed,
                            "separated evidence must stay unarmed at frame {frame}"
                        );
                        assert!(
                            !status.gap_conditions_met,
                            "no overlap at frame {frame}: {status:?}"
                        );
                        if frame == 225 {
                            assert!(spectral && !energy, "early shape-only phase: {status:?}");
                        } else if frame == 390 {
                            assert!(energy && !spectral, "later energy-only phase: {status:?}");
                        }
                    }
                    if name == "disabled" && frame == 1 {
                        for setting in PercussionSetting::CHANNEL
                            .into_iter()
                            .filter(|setting| setting.is_filter_toggle())
                        {
                            ui.set_parameter(Parameter::Percussion(Detector::Kick, setting), 0.0);
                        }
                    }
                    if name == "checkbox" && frame == 575 {
                        let point = ui
                            .presentation
                            .analysis_slider_center(Parameter::Percussion(
                                Detector::Kick,
                                PercussionSetting::InputProminenceEnabled,
                            ))
                            .unwrap();
                        ui.click_controls_at(&view, point, now);
                        ui.set_pointer(Some(point));
                    }
                    if (["heatmap", "expired"].contains(&name)
                        || name.starts_with("cloud-threshold"))
                        && frame == 1
                    {
                        for _ in 0..2 {
                            let graph = ui.presentation.analysis_graph_bounds().unwrap().center();
                            ui.click_controls_at(&view, graph, now);
                        }
                        if name.starts_with("cloud-threshold") {
                            ui.set_pointer(Some(Point::ORIGIN));
                            ui.set_pointer(None);
                        }
                    }
                    if name == "expired" && frame == 540 {
                        ui.visualizer.consume_buf(&vec![[0.0; 2]; 48_000 * 20], 1.0);
                    }
                    if name == "bins" && frame == 1 {
                        let graph = ui.presentation.analysis_graph_bounds().unwrap().center();
                        ui.click_controls_at(&view, graph, now);
                    }
                    if name == "stereo" && frame == 1 {
                        let graph = ui.presentation.analysis_graph_bounds().unwrap().center();
                        ui.click_controls_at(&view, graph, now);
                    }
                    if name == "bins" && frame == 575 {
                        let parameter = ui.presentation.analysis_first_bin_parameter().unwrap();
                        ui.set_parameter(parameter, 0.3);
                    }
                    if ["advanced", "quality", "analysis", "light-hover"].contains(&name)
                        && frame == 575
                    {
                        let setting = match name {
                            "quality" => PercussionSetting::AdmissionProminence,
                            "analysis" => PercussionSetting::SharpnessPercentile,
                            _ => PercussionSetting::FeatureMs,
                        };
                        if let Some(center) = ui
                            .presentation
                            .analysis_slider_center(Parameter::Percussion(Detector::Kick, setting))
                        {
                            ui.set_pointer(Some(center));
                        }
                    }
                    if name == "history" && frame == 590 {
                        let previous = ui.presentation.analysis_previous_event_center().unwrap();
                        ui.click_controls_at(&view, previous, now);
                    }
                    if name == "stereo" && frame == 599 {
                        let previous = ui.presentation.analysis_previous_event_center().unwrap();
                        ui.click_controls_at(&view, previous, now);
                        ui.set_pointer(None);
                    }
                    if selector && frame == 550 {
                        ui.visualizer
                            .set_parameter(Parameter::SelectorEntryKicks, 1.0);
                        ui.visualizer.showcase.exit_focus();
                        ui.visualizer.showcase.kick(&mut ui.visualizer.rng, true);
                    }
                    if controls && frame == 599 {
                        ui.controls.activate(now + Duration::from_secs(10));
                    }
                },
            )?;
            if width == 1920 {
                assert_eq!(
                    ui.presentation.debug_analysis_count(),
                    2,
                    "both diagnostic instruments remain visible in {name}"
                );
            }
            if name.starts_with("cloud-threshold") {
                let reference = ui.visualizer.percussion_reference(2);
                assert!(reference.thresholds.cloud_active);
                assert!(reference.thresholds.sharpness_cloud_active);
            }
            if name.starts_with("energy-loss") {
                let status = ui.visualizer.debug_snapshot();
                assert_eq!(status.drop_status.mix_loss_threshold_db, 0.25);
                assert!(-status.drop_status.energy_delta_db > 0.25);
                assert!(
                    status.drop_onset.armed,
                    "sustained energy loss must arm without a shape change"
                );
                assert!(!status.drop_status.gap_conditions_met);
                assert!(status.drop_status.mix_loss_condition_met);
            }
            if name.starts_with("gap-timing") {
                let snapshot = ui.visualizer.debug_snapshot();
                assert!(
                    snapshot.drop_onset.armed,
                    "overlapping evidence must confirm a gap: {:?}",
                    snapshot.drop_status
                );
                assert!(snapshot.drop_status.gap_conditions_met);
                assert_eq!(snapshot.drop_status.gap_hold_seconds, 0.25);
            }
            if name.starts_with("lf-envelopes") {
                let snapshot = ui.visualizer.debug_snapshot();
                assert_eq!(snapshot.drop_hits, 1, "one broad bass return confirms once");
                assert!(
                    timely_lf_return,
                    "bass return must confirm within 200 ms with envelope evidence"
                );
                assert!(
                    ui.visualizer.debug_history().samples().any(|sample| {
                        sample.drop_status.bass_fast_db > sample.drop_status.bass_slow_db + 1.0
                    }),
                    "history must retain the fast/slow separation used by the LF graph"
                );
            }
            if name.starts_with("rms-gap") {
                let snapshot = ui.visualizer.debug_snapshot();
                assert_eq!(snapshot.drop_hits, 1, "the RMS-armed gap is consumed once");
                assert!(
                    timely_lf_return,
                    "fresh LF return must consume the RMS-armed gap"
                );
                assert!(!snapshot.drop_onset.armed);
                assert!(
                    ui.visualizer.debug_history().samples().any(|sample| {
                        sample.drop_onset.armed && !sample.drop_status.gap_conditions_met
                    }),
                    "history must show RMS-only arming"
                );
            }
            if name.starts_with("loss-gap") {
                let snapshot = ui.visualizer.debug_snapshot();
                assert_eq!(snapshot.drop_hits, 1, "the loss-armed gap is consumed once");
                assert!(
                    timely_lf_return,
                    "fresh LF return must consume the loss-armed gap"
                );
                assert!(!snapshot.drop_onset.armed);
                assert!(
                    ui.visualizer.debug_history().samples().any(|sample| {
                        let status = sample.drop_status;
                        sample.drop_onset.armed
                            && status.mix_loss_held_seconds >= status.mix_loss_hold_seconds
                            && !status.gap_conditions_met
                            && !status.gap_rms_condition_met
                    }),
                    "history must retain independently sufficient loss evidence"
                );
            }
            write_preview(
                &directory.join(format!("{name}.png")),
                width,
                height,
                &pixels,
            )?;
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn renders_presentation_at_sixteen_by_nine() -> anyhow::Result<()> {
        let width = 1920;
        let height = 1080;
        let mut ui = Ui::new(&Config::default())?;
        let view = View::fit(Size::new(f64::from(width), f64::from(height)));
        let now = Instant::now();
        let mut scene = Scene::new();
        let _ = ui.draw(
            &mut scene,
            &view,
            FrameRateLimit::Fps120,
            FrameTime::new(now, now + Duration::from_millis(16)),
        );
        for _ in 0..64 {
            if ui.visualizer.model_id() == Some("rif_ak47") {
                break;
            }
            ui.visualizer.next_model();
        }
        let pixels = presentation_pixels(&mut ui, &view, 150, false, None, |_, _| {})?;
        if let Some(path) = std::env::var_os("BENGA_PRESENTATION_PREVIEW_PATH") {
            write_preview(&std::path::PathBuf::from(path), width, height, &pixels)?;
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn renders_presentation_entrance() -> anyhow::Result<()> {
        let width = 1920;
        let height = 1080;
        let mut ui = Ui::new(&Config::default())?;
        let view = View::fit(Size::new(f64::from(width), f64::from(height)));
        let now = Instant::now();
        let mut scene = Scene::new();
        let _ = ui.draw(
            &mut scene,
            &view,
            FrameRateLimit::Fps120,
            FrameTime::new(now, now + Duration::from_millis(16)),
        );
        for _ in 0..64 {
            if ui.visualizer.model_id() == Some("rif_ak47") {
                break;
            }
            ui.visualizer.next_model();
        }
        let pixels = presentation_pixels(&mut ui, &view, 22, false, None, |_, _| {})?;
        if let Some(path) = std::env::var_os("BENGA_ENTRANCE_PREVIEW_PATH") {
            write_preview(&std::path::PathBuf::from(path), width, height, &pixels)?;
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn renders_presentation_low_opacity() -> anyhow::Result<()> {
        let width = 1920;
        let height = 1080;
        let mut config = Config::default();
        config.presentation_opacity = PresentationOpacity::try_from(0.5)?;
        let mut ui = Ui::new(&config)?;
        let view = View::fit(Size::new(f64::from(width), f64::from(height)));
        let now = Instant::now();
        let mut scene = Scene::new();
        let _ = ui.draw(
            &mut scene,
            &view,
            FrameRateLimit::Fps120,
            FrameTime::new(now, now + Duration::from_millis(16)),
        );
        for _ in 0..64 {
            if ui.visualizer.model_id() == Some("rif_ak47") {
                break;
            }
            ui.visualizer.next_model();
        }
        let pixels = presentation_pixels(&mut ui, &view, 80, false, None, |_, _| {})?;
        if let Some(path) = std::env::var_os("BENGA_OPACITY_PREVIEW_PATH") {
            write_preview(&std::path::PathBuf::from(path), width, height, &pixels)?;
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn renders_presentation_light_theme() -> anyhow::Result<()> {
        let width = 1920;
        let height = 1080;
        let mut ui = Ui::new(&Config::default())?;
        let theme: ThemeId = ron::from_str("\"Tomorrow\"")?;
        ui.set_theme(theme);
        let view = View::fit(Size::new(f64::from(width), f64::from(height)));
        let now = Instant::now();
        let mut scene = Scene::new();
        let _ = ui.draw(
            &mut scene,
            &view,
            FrameRateLimit::Fps120,
            FrameTime::new(now, now + Duration::from_millis(16)),
        );
        for _ in 0..64 {
            if ui.visualizer.model_id() == Some("rif_ak47") {
                break;
            }
            ui.visualizer.next_model();
        }
        let pixels = presentation_pixels(&mut ui, &view, 80, false, None, |_, _| {})?;
        if let Some(path) = std::env::var_os("BENGA_LIGHT_PREVIEW_PATH") {
            write_preview(&std::path::PathBuf::from(path), width, height, &pixels)?;
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn renders_presentation_aspects() -> anyhow::Result<()> {
        let Some(directory) = std::env::var_os("BENGA_ASPECT_PREVIEW_DIR") else {
            return Ok(());
        };
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory)?;
        let mut ui = Ui::new(&Config::default())?;
        let now = Instant::now();
        let mut scene = Scene::new();
        let load_view = View::fit(Size::new(1920.0, 1080.0));
        let _ = ui.draw(
            &mut scene,
            &load_view,
            FrameRateLimit::Fps120,
            FrameTime::new(now, now + Duration::from_millis(16)),
        );
        for _ in 0..64 {
            if ui.visualizer.model_id() == Some("rif_ak47") {
                break;
            }
            ui.visualizer.next_model();
        }
        for (width, height) in [(1280, 720), (1280, 440), (1024, 768)] {
            let view = View::fit(Size::new(f64::from(width), f64::from(height)));
            let pixels = presentation_pixels(&mut ui, &view, 70, false, None, |_, _| {})?;
            write_preview(
                &directory.join(format!("presentation-{width}x{height}.png")),
                width,
                height,
                &pixels,
            )?;
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn renders_selector_presentation() -> anyhow::Result<()> {
        use crate::visualizer::Action;

        let width = 1920;
        let height = 1080;
        let mut ui = Ui::new(&Config::default())?;
        let view = View::fit(Size::new(f64::from(width), f64::from(height)));
        let now = Instant::now();
        let mut scene = Scene::new();
        let _ = ui.draw(
            &mut scene,
            &view,
            FrameRateLimit::Fps120,
            FrameTime::new(now, now + Duration::from_millis(16)),
        );
        for _ in 0..64 {
            if ui.visualizer.model_id() == Some("rif_ak47") {
                break;
            }
            ui.visualizer.next_model();
        }
        let pixels = presentation_pixels(&mut ui, &view, 90, false, None, |frame, ui| {
            if frame == 0 {
                ui.visualizer
                    .set_parameter(Parameter::SelectorEntryKicks, 1.0);
                ui.visualizer.handle_action(Action::Kick(0.6), 0.0, 0.75);
            }
            if frame == 30 || frame == 55 {
                ui.visualizer.handle_action(Action::Hat, 0.0, 0.75);
            }
        })?;
        if let Some(path) = std::env::var_os("BENGA_SELECTOR_PREVIEW_PATH") {
            write_preview(&std::path::PathBuf::from(path), width, height, &pixels)?;
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn renders_selector_exit_presentation() -> anyhow::Result<()> {
        use crate::visualizer::Action;

        let width = 1920;
        let height = 1080;
        let mut ui = Ui::new(&Config::default())?;
        let view = View::fit(Size::new(f64::from(width), f64::from(height)));
        let now = Instant::now();
        let mut scene = Scene::new();
        let _ = ui.draw(
            &mut scene,
            &view,
            FrameRateLimit::Fps120,
            FrameTime::new(now, now + Duration::from_millis(16)),
        );
        for _ in 0..64 {
            if ui.visualizer.model_id() == Some("rif_ak47") {
                break;
            }
            ui.visualizer.next_model();
        }
        let pixels = presentation_pixels(&mut ui, &view, 38, false, None, |frame, ui| {
            if frame == 0 {
                ui.visualizer
                    .set_parameter(Parameter::SelectorEntryKicks, 1.0);
                ui.visualizer
                    .set_parameter(Parameter::SelectorExitTriggers, 1.0);
                ui.visualizer.handle_action(Action::Kick(0.6), 0.0, 0.75);
            }
            if frame == 30 {
                ui.visualizer.handle_action(Action::Kick(0.6), 0.0, 0.75);
            }
        })?;
        if let Some(path) = std::env::var_os("BENGA_SELECTOR_EXIT_PREVIEW_PATH") {
            write_preview(&std::path::PathBuf::from(path), width, height, &pixels)?;
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn renders_poi_presentation() -> anyhow::Result<()> {
        use crate::visualizer::{Action, Rng};

        let width = 1920;
        let height = 1080;
        let mut ui = Ui::new(&Config::default())?;
        let view = View::fit(Size::new(f64::from(width), f64::from(height)));
        let now = Instant::now();
        let mut scene = Scene::new();
        let _ = ui.draw(
            &mut scene,
            &view,
            FrameRateLimit::Fps120,
            FrameTime::new(now, now + Duration::from_millis(16)),
        );
        for _ in 0..64 {
            if ui.visualizer.model_id() == Some("rif_ak47") {
                break;
            }
            ui.visualizer.next_model();
        }
        ui.visualizer
            .set_parameter(Parameter::SnarePoiSwitches, 10.0);
        let mut rng = Rng::new(7);
        ui.visualizer.showcase.enter_focus(&mut rng, 1.1);
        let pixels = presentation_pixels(&mut ui, &view, 80, false, None, |frame, ui| {
            if frame == 35 {
                ui.visualizer.handle_action(Action::Snare, 0.0, 1.1);
            }
            if frame == 74 {
                ui.visualizer.handle_action(Action::Hat, 0.0, 1.1);
                ui.presentation.debug_hit_marker();
            }
        })?;
        if let Some(path) = std::env::var_os("BENGA_POI_PREVIEW_PATH") {
            write_preview(&std::path::PathBuf::from(path), width, height, &pixels)?;
        }
        Ok(())
    }

    #[test]
    fn presentation_overlay_tracks_theme_composition_and_opacity() {
        let mut ui = Ui::new(&Config::default()).expect("the bundled fonts and models load");
        let view = View::fit(Size::new(1280.0, 720.0));
        let now = Instant::now();
        let mut scene = Scene::new();
        let frame_time = FrameTime::new(now, now + Duration::from_millis(16));
        let dark = Ui::model_composite(ui.theme.selected());
        let _ = ui.draw(&mut scene, &view, FrameRateLimit::Fps120, frame_time);

        let light: ThemeId = ron::from_str("\"Tomorrow\"").expect("the bundled light theme loads");
        ui.set_theme(light);
        ui.set_presentation_opacity(
            PresentationOpacity::try_from(0.5).expect("the opacity is inside its bounds"),
        );
        let frame = ui.draw(&mut scene, &view, FrameRateLimit::Fps120, frame_time);
        assert_ne!(dark, Ui::model_composite(light));
        assert_eq!(frame.overlay_composite, Some(Ui::model_composite(light)));
        assert!((frame.overlay_opacity - 0.5).abs() < 1e-6);
    }
}
