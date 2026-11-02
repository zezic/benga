#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::{
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

use anyhow::Context;
use audio::capture_stream;
use config::{Config, CursorVisibility, PreampDb};
use frame_schedule::{
    presentation_interval, Adjustment, FrameKind, FrameSchedule, FrameTime, Occlusion,
    RedrawPermit, SurfaceArea,
};
use render::RenderWindow;
use saved::Saved;
use vello::{kurbo::Point, wgpu};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::{CursorIcon, Fullscreen, WindowId},
};

use benga::window_chrome::{Chrome, Hit, WindowButton};
use benga::{audio, config, frame_schedule, profile, render, visualizer, AudioEvent};
use visualizer::{ControlAction, Dial, Ui, View};
mod cli;
mod desktop;
mod frame_trace;
mod saved;
mod updater;

enum WindowState {
    Suspended,
    Active(Box<ActiveWindow>),
    Failed(anyhow::Error),
}

struct ActiveWindow {
    chrome: Chrome,
    render: RenderWindow,
    view: View,
    visualizer_renderer: visualizer::Renderer,
}

impl ActiveWindow {
    fn new(event_loop: &ActiveEventLoop) -> anyhow::Result<Self> {
        let preferences = wgpu::DeviceDescriptor {
            required_features: wgpu::Features::TIMESTAMP_QUERY,
            ..Default::default()
        };
        let render = RenderWindow::new(event_loop, preferences)?;
        let chrome = Chrome::new(&render.window)?;
        let mut view = View::fit(render.viewport());
        view.set_ui_scale(render.window.scale_factor());
        let visualizer_renderer =
            visualizer::Renderer::new(&render.device().device, render.format());
        Ok(Self {
            chrome,
            render,
            view,
            visualizer_renderer,
        })
    }
}

enum Setting {
    Theme,
    PresentationOpacity,
    FrameRate,
    Shutter,
    LineWidth,
    Zoom,
}

struct App {
    desktop: desktop::Desktop,
    exit_requested: bool,
    state: WindowState,
    rx: Receiver<AudioEvent>,
    ui: Ui,
    settings: Saved<Config>,
    mouse: Point,
    modifiers: ModifiersState,
    frames: FrameSchedule,
    previous_frame: Instant,
    previous_presentation: Option<Instant>,
    profile: Option<profile::Run>,
    session_ephemeral: bool,
    renderer_status: Option<String>,
    frame_trace: Option<frame_trace::FrameTrace>,
}

impl App {
    fn frame_rate_limit(&self) -> frame_schedule::FrameRateLimit {
        self.profile.as_ref().map_or(
            self.settings.frame_rate_limit,
            profile::Run::target_frame_rate,
        )
    }

    fn adjust_setting(&mut self, setting: Setting, adjustment: Adjustment) {
        let settings = self.settings.edit();
        match setting {
            Setting::Shutter => {
                settings.shutter_frames = settings.shutter_frames.adjusted(adjustment);
                self.ui.set_shutter(settings.shutter_frames);
            }
            Setting::Theme => self.ui.navigate_theme(adjustment),
            Setting::PresentationOpacity => {
                settings.presentation_opacity = settings.presentation_opacity.adjusted(adjustment);
                self.ui
                    .set_presentation_opacity(settings.presentation_opacity);
            }
            Setting::FrameRate => {
                settings.frame_rate_limit = settings.frame_rate_limit.adjusted(adjustment);
                self.frames.limit_changed(Instant::now());
                if let WindowState::Active(window) = &mut self.state {
                    window.visualizer_renderer.reset_frame_timing();
                }
            }
            Setting::Zoom => {
                settings.zoom = settings.zoom.adjusted(adjustment);
                let WindowState::Active(window) = &mut self.state else {
                    unreachable!("zoom requires an active window");
                };
                window.view.set_zoom(settings.zoom);
            }
            Setting::LineWidth => {
                settings.line_width = settings.line_width.adjusted(adjustment);
                self.ui.set_line_width(settings.line_width);
            }
        }
    }

    fn set_fullscreen(&mut self, fullscreen: bool) {
        self.ui.reset_presentation_clicks();
        let WindowState::Active(window) = &mut self.state else {
            unreachable!("fullscreen requires an active window");
        };
        if fullscreen {
            window.chrome.native_opacity(&window.render.window, 1.0);
        }
        window.chrome.set_pressed(None);
        window.chrome.reset_drag_clicks();
        window
            .render
            .window
            .set_fullscreen(fullscreen.then_some(Fullscreen::Borderless(None)));
        self.settings.edit().fullscreen = fullscreen;
    }

