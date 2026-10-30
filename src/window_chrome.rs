//! Platform window controls drawn above the scene, in logical window points.

#[cfg(target_os = "macos")]
mod macos;

use std::time::{Duration, Instant};

use vello::{
    kurbo::{Affine, Circle, Line, Point, Rect, Size, Stroke},
    peniko::{Color, Fill, Mix},
    Scene,
};
use winit::window::{CursorIcon, ResizeDirection, Window, WindowAttributes};

use crate::{
    logo::Logo,
    theme::{mix, ThemeColors},
};

const HEIGHT: f64 = 28.0;
const NATIVE_INSET: f64 = 78.0;
const WINDOWS_CELL: f64 = 46.0;
const LINUX_CELL: f64 = 26.0;
const RESIZE_GRIP: f64 = 5.0;
const RESIZE_CORNER: f64 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    MacOS,
    Windows,
    Linux,
}

impl Kind {
    pub fn for_platform() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOS
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowButton {
    Minimize,
    Maximize,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Button(WindowButton),
    Drag,
    Resize(ResizeDirection),
}

impl Hit {
    pub fn cursor(self) -> CursorIcon {
        match self {
            Self::Button(_) => CursorIcon::Default,
            Self::Drag => CursorIcon::Default,
            Self::Resize(ResizeDirection::West | ResizeDirection::East) => CursorIcon::EwResize,
            Self::Resize(ResizeDirection::North | ResizeDirection::South) => CursorIcon::NsResize,
            Self::Resize(ResizeDirection::NorthWest | ResizeDirection::SouthEast) => {
                CursorIcon::NwseResize
            }
            Self::Resize(ResizeDirection::NorthEast | ResizeDirection::SouthWest) => {
                CursorIcon::NeswResize
            }
        }
    }
}

pub fn attributes(attrs: WindowAttributes) -> WindowAttributes {
    #[cfg(target_os = "macos")]
    {
        macos::attributes(attrs)
    }
    #[cfg(not(target_os = "macos"))]
    {
        attrs.with_decorations(false)
    }
}

pub struct Chrome {
    kind: Kind,
    height: f64,
    left_inset: f64,
    logo: Logo,
    logo_opacity: f32,
    pointer: Option<Point>,
    pressed: Option<WindowButton>,
    last_drag_click: Option<(Point, Instant)>,
    #[cfg(target_os = "macos")]
    native_opacity: Option<f32>,
}

impl Chrome {
    pub fn new(window: &Window) -> anyhow::Result<Self> {
        let mut chrome = Self::with_kind(Kind::for_platform())?;
        chrome.refresh_native_metrics(window);
        #[cfg(target_os = "windows")]
        cast_shadow(window);
        Ok(chrome)
    }

