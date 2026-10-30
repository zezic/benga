//! Keep AppKit's frame and traffic lights while drawing the titlebar in Vello.

use objc2::{rc::Retained, MainThreadMarker};
use objc2_app_kit::{
    NSTitlebarSeparatorStyle, NSView, NSWindow, NSWindowButton, NSWindowStyleMask,
};
use winit::{
    platform::macos::WindowAttributesExtMacOS,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::{Window, WindowAttributes},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct NativeMetrics {
    pub height: f64,
    pub left_inset: f64,
}

pub(super) fn attributes(attributes: WindowAttributes) -> WindowAttributes {
    attributes
        .with_titlebar_transparent(true)
        .with_fullsize_content_view(true)
        .with_title_hidden(true)
        .with_movable_by_window_background(false)
}

pub(super) fn configure(window: &Window) -> NativeMetrics {
    let (native, content) = native_views(window);
    native.setTitlebarSeparatorStyle(NSTitlebarSeparatorStyle::None);
    content.layoutSubtreeIfNeeded();
    // Both rectangles are in the window's base coordinate system, including
    // when winit's content view is flipped.
    let bounds = content.convertRect_toView(content.bounds(), None);
    let layout = native.contentLayoutRect();
    let top = bounds.origin.y + bounds.size.height;
    let buttons: Vec<_> = buttons(&native)
        .map(|button| {
            let rect = button.convertRect_toView(button.bounds(), None);
            (
                rect.origin.x + rect.size.width,
                top - rect.origin.y - rect.size.height * 0.5,
            )
        })
        .collect();
    measure(top - layout.origin.y - layout.size.height, &buttons)
}

/// AppKit's control positions account for the taller macOS 26+ titlebar. Using
/// their live centres also covers styles whose transparent contentLayoutRect
/// does not reserve the whole control band.
fn measure(layout_height: f64, buttons: &[(f64, f64)]) -> NativeMetrics {
    let mut height = 28.0_f64;
    let mut left_inset = 78.0_f64;
    if layout_height.is_finite() && layout_height > 0.0 {
        height = height.max(layout_height);
    }
    for &(right, centre_from_top) in buttons {
        if right.is_finite() && right > 0.0 {
            left_inset = left_inset.max(right + 12.0);
        }
        if centre_from_top.is_finite() && centre_from_top > 0.0 {
            height = height.max(centre_from_top * 2.0);
        }
    }
    NativeMetrics { height, left_inset }
}

pub(super) fn set_opacity(window: &Window, opacity: f32) -> bool {
    // Fullscreen's native titlebar is managed by AppKit, including its reveal
    // animation and temporary view hierarchy.
    let (native, content) = native_views(window);
    if window.fullscreen().is_some() || native.styleMask().contains(NSWindowStyleMask::FullScreen) {
        return false;
    }
    let opacity = f64::from(opacity.clamp(0.0, 1.0));
    if let Some(overlay) = titlebar_overlay(&native, &content) {
        overlay.setAlphaValue(opacity);
        // AppKit's hit testing need not exclude a view merely because its
        // alpha is zero. Hidden controls must not consume invisible clicks.
        overlay.setHidden(opacity == 0.0);
    } else {
        for button in buttons(&native) {
            button.setAlphaValue(opacity);
            button.setHidden(opacity == 0.0);
        }
    }
    true
}

fn native_views(window: &Window) -> (Retained<NSWindow>, Retained<NSView>) {
    let _main_thread = MainThreadMarker::new().expect("window chrome runs on the AppKit thread");
    let handle = window.window_handle().expect("live window handle");
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        unreachable!("macOS uses AppKit window handles");
    };
    // SAFETY: winit owns this live NSView; this function is called on the
    // AppKit thread and retains the objects for the duration of the operation.
    let content = unsafe { Retained::retain(handle.ns_view.cast::<NSView>().as_ptr()) }
        .expect("winit's content view is alive");
    let native = content.window().expect("winit's view belongs to a window");
    (native, content)
}

fn buttons(window: &NSWindow) -> impl Iterator<Item = Retained<objc2_app_kit::NSButton>> + '_ {
    [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ]
    .into_iter()
    .filter_map(|button| window.standardWindowButton(button))
}

/// Find the highest titlebar ancestor that excludes the app's content. No
/// private AppKit classes, selectors, or assumptions about class names are used.
fn titlebar_overlay(window: &NSWindow, content: &NSView) -> Option<Retained<NSView>> {
    let close = window.standardWindowButton(NSWindowButton::CloseButton)?;
    // SAFETY: the window retains its view hierarchy throughout this call on
    // the AppKit thread; returned ancestors are retained before continuing.
    let mut overlay = unsafe { close.superview() }?;
    if std::ptr::eq(&*overlay, content) || content.isDescendantOf(&overlay) {
        return None;
    }
    while let Some(parent) = unsafe { overlay.superview() } {
        if std::ptr::eq(&*parent, content) || content.isDescendantOf(&parent) {
            break;
        }
        overlay = parent;
    }
    Some(overlay)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_button_geometry_grows_the_band_and_inset() {
        assert_eq!(
            measure(22.0, &[(22.0, 14.0), (44.0, 14.0), (66.0, 14.0)]),
            NativeMetrics {
                height: 28.0,
                left_inset: 78.0
            }
        );
        assert_eq!(
            measure(28.0, &[(34.0, 20.0), (60.0, 20.0), (86.0, 20.0)]),
            NativeMetrics {
                height: 40.0,
                left_inset: 98.0
            }
        );
        assert_eq!(measure(48.0, &[(86.0, 20.0)]).height, 48.0);
    }

    #[test]
    fn absent_or_invalid_geometry_retains_the_legacy_native_band() {
        assert_eq!(
            measure(f64::NAN, &[(f64::INFINITY, -1.0)]),
            NativeMetrics {
                height: 28.0,
                left_inset: 78.0
            }
        );
    }
}