    fn apply_control(&mut self, action: ControlAction) {
        match action {
            ControlAction::Help => self.desktop.open = true,
            ControlAction::Fullscreen => {
                let WindowState::Active(window) = &self.state else {
                    unreachable!("a control requires an active window");
                };
                self.set_fullscreen(window.render.window.fullscreen().is_none());
            }
            ControlAction::Adjust(dial, adjustment) => {
                let setting = match dial {
                    Dial::Theme => Setting::Theme,
                    Dial::Model => {
                        self.ui.navigate_model(adjustment);
                        return;
                    }
                    Dial::LineWidth => Setting::LineWidth,
                    Dial::Zoom => Setting::Zoom,
                    Dial::FrameRate => Setting::FrameRate,
                    Dial::Shutter => Setting::Shutter,
                    Dial::TextOpacity => Setting::PresentationOpacity,
                };
                self.adjust_setting(setting, adjustment);
            }
            ControlAction::AutoColors => {
                let preferences = self.ui.theme_preferences_mut();
                preferences.auto_colors = !preferences.auto_colors;
            }
            ControlAction::Contrast(contrast) => {
                self.ui.theme_preferences_mut().contrast = contrast
            }
            ControlAction::Tone(tone) => self.ui.choose_tone(tone),
            ControlAction::ModelLock => {
                let rotation = self.settings.model_rotation.toggled();
                self.settings.edit().model_rotation = rotation;
                self.ui.set_model_rotation(rotation);
            }
            ControlAction::Boost(boost) => {
                self.settings.edit().preamp_db = boost;
                self.ui.set_preamp(boost);
            }
            ControlAction::MotionBlur => {
                let motion = self.settings.model_motion.toggled();
                self.settings.edit().model_motion = motion;
                self.ui.set_model_motion(motion);
            }
            ControlAction::HideCursor => {
                let visibility = match self.settings.cursor_visibility {
                    CursorVisibility::Always => CursorVisibility::AutoHide,
                    CursorVisibility::AutoHide => CursorVisibility::Always,
                };
                self.settings.edit().cursor_visibility = visibility;
            }
            ControlAction::Dislike => self.ui.dislike_theme(),
            ControlAction::SaveDefault => {
                let theme = self.ui.selected_theme();
                self.settings.edit().theme = theme;
                self.ui.choose_theme(theme);
            }
            ControlAction::Restore(theme) => {
                self.ui.theme_preferences_mut().restore(theme);
            }
            ControlAction::ToggleAnalyzers => self.ui.toggle_analyzers(),
            ControlAction::Manage => {}
        }
        self.sync_theme_preferences();
    }

    fn sync_theme_preferences(&mut self) {
        if &self.settings.theme_preferences != self.ui.theme_preferences() {
            self.settings.edit().theme_preferences = self.ui.theme_preferences().clone();
        }
    }

