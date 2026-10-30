//! First-start guidance and background update controls, separate from the visualizer.
use crate::updater::{self, Prepared, Release};
use benga::{
    config::Config,
    text::{Fonts, Text, UiFace},
    theme::{mix, ThemeColors},
};
use std::{
    collections::HashMap,
    sync::mpsc::{self, Receiver},
};
use vello::{
    kurbo::{Affine, Point, Rect, Size, Stroke},
    peniko::{Color, Fill},
    Scene,
};

enum Event {
    Checked(anyhow::Result<Option<Release>>),
    Prepared(anyhow::Result<Prepared>),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Open,
    Close,
    Check,
    ToggleChecks,
    ToggleAutomatic,
    Install,
}

pub struct Desktop {
    pub open: bool,
    fonts: Fonts,
    targets: Vec<(Rect, Action)>,
    pointer: Option<Point>,
    text: HashMap<(String, TextStyle), Text>,
    receiver: Option<Receiver<Event>>,
    available: Option<Release>,
    prepared: Option<Prepared>,
    status: String,
    audio_error: Option<String>,
    automatic: bool,
}

impl Desktop {
    pub fn new(
        config: &Config,
        audio_error: Option<String>,
        profile: bool,
    ) -> anyhow::Result<Self> {
        let mut panel = Self {
            open: !profile && (!config.onboarding_seen || audio_error.is_some()),
            fonts: Fonts::new()?,
            targets: Vec::new(),
            pointer: None,
            text: HashMap::new(),
            receiver: None,
            available: None,
            prepared: None,
            status: if updater::configured() {
                "Updates"
            } else {
                "Updates unavailable in this build."
            }
            .into(),
            audio_error,
            automatic: config.updates.auto_install,
        };
        if !profile && config.updates.check_on_start && updater::configured() {
            panel.check();
        }
        Ok(panel)
    }

    pub fn check(&mut self) {
        if self.receiver.is_some() {
            return;
        }
        if !updater::configured() {
            self.status = "Updates unavailable in this build.".into();
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        self.status = "Checking...".into();
        std::thread::spawn(move || {
            let _ = tx.send(Event::Checked(updater::check()));
        });
    }

    fn download(&mut self) {
        if self.receiver.is_some() {
            return;
        }
        if let Some(release) = self.available.clone() {
            let (tx, rx) = mpsc::channel();
            self.receiver = Some(rx);
            self.status = format!("Downloading v{}...", release.version);
            std::thread::spawn(move || {
                let _ = tx.send(Event::Prepared(updater::prepare(&release)));
            });
        }
    }

    pub fn poll(&mut self, config: &Config) {
        self.automatic = config.updates.auto_install;
        if let Some(event) = self.receiver.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.receiver = None;
            match event {
                Event::Checked(Ok(Some(release))) => {
                    self.status = format!("v{} available", release.version);
                    self.available = Some(release);
                    if config.updates.check_on_start && self.automatic {
                        self.download();
                    }
                }
                Event::Checked(Ok(None)) => self.status = "Up to date.".into(),
                Event::Checked(Err(error)) => {
                    log::info!("Update check: {error:#}");
                    self.status = "Couldn't check. Try again.".into();
                }
                Event::Prepared(Ok(prepared)) => {
                    self.prepared = Some(prepared);
                    self.status = if self.automatic {
                        "Ready. Installs when you quit."
                    } else {
                        "Update ready."
                    }
                    .into();
                }
                Event::Prepared(Err(error)) => {
                    log::warn!("Update preparation: {error:#}");
                    self.status = "Update failed. Try again.".into();
                }
            }
        }
    }