    fn with_kind(kind: Kind) -> anyhow::Result<Self> {
        Ok(Self {
            kind,
            height: HEIGHT,
            left_inset: NATIVE_INSET,
            logo: Logo::new()?,
            logo_opacity: 1.0,
            pointer: None,
            pressed: None,
            last_drag_click: None,
            #[cfg(target_os = "macos")]
            native_opacity: None,
        })
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The windowed bar height in logical points, independent of scene zoom.
    pub fn height(&self) -> f64 {
        self.height
    }

    pub fn set_logo_opacity(&mut self, opacity: f32) {
        self.logo_opacity = opacity.clamp(0.0, 1.0);
    }

    pub fn refresh_native_metrics(&mut self, window: &Window) {
        #[cfg(target_os = "macos")]
        if self.kind == Kind::MacOS && window.fullscreen().is_none() {
            let metrics = macos::configure(window);
            self.height = metrics.height;
            self.left_inset = metrics.left_inset;
            // AppKit can recreate its titlebar view after changing screen mode.
            self.native_opacity = None;
        }
        #[cfg(not(target_os = "macos"))]
        let _ = window;
    }

    pub fn native_opacity(&mut self, window: &Window, opacity: f32) {
        #[cfg(target_os = "macos")]
        if self.kind == Kind::MacOS && window.fullscreen().is_none() {
            let opacity = opacity.clamp(0.0, 1.0);
            if self.native_opacity != Some(opacity) {
                if macos::set_opacity(window, opacity) {
                    self.native_opacity = Some(opacity);
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (window, opacity);
    }

    pub fn pointer(&mut self, pointer: Option<Point>) {
        self.pointer = pointer;
    }

    pub fn set_pressed(&mut self, pressed: Option<WindowButton>) {
        self.pressed = pressed;
    }

    pub fn take_pressed(&mut self) -> Option<WindowButton> {
        self.pressed.take()
    }

    /// Includes the native traffic-light area, so hovering any titlebar keeps
    /// the same inactivity fade awake as hovering the presentation controls.
    pub fn is_hovered(&self, viewport: Size, scale: f64, fullscreen: bool) -> bool {
        !fullscreen
            && self.pointer.is_some_and(|point| {
                Rect::new(0.0, 0.0, viewport.width, self.height * scale).contains(point)
            })
    }

    pub fn hit_test(
        &self,
        point: Point,
        viewport: Size,
        scale: f64,
        fullscreen: bool,
        maximized: bool,
    ) -> Option<Hit> {
        if fullscreen || scale <= 0.0 {
            return None;
        }
        let point = Point::new(point.x / scale, point.y / scale);
        let size = Size::new(viewport.width / scale, viewport.height / scale);
        if !Rect::from_origin_size(Point::ORIGIN, size).contains(point) {
            return None;
        }
        if let Some(edge) = self.resize_edge(point, size, maximized) {
            return Some(Hit::Resize(edge));
        }
        if point.y >= self.height {
            return None;
        }
        if self.kind == Kind::MacOS && point.x < self.left_inset {
            return None;
        }
        self.buttons(size.width)
            .find(|(_, rect)| rect.contains(point))
            .map(|(button, _)| Hit::Button(button))
            .or(Some(Hit::Drag))
    }

    /// `point` is logical: double-click tolerance must stay the same at Retina
    /// and regular DPI. The caller consumes both clicks before scene controls.
    pub fn drag_double_click(&mut self, point: Point, now: Instant) -> bool {
        let double = self.last_drag_click.take().is_some_and(|(previous, at)| {
            now.saturating_duration_since(at) < Duration::from_millis(500)
                && previous.distance(point) < 4.0
        });
        if !double {
            self.last_drag_click = Some((point, now));
        }
        double
    }

    pub fn reset_drag_clicks(&mut self) {
        self.last_drag_click = None;
    }

    fn buttons(&self, width: f64) -> impl Iterator<Item = (WindowButton, Rect)> {
        let height = self.height;
        let (buttons, x, cell) = match self.kind {
            Kind::MacOS => (None, 0.0, 0.0),
            Kind::Windows => {
                let cell = WINDOWS_CELL.min(width / 3.0);
                (
                    Some([
                        WindowButton::Minimize,
                        WindowButton::Maximize,
                        WindowButton::Close,
                    ]),
                    width - 3.0 * cell,
                    cell,
                )
            }
            Kind::Linux => (
                Some([
                    WindowButton::Close,
                    WindowButton::Maximize,
                    WindowButton::Minimize,
                ]),
                0.0,
                LINUX_CELL.min(width / 3.0),
            ),
        };
        buttons
            .into_iter()
            .flatten()
            .enumerate()
            .map(move |(index, button)| {
                let x = x + index as f64 * cell;
                (button, Rect::new(x, 0.0, x + cell, height))
            })
    }

    fn resize_edge(&self, point: Point, size: Size, maximized: bool) -> Option<ResizeDirection> {
        if self.kind == Kind::MacOS || maximized {
            return None;
        }
        #[derive(Clone, Copy)]
        enum End {
            Start,
            Finish,
        }
        let near = |at: f64, extent: f64, reach: f64| {
            if at < reach {
                Some(End::Start)
            } else if at >= extent - reach {
                Some(End::Finish)
            } else {
                None
            }
        };
        let (x, y) = (
            near(point.x, size.width, RESIZE_GRIP),
            near(point.y, size.height, RESIZE_GRIP),
        );
        if x.is_none() && y.is_none() {
            return None;
        }
        let x = x.or_else(|| near(point.x, size.width, RESIZE_CORNER));
        let y = y.or_else(|| near(point.y, size.height, RESIZE_CORNER));
        Some(match (x, y) {
            (Some(End::Start), None) => ResizeDirection::West,
            (Some(End::Finish), None) => ResizeDirection::East,
            (None, Some(End::Start)) => ResizeDirection::North,
            (None, Some(End::Finish)) => ResizeDirection::South,
            (Some(End::Start), Some(End::Start)) => ResizeDirection::NorthWest,
            (Some(End::Finish), Some(End::Start)) => ResizeDirection::NorthEast,
            (Some(End::Start), Some(End::Finish)) => ResizeDirection::SouthWest,
            (Some(End::Finish), Some(End::Finish)) => ResizeDirection::SouthEast,
            (None, None) => unreachable!("one edge is within reach"),
        })
    }

    fn logo_area(&self, width: f64) -> Option<Rect> {
        let left = match self.kind {
            Kind::MacOS => self.left_inset + 12.0,
            Kind::Linux => NATIVE_INSET + 10.0,
            Kind::Windows => 14.0,
        };
        let right = if self.kind == Kind::Windows {
            width - 3.0 * WINDOWS_CELL - 14.0
        } else {
            width - 14.0
        };
        let center = width * 0.5;
        let available = 2.0 * (center - left).min(right - center);
        if available <= 0.0 {
            return None;
        }
        let height = (self.height - 10.0).clamp(1.0, 20.0);
        let top = (self.height - height) * 0.5;
        let bounds = self.logo.bounds();
        let width = (bounds.width() * height / bounds.height()).min(available);
        Some(Rect::new(
            center - width * 0.5,
            top,
            center + width * 0.5,
            top + height,
        ))
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        viewport: Size,
        scale: f64,
        colors: ThemeColors,
        ink: Color,
        opacity: f32,
        fullscreen: bool,
        maximized: bool,
        focused: bool,
    ) -> bool {
        if fullscreen {
            return false;
        }
        let opacity = if self.kind == Kind::MacOS {
            opacity.clamp(0.0, 1.0)
        } else {
            1.0
        };
        if opacity == 0.0 {
            return false;
        }
        let width = viewport.width / scale;
        let band = Rect::new(0.0, 0.0, width, self.height);
        let transform = Affine::scale(scale);
        scene.push_layer(Fill::NonZero, Mix::Normal, opacity, transform, &band);
        scene.fill(
            Fill::NonZero,
            transform,
            mix(colors.background, ink, 0.05).with_alpha(0.50),
            None,
            &band,
        );
        if self.logo_opacity > 0.0 {
            if let Some(area) = self.logo_area(width) {
                let title_ink =
                    ink.with_alpha(self.logo_opacity * if focused { 0.86 } else { 0.48 });
                self.logo
                    .draw(scene, area, transform, colors.background, title_ink);
            }
        }
        for (button, rect) in self.buttons(width) {
            let hovered = self
                .pointer
                .is_some_and(|point| rect.contains(Point::new(point.x / scale, point.y / scale)));
            let pressed = hovered && self.pressed == Some(button);
            let close_hover = hovered && button == WindowButton::Close;
            let glyph_ink = if close_hover {
                Color::WHITE
            } else {
                ink.with_alpha(if focused { 0.88 } else { 0.45 })
            };
            match self.kind {
                Kind::Windows => {
                    if hovered {
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            if close_hover {
                                Color::from_rgb8(if pressed { 180 } else { 232 }, 17, 35)
                            } else {
                                mix(colors.background, ink, if pressed { 0.25 } else { 0.14 })
                            },
                            None,
                            &rect,
                        );
                    }
                }
                Kind::Linux => {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        if close_hover {
                            mix(
                                Color::from_rgb8(233, 84, 32),
                                colors.background,
                                if pressed { 0.20 } else { 0.0 },
                            )
                        } else {
                            mix(
                                colors.background,
                                ink,
                                if pressed {
                                    0.30
                                } else if hovered {
                                    0.22
                                } else {
                                    0.10
                                },
                            )
                        },
                        None,
                        &Circle::new(rect.center(), 8.5),
                    );
                }
                Kind::MacOS => {}
            }
            draw_button_glyph(
                scene,
                transform,
                button,
                rect.center(),
                if self.kind == Kind::Windows {
                    10.0
                } else {
                    7.0
                },
                glyph_ink,
                maximized,
            );
        }
        scene.pop_layer();
        true
    }
}

fn draw_button_glyph(
    scene: &mut Scene,
    transform: Affine,
    button: WindowButton,
    center: Point,
    size: f64,
    color: Color,
    maximized: bool,
) {
    let half = size * 0.5;
    let transform = transform * Affine::translate(center.to_vec2());
    let stroke = Stroke::new(1.0);
    match button {
        WindowButton::Minimize => scene.stroke(
            &stroke,
            transform,
            color,
            None,
            &Line::new((-half, 0.0), (half, 0.0)),
        ),
        WindowButton::Close => {
            scene.stroke(
                &stroke,
                transform,
                color,
                None,
                &Line::new((-half, -half), (half, half)),
            );
            scene.stroke(
                &stroke,
                transform,
                color,
                None,
                &Line::new((half, -half), (-half, half)),
            );
        }
        WindowButton::Maximize if maximized => {
            let rect = Rect::new(-half + 2.0, -half, half, half - 2.0);
            scene.stroke(&stroke, transform, color, None, &rect);
            scene.stroke(
                &stroke,
                transform,
                color,
                None,
                &Rect::new(-half, -half + 2.0, half - 2.0, half),
            );
        }
        WindowButton::Maximize => scene.stroke(
            &stroke,
            transform,
            color,
            None,
            &Rect::new(-half, -half, half, half),
        ),
    }
}

#[cfg(target_os = "windows")]
fn cast_shadow(window: &Window) {
    use windows_sys::Win32::{Graphics::Dwm::DwmExtendFrameIntoClientArea, UI::Controls::MARGINS};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle().expect("live window handle");
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        unreachable!("Win32 window")
    };
    let margins = MARGINS {
        cxLeftWidth: 1,
        cxRightWidth: 1,
        cyTopHeight: 1,
        cyBottomHeight: 1,
    };
    // SAFETY: the handle belongs to the live window and margins remains valid for the call.
    let result = unsafe { DwmExtendFrameIntoClientArea(handle.hwnd.get() as _, &margins) };
    if result < 0 {
        log::warn!("Could not extend the window shadow ({result:#x})");
    }
}

#[cfg(test)]
#[path = "window_chrome/tests.rs"]
mod tests;