    fn window_event(&mut self, event: WindowEvent) -> anyhow::Result<()> {
        let limit = self.frame_rate_limit();
        let WindowState::Active(window) = &mut self.state else {
            return Ok(());
        };
        let ActiveWindow {
            chrome,
            render,
            view,
            visualizer_renderer,
        } = &mut **window;
        let renderer_key = matches!(
            &event,
            WindowEvent::KeyboardInput { event, .. }
                if event.physical_key == PhysicalKey::Code(KeyCode::KeyR)
        );
        if self.profile.is_some()
            && !renderer_key
            && !matches!(
                event,
                WindowEvent::Resized(_) | WindowEvent::Occluded(_) | WindowEvent::RedrawRequested
            )
        {
            return Ok(());
        }
        match event {
            WindowEvent::Resized(size) => {
                if let Some(profile) = &mut self.profile {
                    profile.pause_timing();
                }
                self.previous_presentation = None;
                self.ui.pause_frame_performance();
                visualizer_renderer.reset_frame_timing();
                let fullscreen = render.window.fullscreen().is_some();
                if self.settings.fullscreen != fullscreen {
                    self.settings.edit().fullscreen = fullscreen;
                }
                render.resize(size);
                if let Some(trace) = &mut self.frame_trace {
                    let extent = render.viewport();
                    trace.extent(
                        extent.width as u32,
                        extent.height as u32,
                        render.window.scale_factor(),
                    );
                }
                chrome.refresh_native_metrics(&render.window);
                *view = View::fit(render.viewport());
                view.set_zoom(self.settings.zoom);
                view.set_ui_scale(render.window.scale_factor());
                self.frames.resized(size.into(), Instant::now());
                self.ui.set_plate_mask(Some(render.plate_mask_image()));
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                view.set_ui_scale(scale_factor);
                if let Some(trace) = &mut self.frame_trace {
                    let extent = render.viewport();
                    trace.extent(extent.width as u32, extent.height as u32, scale_factor);
                }
                chrome.refresh_native_metrics(&render.window);
            }
            WindowEvent::Occluded(occluded) => {
                if let Some(profile) = &mut self.profile {
                    profile.pause_timing();
                }
                self.previous_presentation = None;
                self.ui.pause_frame_performance();
                visualizer_renderer.reset_frame_timing();
                let occlusion = if occluded {
                    Occlusion::Occluded
                } else {
                    Occlusion::Visible
                };
                self.frames.occlusion_changed(occlusion, Instant::now());
                self.previous_frame = Instant::now();
            }
            WindowEvent::MouseInput {
                button: MouseButton::Left,
                state,
                ..
            } => match state {
                ElementState::Pressed => {
                    let scale = render.window.scale_factor();
                    let fullscreen = render.window.fullscreen().is_some();
                    let hit = chrome.hit_test(
                        self.mouse,
                        render.viewport(),
                        scale,
                        fullscreen,
                        render.window.is_maximized(),
                    );
                    if let Some(hit) = hit {
                        self.ui.reset_presentation_clicks();
                        self.ui.reveal_controls();
                        match hit {
                            Hit::Button(button) => {
                                chrome.reset_drag_clicks();
                                chrome.set_pressed(Some(button));
                            }
                            Hit::Resize(direction) => {
                                chrome.reset_drag_clicks();
                                if let Err(error) = render.window.drag_resize_window(direction) {
                                    log::debug!("Window resize: {error}");
                                }
                            }
                            Hit::Drag => {
                                let point = Point::new(self.mouse.x / scale, self.mouse.y / scale);
                                if chrome.drag_double_click(point, Instant::now()) {
                                    render.window.set_maximized(!render.window.is_maximized());
                                } else if let Err(error) = render.window.drag_window() {
                                    log::debug!("Window drag: {error}");
                                }
                            }
                        }
                        return Ok(());
                    }
                    chrome.set_pressed(None);
                    chrome.reset_drag_clicks();
                    if chrome.is_hovered(render.viewport(), scale, fullscreen) {
                        self.ui.reset_presentation_clicks();
                        self.ui.reveal_controls();
                        return Ok(());
                    }
                    if self.ui.startup_visible() {
                        return Ok(());
                    }
                    if self.desktop.open || self.desktop.contains(self.mouse) {
                        self.exit_requested |= self.desktop.click(self.mouse, self.settings.edit());
                    } else if let Some(action) = self.ui.click_controls(view, self.mouse) {
                        self.apply_control(action);
                    }
                }
                ElementState::Released => {
                    if let Some(pressed) = chrome.take_pressed() {
                        let hit = chrome.hit_test(
                            self.mouse,
                            render.viewport(),
                            render.window.scale_factor(),
                            render.window.fullscreen().is_some(),
                            render.window.is_maximized(),
                        );
                        if hit == Some(Hit::Button(pressed)) {
                            match pressed {
                                WindowButton::Close => self.exit_requested = true,
                                WindowButton::Minimize => render.window.set_minimized(true),
                                WindowButton::Maximize => {
                                    render.window.set_maximized(!render.window.is_maximized())
                                }
                            }
                        }
                        return Ok(());
                    }
                    self.ui.end_slider_drag();
                }
            },
            WindowEvent::MouseInput {
                button: MouseButton::Right,
                state: ElementState::Pressed,
                ..
            } => {
                if chrome.is_hovered(
                    render.viewport(),
                    render.window.scale_factor(),
                    render.window.fullscreen().is_some(),
                ) {
                    render
                        .window
                        .show_window_menu(winit::dpi::PhysicalPosition::new(
                            self.mouse.x,
                            self.mouse.y,
                        ));
                    self.ui.reset_presentation_clicks();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let position = Point::new(position.x, position.y);
                self.ui.set_pointer(Some(position));
                self.desktop.set_pointer(Some(position));
                chrome.pointer(Some(position));
                let hit = chrome.hit_test(
                    position,
                    render.viewport(),
                    render.window.scale_factor(),
                    render.window.fullscreen().is_some(),
                    render.window.is_maximized(),
                );
                render
                    .window
                    .set_cursor(hit.map(Hit::cursor).unwrap_or(CursorIcon::Default));
                if chrome.is_hovered(
                    render.viewport(),
                    render.window.scale_factor(),
                    render.window.fullscreen().is_some(),
                ) {
                    self.ui.reveal_controls();
                    chrome.native_opacity(&render.window, 1.0);
                }
                render.window.set_cursor_visible(true);
                self.mouse = position;
            }
            WindowEvent::CursorLeft { .. } => {
                self.ui.set_pointer(None);
                self.desktop.set_pointer(None);
                chrome.pointer(None);
                chrome.set_pressed(None);
                chrome.reset_drag_clicks();
                render.window.set_cursor(CursorIcon::Default);
                render.window.set_cursor_visible(true);
                self.ui.end_slider_drag();
            }
            WindowEvent::Focused(false) => {
                self.modifiers = ModifiersState::empty();
                self.ui.set_control_modifiers(false);
                self.ui.set_pointer(None);
                self.desktop.set_pointer(None);
                chrome.pointer(None);
                chrome.set_pressed(None);
                chrome.reset_drag_clicks();
                render.window.set_cursor_visible(true);
                self.ui.end_slider_drag();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
                self.ui.set_control_modifiers(!self.modifiers.is_empty());
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.desktop.open || self.ui.startup_visible() {
                    return Ok(());
                }
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    MouseScrollDelta::PixelDelta(position) => position.y / 30.0,
                };
                self.ui.scroll_controls(lines);
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                if let PhysicalKey::Code(key) = event.physical_key {
                    if key == KeyCode::KeyR && !event.repeat {
                        self.ui.toggle_gpu_ribbons();
                        self.previous_presentation = None;
                        visualizer_renderer.reset_frame_timing();
                        return Ok(());
                    }
                    if self.ui.startup_visible() {
                        return Ok(());
                    }
                    if key == KeyCode::F1 && !event.repeat {
                        if self.desktop.open {
                            self.desktop.close(self.settings.edit());
                        } else {
                            self.desktop.open = true;
                        }
                    } else if key == KeyCode::Escape && !event.repeat {
                        if self.desktop.open {
                            self.desktop.close(self.settings.edit());
                        } else {
                            self.set_fullscreen(false);
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                let frame_started = Instant::now();
                let mut frame_capture = self
                    .frame_trace
                    .as_ref()
                    .map(|_| frame_trace::Capture::new(frame_started));
                while let Ok(event) = self.rx.try_recv() {
                    self.ui.audio_event(event);
                }
                let detectors = self.ui.detector_settings();
                if self.settings.detectors != detectors {
                    self.settings.edit().detectors = detectors;
                }
                let tuning = self.ui.visualizer_tuning();
                if self.settings.tuning != tuning {
                    self.settings.edit().tuning = tuning;
                }
                if &self.settings.theme_preferences != self.ui.theme_preferences() {
                    self.settings.edit().theme_preferences = self.ui.theme_preferences().clone();
                }
                let now = Instant::now();
                let fullscreen = render.window.fullscreen().is_some();
                if self.desktop.open
                    || chrome.is_hovered(
                        render.viewport(),
                        render.window.scale_factor(),
                        fullscreen,
                    )
                {
                    self.ui.reveal_controls();
                }
                let mut top_overlay = self.profile.is_none()
                    && self.ui.draw_controls(
                        &mut render.overlay_scene,
                        view,
                        &self.settings,
                        render.window.fullscreen().is_some(),
                        now,
                    );
                if self.profile.is_some() {
                    render.overlay_scene.reset();
                    if self
                        .profile
                        .as_ref()
                        .is_some_and(profile::Run::performance_overlay)
                    {
                        top_overlay =
                            self.ui
                                .draw_performance(&mut render.overlay_scene, view, now);
                    }
                }
                if self.profile.is_none() {
                    if self.ui.draw_startup(&mut render.overlay_scene, view, now) {
                        top_overlay = true;
                    } else {
                        let viewport = render.viewport();
                        let (colors, ink) = self.ui.scene_colors();
                        top_overlay |= self.desktop.draw(
                            &mut render.overlay_scene,
                            viewport,
                            render.window.scale_factor(),
                            &self.settings,
                            colors,
                            ink,
                        );
                    }
                }
                let (colors, ink) = self.ui.scene_colors();
                let chrome_opacity = if self.profile.is_some() {
                    0.0
                } else {
                    self.ui.controls_opacity()
                };
                chrome.native_opacity(&render.window, chrome_opacity);
                chrome.set_logo_opacity(self.ui.titlebar_logo_opacity(now));
                if self.profile.is_none() {
                    let viewport = render.viewport();
                    top_overlay |= chrome.draw(
                        &mut render.overlay_scene,
                        viewport,
                        render.window.scale_factor(),
                        colors,
                        ink,
                        chrome_opacity,
                        fullscreen,
                        render.window.is_maximized(),
                        render.window.has_focus(),
                    );
                }
                render.window.set_cursor_visible(
                    self.desktop.open || self.ui.cursor_visible(self.settings.cursor_visibility),
                );
                let frame_time = FrameTime::new(self.previous_frame, now);
                self.previous_frame = now;
                if let Some(profile) = &mut self.profile {
                    profile.feed(&mut self.ui, now)?;
                }
                let before_draw = Instant::now();
                render.scene.reset();
                let frame = self.ui.draw(&mut render.scene, view, limit, frame_time);
                let after_draw = Instant::now();
                if let Some(capture) = &mut frame_capture {
                    capture.ui_draw(before_draw, after_draw);
                }
                let changed =
                    matches!(frame.background, render::Frame::Rebuilt(_)) || frame.foreground;
                let viewport = render.viewport();
                let gpu_timing = (frame.overlay.gpu_active).then(|| {
                    visualizer_renderer
                        .gpu_timing_tag([viewport.width as u32, viewport.height as u32])
                });
                let content = render::FrameContent {
                    background: frame.background,
                    foreground: frame.foreground,
                    top_overlay,
                    raster_assets: &frame.raster_assets,
                    plates: frame.plate_instances,
                    plate_ink: frame.plate_ink,
                    gpu_timing,
                };
                let overlay =
                    |device: &vello::util::DeviceHandle,
                     encoder: &mut vello::wgpu::CommandEncoder,
                     background: &vello::wgpu::TextureView,
                     target: &vello::wgpu::TextureView,
                     foreground: Option<&vello::wgpu::TextureView>,
                     top: Option<&vello::wgpu::TextureView>,
                     frame_timestamps: Option<&wgpu::QuerySet>| {
                        visualizer_renderer.set_frame_end_timestamp(frame_timestamps);
                        visualizer_renderer.draw(
                            device,
                            encoder,
                            background,
                            target,
                            viewport,
                            frame.overlay,
                            foreground,
                            frame.overlay_composite,
                            frame.overlay_opacity,
                            top,
                        );
                    };
                if let Some(capture) = &mut frame_capture {
                    capture.begin_present();
                }
                let mut capture_stage = |stage: &render::Stage| {
                    if let Some(capture) = &mut frame_capture {
                        capture.stage(stage);
                    }
                };
                let mut acquisition_wait = Duration::ZERO;
                let presentation = if let Some(profile) = &mut self.profile {
                    profile.present(
                        frame_started,
                        before_draw,
                        after_draw,
                        changed,
                        |completed| {
                            let acquire_started = Instant::now();
                            render.present(
                                content,
                                |stage| {
                                    if matches!(stage, render::Stage::Acquire) {
                                        acquisition_wait = acquire_started.elapsed();
                                    }
                                    capture_stage(&stage);
                                    completed(stage);
                                },
                                overlay,
                            )
                        },
                    )?
                } else {
                    let acquire_started = Instant::now();
                    render.present(
                        content,
                        |stage| {
                            if matches!(stage, render::Stage::Acquire) {
                                acquisition_wait = acquire_started.elapsed();
                            }
                            capture_stage(&stage);
                        },
                        overlay,
                    )?
                };
                {
                    let rendered_downsize = visualizer_renderer.ribbon_downsize();
                    let rendered_extent = visualizer_renderer.ribbon_render_extent();
                    let completed = Instant::now();
                    if let (Some(trace), Some(capture)) = (&mut self.frame_trace, frame_capture) {
                        trace.record(
                            capture,
                            completed,
                            self.previous_presentation,
                            matches!(presentation, render::Presentation::Presented),
                            self.ui.gpu_ribbon_status(),
                            self.ui.scene_name(),
                        );
                    }
                    if matches!(presentation, render::Presentation::Presented) {
                        if let Some(previous) = self.previous_presentation {
                            let interval = completed.duration_since(previous);
                            let work = completed.duration_since(frame_started);
                            self.ui.record_frame_performance(
                                completed,
                                interval,
                                work,
                                rendered_extent.map_or(1.0, |_| rendered_downsize),
                                rendered_extent,
                            );
                            let monitor = render.window.current_monitor();
                            let reported_refresh_millihertz = monitor
                                .as_ref()
                                .and_then(|monitor| monitor.refresh_rate_millihertz())
                                .filter(|rate| *rate > 0);
                            let synchronized = matches!(
                                render.surface_configuration().present_mode,
                                wgpu::PresentMode::AutoVsync
                                    | wgpu::PresentMode::Fifo
                                    | wgpu::PresentMode::FifoRelaxed
                            );
                            let target = presentation_interval(
                                limit,
                                reported_refresh_millihertz,
                                synchronized,
                            )
                            .unwrap_or_else(|| Duration::from_secs_f64(1.0 / 60.0));
                            if let Some(profile) = &mut self.profile {
                                profile.record_gpu_timing_support(render.gpu_timing_supported());
                                profile.record_pacing_metadata(
                                    monitor,
                                    reported_refresh_millihertz,
                                    target,
                                    render.surface_configuration(),
                                    || render.supported_present_modes(),
                                );
                            }
                            if render.gpu_timing_supported() {
                                // Results arrive asynchronously. Until a fresh
                                // result is ready, keep the current quality;
                                // never substitute a surface/VSync wait here.
                                if let Some(timing) = render.take_gpu_frame_timing() {
                                    if let Some(profile) = &mut self.profile {
                                        profile.record_gpu_frame_timing(&timing);
                                    }
                                    visualizer_renderer.record_gpu_frame_timing(&timing, target);
                                }
                            } else {
                                visualizer_renderer.record_frame_timing_with_acquisition(
                                    interval,
                                    work,
                                    acquisition_wait,
                                    target,
                                );
                            }
                        }
                        self.previous_presentation = Some(completed);
                    } else {
                        self.previous_presentation = None;
                        self.ui.pause_frame_performance();
                        visualizer_renderer.reset_frame_timing();
                    }
                    let backend = self.ui.gpu_ribbon_status();
                    let status = if backend.starts_with("GPU") {
                        match rendered_extent {
                            Some([width, height]) => format!(
                                "{backend} / {:.2}x downsize ({width} × {height})",
                                rendered_downsize,
                            ),
                            None => backend.to_string(),
                        }
                    } else {
                        backend.to_string()
                    };
                    if self.renderer_status.as_deref() != Some(status.as_str()) {
                        render
                            .window
                            .set_title(&format!("Benga — {status} — R: CPU/GPU"));
                        if self.session_ephemeral {
                            eprintln!("Renderer: {status} (R switches CPU/GPU; session settings are not saved)");
                        } else {
                            eprintln!("Renderer: {status} (R switches CPU/GPU)");
                        }
                        self.renderer_status = Some(status);
                    }
                }
                if matches!(presentation, render::Presentation::Unavailable) {
                    self.frames.surface_unavailable(Instant::now());
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(trace) = &mut self.frame_trace {
            trace.flush();
        }
        self.sync_theme_preferences();
        if let Err(error) = self.settings.flush() {
            log::error!("{error:#}");
        }
        self.desktop.install_on_exit(&self.settings);
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !matches!(self.state, WindowState::Suspended) {
            return;
        }
        let window = ActiveWindow::new(event_loop).and_then(|mut window| {
            if let Some(mode) = self
                .profile
                .as_ref()
                .and_then(profile::Run::requested_present_mode)
            {
                window.render.set_present_mode(mode)?;
            }
            Ok(window)
        });
        self.state = match window {
            Ok(mut window) => {
                if let Some(trace) = &mut self.frame_trace {
                    let extent = window.render.viewport();
                    trace.extent(
                        extent.width as u32,
                        extent.height as u32,
                        window.render.window.scale_factor(),
                    );
                }
                self.ui.set_gpu_ribbons_supported(
                    visualizer::Renderer::gpu_ribbons_supported(&window.render.device().device)
                        && visualizer::Renderer::gpu_ribbons_adapter_supported(
                            window.render.device().adapter(),
                        ),
                );
                let limits = window.render.device().device.limits();
                self.ui.set_gpu_ribbon_edge_capacity(
                    (u64::from(limits.max_storage_buffer_binding_size).min(limits.max_buffer_size)
                        / (6 * 64)) as usize,
                );
                self.renderer_status = None;
                window
                    .visualizer_renderer
                    .set_adaptive_ribbon_resolution(true);
                if let Some(profile) = &self.profile {
                    window
                        .visualizer_renderer
                        .set_fixed_ribbon_downsize(profile.ribbon_downsize());
                }
                std::thread::spawn(updater::cleanup_after_start);
                window.view.set_zoom(self.settings.zoom);
                window.render.window.set_fullscreen(
                    (self.settings.fullscreen
                        && self
                            .profile
                            .as_ref()
                            .is_none_or(|p| p.requested_extent().is_none()))
                    .then_some(Fullscreen::Borderless(None)),
                );
                if let Some([width, height]) = self
                    .profile
                    .as_ref()
                    .and_then(profile::Run::requested_extent)
                {
                    let _ = window
                        .render
                        .window
                        .request_inner_size(winit::dpi::PhysicalSize::new(width, height));
                }
                let now = Instant::now();
                self.frames
                    .resized(window.render.window.inner_size().into(), now);
                self.frames.occlusion_changed(Occlusion::Visible, now);
                self.ui
                    .set_plate_mask(Some(window.render.plate_mask_image()));
                WindowState::Active(Box::new(window))
            }
            Err(error) => {
                event_loop.exit();
                WindowState::Failed(error)
            }
        };
        self.previous_frame = Instant::now();
        self.previous_presentation = None;
        self.ui.pause_frame_performance();
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        self.previous_presentation = None;
        self.ui.pause_frame_performance();
        self.desktop.set_pointer(None);
        self.modifiers = ModifiersState::empty();
        if matches!(self.state, WindowState::Active(_)) {
            self.state = WindowState::Suspended;
            self.frames.resized(SurfaceArea::Empty, Instant::now());
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window: WindowId,
        event: WindowEvent,
    ) {
        if matches!(event, WindowEvent::RedrawRequested)
            && self.frames.take_redraw_permit() == RedrawPermit::Idle
        {
            return;
        }
        if matches!(event, WindowEvent::CloseRequested) {
            event_loop.exit();
            return;
        }
        if let Err(error) = self.window_event(event) {
            self.state = WindowState::Failed(error);
            self.frames.resized(SurfaceArea::Empty, Instant::now());
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.exit_requested {
            event_loop.exit();
            return;
        }
        self.desktop.poll(&self.settings);
        while let Ok(event) = self.rx.try_recv() {
            self.ui.audio_event(event);
        }
        let now = Instant::now();
        if self
            .profile
            .as_ref()
            .is_some_and(|profile| profile.finished(now))
        {
            event_loop.exit();
            return;
        }
        if let Err(error) = self.settings.flush_if_due(now) {
            log::error!("{error:#}");
        }
        if self.frames.is_due(now) {
            match self.frames.kind(now) {
                FrameKind::Visible => {
                    let WindowState::Active(window) = &self.state else {
                        unreachable!("a drawable surface owns its window")
                    };
                    let refresh = window
                        .render
                        .window
                        .current_monitor()
                        .and_then(|monitor| monitor.refresh_rate_millihertz());
                    let synchronized = matches!(
                        window.render.surface_configuration().present_mode,
                        wgpu::PresentMode::AutoVsync
                            | wgpu::PresentMode::Fifo
                            | wgpu::PresentMode::FifoRelaxed
                    );
                    let interval =
                        presentation_interval(self.frame_rate_limit(), refresh, synchronized);
                    self.frames
                        .request_visible_frame_with_interval(now, interval);
                    window.render.window.request_redraw();
                }
                FrameKind::Background => self.frames.background_tick_finished(now),
            }
        } else if self.frames.redraw_is_pending() && self.frames.kind(now) == FrameKind::Visible {
            let WindowState::Active(window) = &self.state else {
                unreachable!("a pending redraw owns its window")
            };
            window.render.window.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.frames.deadline(now)));
    }
}

fn main() -> anyhow::Result<()> {
    pretty_env_logger::init();
    if updater::run_helper()? {
        return Ok(());
    }
    let playback = match cli::parse(std::env::args_os().skip(1))? {
        cli::Command::Help => {
            cli::help();
            return Ok(());
        }
        cli::Command::Version => {
            println!("Benga {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        cli::Command::Render(options) => {
            let settings = if let Some(path) = &options.config {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("Could not read {}", path.display()))?;
                ron::from_str::<Config>(&text)
                    .with_context(|| format!("Invalid settings in {}", path.display()))?
            } else {
                (*Saved::<Config>::load(
                    config::config_file()?,
                    Some(config::legacy_config_file()?),
                )?)
                .clone()
            };
            return benga::export::run(options, &settings);
        }
        command @ (cli::Command::Desktop
        | cli::Command::Profile
        | cli::Command::Experimental { .. }
        | cli::Command::Cpu { .. }) => command.playback().expect("desktop playback command"),
    };
    let profile = if playback.profile {
        profile::Run::from_args()?
    } else {
        None
    };
    anyhow::ensure!(
        playback.gpu_ribbons
            || profile
                .as_ref()
                .is_none_or(|p| p.ribbon_downsize().is_none()),
        "BENGA_PROFILE_RIBBON_DOWNSIZE requires GPU ribbons (omit --cpu-ribbons)"
    );
    let mut settings =
        Saved::<Config>::load(config::config_file()?, Some(config::legacy_config_file()?))?;
    if playback.ephemeral {
        settings.make_ephemeral();
    }
    if playback.trial_shutter {
        settings.edit().shutter_frames = config::ShutterFrames::try_from(2.0)?;
    }
    let mut ui = Ui::new(&settings)?;
    ui.set_gpu_ribbons(true);
    if !playback.gpu_ribbons {
        ui.toggle_gpu_ribbons();
    }
    if profile.is_some() {
        ui.dismiss_startup();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let mut audio_error = None;
    let _stream = if profile.is_some() {
        ui.set_preamp(PreampDb::try_from(0.0)?);
        ui.audio_event(AudioEvent::Config {
            samplerate: profile::SAMPLE_RATE,
        });
        None
    } else {
        match capture_stream(tx) {
            Ok(stream) => Some(stream),
            Err(error) => {
                log::error!("Audio capture: {error:#}");
                audio_error = Some(format!("{error:#}"));
                None
            }
        }
    };
    let event_loop = EventLoop::new()?;
    let mut app = App {
        desktop: desktop::Desktop::new(&settings, audio_error, profile.is_some())?,
        exit_requested: false,
        state: WindowState::Suspended,
        rx,
        ui,
        settings,
        mouse: Point::ORIGIN,
        modifiers: ModifiersState::empty(),
        frames: FrameSchedule::new(Instant::now()),
        previous_frame: Instant::now(),
        previous_presentation: None,
        profile,
        session_ephemeral: playback.ephemeral,
        renderer_status: None,
        frame_trace: frame_trace::FrameTrace::from_env(true),
    };
    let result = event_loop.run_app(&mut app);
    app.settings.flush()?;
    result?;
    if let WindowState::Failed(error) = app.state {
        return Err(error);
    }
    if let Some(profile) = &app.profile {
        if let WindowState::Active(window) = &app.state {
            let info = window.render.device().adapter().get_info();
            let size = window.render.viewport();
            eprintln!(
                "Target: {}x{} pixels; {} ({:?})",
                size.width, size.height, info.name, info.backend
            );
        }
        profile.report()?;
        if let (Some(requested), WindowState::Active(window)) =
            (profile.requested_extent(), &app.state)
        {
            let size = window.render.viewport();
            let actual = [size.width as u32, size.height as u32];
            anyhow::ensure!(actual == requested, "Requested profile extent {requested:?} differs from actual {actual:?}; reported timings cover the actual extent");
        }
    }
    Ok(())
}