    /// Returns true when the caller should exit for the updater helper.
    pub fn click(&mut self, point: Point, config: &mut Config) -> bool {
        let Some(action) = self
            .targets
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, a)| *a)
        else {
            return false;
        };
        match action {
            Action::Open => self.open = true,
            Action::Close => {
                self.open = false;
                config.onboarding_seen = true;
            }
            Action::Check => self.check(),
            Action::ToggleChecks => {
                config.updates.check_on_start = !config.updates.check_on_start;
            }
            Action::ToggleAutomatic => {
                config.updates.auto_install = !config.updates.auto_install;
                self.automatic = config.updates.auto_install;
            }
            Action::Install => {
                if let Some(prepared) = self.prepared.take() {
                    match prepared.launch(true) {
                        Ok(()) => return true,
                        Err(error) => {
                            log::warn!("Update installation: {error:#}");
                            self.status = "Couldn't install the update.".into();
                        }
                    }
                } else {
                    self.download();
                }
            }
        }
        false
    }

    pub fn close(&mut self, config: &mut Config) {
        self.open = false;
        config.onboarding_seen = true;
    }
    pub fn contains(&self, point: Point) -> bool {
        self.targets.iter().any(|(rect, _)| rect.contains(point))
    }
    pub fn install_on_exit(&mut self, config: &Config) {
        if config.updates.check_on_start && config.updates.auto_install {
            if let Some(prepared) = self.prepared.take() {
                if let Err(error) = prepared.launch(false) {
                    log::error!("Could not apply update: {error:#}");
                }
            }
        }
    }

    pub fn set_pointer(&mut self, pointer: Option<Point>) {
        self.pointer = pointer;
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        viewport: Size,
        scale: f64,
        config: &Config,
        colors: ThemeColors,
        ink: Color,
    ) -> bool {
        self.targets.clear();
        let palette = DialogPalette::new(colors, ink);
        if !self.open {
            if let Some(release) = &self.available {
                let label = format!("Benga v{} available", release.version);
                let width = (viewport.width / scale - 24.0).clamp(100.0, 248.0);
                let rect = Rect::new(
                    viewport.width / scale - width - 12.0,
                    12.0,
                    viewport.width / scale - 12.0,
                    44.0,
                );
                let mut canvas = Canvas {
                    scene,
                    transform: Affine::scale(scale),
                    palette,
                };
                self.button(&mut canvas, rect, &label, Action::Open, false, true);
                return true;
            }
            return false;
        }
        let width = 440.0;
        let height = 288.0;
        let scale = scale
            .min(viewport.width / (width + 24.0))
            .min(viewport.height / (height + 24.0));
        let left = (viewport.width / scale - width) * 0.5;
        let top = (viewport.height / scale - height) * 0.5;
        let transform = Affine::scale(scale) * Affine::translate((left, top));
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            colors.background.with_alpha(0.70),
            None,
            &Rect::from_origin_size(Point::ORIGIN, viewport),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            palette.surface,
            None,
            &Rect::new(0.0, 0.0, width, height),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            palette.border,
            None,
            &Rect::new(0.0, 0.0, width, height),
        );
        let mut canvas = Canvas {
            scene,
            transform,
            palette,
        };
        self.label(
            &mut canvas,
            Point::new(20.0, 32.0),
            width - 40.0,
            &format!("Benga  v{}", env!("CARGO_PKG_VERSION")),
            TextStyle::Title,
            palette.ink,
        );
        let audio = if self.audio_error.is_some() {
            if cfg!(target_os = "macos") {
                "No audio: check recording permission and reopen."
            } else if cfg!(windows) {
                "No audio: check your output device and reopen."
            } else {
                "No audio: select your output's monitor and reopen."
            }
        } else if cfg!(target_os = "macos") {
            "macOS: allow system audio recording."
        } else if cfg!(windows) {
            "Windows: uses the default audio output."
        } else {
            "Linux: select your output's monitor in pavucontrol."
        };
        self.label(
            &mut canvas,
            Point::new(20.0, 61.0),
            width - 40.0,
            audio,
            TextStyle::Label,
            palette.ink,
        );
        self.label(
            &mut canvas,
            Point::new(20.0, 81.0),
            width - 40.0,
            "Double-click: fullscreen. Esc: leave.",
            TextStyle::Label,
            palette.muted,
        );
        canvas.scene.fill(
            Fill::NonZero,
            transform,
            palette.border,
            None,
            &Rect::new(20.0, 98.0, width - 20.0, 99.0),
        );
        self.label(
            &mut canvas,
            Point::new(20.0, 121.0),
            width - 40.0,
            &self.status.clone(),
            TextStyle::Label,
            palette.muted,
        );
        let checks = format!(
            "[{}] Check on startup",
            if config.updates.check_on_start {
                "x"
            } else {
                " "
            }
        );
        self.button(
            &mut canvas,
            Rect::new(20.0, 136.0, width - 20.0, 166.0),
            &checks,
            Action::ToggleChecks,
            config.updates.check_on_start,
            true,
        );
        let automatic = format!(
            "[{}] Install updates on quit",
            if config.updates.auto_install {
                "x"
            } else {
                " "
            }
        );
        self.button(
            &mut canvas,
            Rect::new(20.0, 174.0, width - 20.0, 204.0),
            &automatic,
            Action::ToggleAutomatic,
            config.updates.auto_install,
            true,
        );
        let idle = self.receiver.is_none();
        self.button(
            &mut canvas,
            Rect::new(20.0, 230.0, 140.0, 264.0),
            "Check now",
            Action::Check,
            false,
            idle && updater::configured(),
        );
        if self.available.is_some() {
            self.button(
                &mut canvas,
                Rect::new(148.0, 230.0, 296.0, 264.0),
                if self.prepared.is_some() {
                    "Install & restart"
                } else {
                    "Download"
                },
                Action::Install,
                false,
                idle,
            );
        }
        self.button(
            &mut canvas,
            Rect::new(304.0, 230.0, 420.0, 264.0),
            "Close / Esc",
            Action::Close,
            false,
            true,
        );
        true
    }

    fn label(
        &mut self,
        canvas: &mut Canvas<'_>,
        origin: Point,
        width: f64,
        label: &str,
        style: TextStyle,
        color: Color,
    ) {
        let key = (label.to_owned(), style);
        let plan = self.text.entry(key).or_insert_with(|| {
            self.fonts.layout_ui(
                label,
                if style == TextStyle::Title {
                    16.0
                } else {
                    12.0
                },
                if style == TextStyle::Label {
                    UiFace::Light
                } else {
                    UiFace::SemiBold
                },
            )
        });
        let fit = (width / plan.width().max(1.0)).min(1.0);
        plan.draw_left(
            canvas.scene,
            canvas.transform * Affine::translate(origin.to_vec2()) * Affine::scale(fit),
            color,
        );
    }

    fn button(
        &mut self,
        canvas: &mut Canvas<'_>,
        rect: Rect,
        label: &str,
        action: Action,
        selected: bool,
        enabled: bool,
    ) {
        let physical = canvas.transform.transform_rect_bbox(rect);
        let hovered = enabled && self.pointer.is_some_and(|point| physical.contains(point));
        let palette = canvas.palette;
        let fill = if hovered {
            palette.hover
        } else if selected {
            palette.selected
        } else {
            palette.button
        };
        canvas
            .scene
            .fill(Fill::NonZero, canvas.transform, fill, None, &rect);
        canvas.scene.stroke(
            &Stroke::new(1.0),
            canvas.transform,
            palette.ink.with_alpha(if hovered {
                0.70
            } else if enabled {
                0.25
            } else {
                0.10
            }),
            None,
            &rect,
        );
        self.label(
            canvas,
            Point::new(rect.x0 + 8.0, rect.center().y + 4.0),
            rect.width() - 16.0,
            label,
            TextStyle::Button,
            palette.ink.with_alpha(if enabled { 0.95 } else { 0.32 }),
        );
        if enabled {
            self.targets.push((physical, action));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum TextStyle {
    Title,
    Label,
    Button,
}

#[derive(Clone, Copy)]
struct DialogPalette {
    surface: Color,
    ink: Color,
    muted: Color,
    border: Color,
    button: Color,
    selected: Color,
    hover: Color,
}

impl DialogPalette {
    fn new(colors: ThemeColors, ink: Color) -> Self {
        Self {
            surface: mix(colors.background, ink, 0.03),
            ink,
            muted: mix(colors.background, ink, 0.65),
            border: mix(colors.background, ink, 0.28),
            button: mix(colors.background, ink, 0.07),
            selected: mix(colors.background, ink, 0.12),
            hover: mix(colors.background, ink, 0.20),
        }
    }
}

struct Canvas<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    palette: DialogPalette,
}

#[cfg(test)]
#[path = "desktop_tests.rs"]
mod tests;
