//! Check native titlebar metrics and opacity on this process's hidden window.
//! `cargo run --example window_chrome_probe`

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    native::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("The native titlebar probe applies to macOS.");
}

#[cfg(target_os = "macos")]
mod native {
    use std::time::{Duration, Instant};

    use anyhow::{ensure, Context};
    use benga::window_chrome::{self, Chrome};
    use objc2::rc::Retained;
    use objc2_app_kit::{NSView, NSWindow, NSWindowButton};
    use winit::{
        application::ApplicationHandler,
        dpi::LogicalSize,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
        raw_window_handle::{HasWindowHandle, RawWindowHandle},
        window::{Window, WindowId},
    };

    struct Probe {
        window: Option<Window>,
        chrome: Option<Chrome>,
        phase: usize,
        next: Instant,
        deadline: Instant,
        failure: Option<anyhow::Error>,
    }

    impl Probe {
        fn step(&mut self) -> anyhow::Result<bool> {
            let window = self.window.as_ref().context("The probe window is ready")?;
            let chrome = self.chrome.as_mut().context("The probe chrome is ready")?;
            let opacity = [1.0, 0.5, 0.0, 1.0][self.phase];
            chrome.native_opacity(window, opacity);
            let (native, content) = views(window)?;
            ensure!(
                near(native.alphaValue(), 1.0),
                "Native window alpha changed"
            );
            ensure!(near(content.alphaValue(), 1.0), "App content alpha changed");
            ensure!(
                !content.isHiddenOrHasHiddenAncestor(),
                "App content became hidden"
            );
            let close = native
                .standardWindowButton(NSWindowButton::CloseButton)
                .context("The hidden probe window has a native close button")?;
            let effective = effective_alpha(&close);
            let hidden = close.isHiddenOrHasHiddenAncestor();
            ensure!(
                near(effective, f64::from(opacity)),
                "Expected titlebar opacity {opacity}, observed {effective}"
            );
            ensure!(
                hidden == (opacity == 0.0),
                "Native titlebar hidden state did not follow opacity"
            );
            println!("opacity={opacity:.1} effective={effective:.3} hidden={hidden} window_alpha={:.1} content_alpha={:.1}", native.alphaValue(), content.alphaValue());
            self.phase += 1;
            Ok(self.phase == 4)
        }
    }

    impl ApplicationHandler for Probe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let result = (|| -> anyhow::Result<()> {
                let window = event_loop.create_window(window_chrome::attributes(
                    Window::default_attributes()
                        .with_title("Benga titlebar probe")
                        .with_inner_size(LogicalSize::new(800.0, 500.0))
                        .with_visible(false),
                ))?;
                let chrome = Chrome::new(&window)?;
                let (native, content) = views(&window)?;
                let rect = native.contentLayoutRect();
                println!("logical_titlebar_height={:.1}, content_height={:.1}, native_layout_height={:.1}", chrome.height(), content.bounds().size.height, rect.size.height);
                for (name, kind) in [
                    ("close", NSWindowButton::CloseButton),
                    ("minimize", NSWindowButton::MiniaturizeButton),
                    ("zoom", NSWindowButton::ZoomButton),
                ] {
                    if let Some(button) = native.standardWindowButton(kind) {
                        let frame = button.convertRect_toView(button.bounds(), None);
                        println!(
                            "{name}: x={:.1}, y={:.1}, width={:.1}, height={:.1}",
                            frame.origin.x, frame.origin.y, frame.size.width, frame.size.height
                        );
                    }
                }
                ensure!(
                    chrome.height().is_finite() && (28.0..=128.0).contains(&chrome.height()),
                    "Invalid native titlebar height"
                );
                self.window = Some(window);
                self.chrome = Some(chrome);
                self.next = Instant::now() + Duration::from_millis(200);
                Ok(())
            })();
            if let Err(error) = result {
                self.failure = Some(error);
                event_loop.exit();
            }
        }

        fn window_event(
            &mut self,
            _event_loop: &ActiveEventLoop,
            _window: WindowId,
            _event: WindowEvent,
        ) {
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if self.phase == 4 || self.failure.is_some() {
                event_loop.exit();
                return;
            }
            let now = Instant::now();
            if now >= self.deadline {
                self.failure = Some(anyhow::anyhow!("Native titlebar probe timed out"));
                event_loop.exit();
                return;
            }
            if self.window.is_some() && now >= self.next {
                match self.step() {
                    Ok(true) => {
                        event_loop.exit();
                        return;
                    }
                    Ok(false) => self.next = now + Duration::from_millis(200),
                    Err(error) => {
                        self.failure = Some(error);
                        event_loop.exit();
                        return;
                    }
                }
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.next.min(self.deadline)));
        }
    }

    fn views(window: &Window) -> anyhow::Result<(Retained<NSWindow>, Retained<NSView>)> {
        let handle = window.window_handle()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            anyhow::bail!("Expected an AppKit window");
        };
        // SAFETY: this probe owns the window and accesses its retained NSView
        // only from winit's main-thread application callbacks.
        let content = unsafe { Retained::retain(handle.ns_view.cast::<NSView>().as_ptr()) }
            .context("Probe content view is alive")?;
        let native = content
            .window()
            .context("Probe content belongs to its window")?;
        Ok((native, content))
    }

    fn effective_alpha(view: &NSView) -> f64 {
        let mut alpha = view.alphaValue();
        // SAFETY: all ancestors are retained while this live window is owned
        // by the probe; this runs exclusively on the AppKit thread.
        let mut ancestor = unsafe { view.superview() };
        while let Some(view) = ancestor {
            alpha *= view.alphaValue();
            ancestor = unsafe { view.superview() };
        }
        alpha
    }

    fn near(left: f64, right: f64) -> bool {
        (left - right).abs() < 1e-6
    }

    pub fn run() -> anyhow::Result<()> {
        let mut probe = Probe {
            window: None,
            chrome: None,
            phase: 0,
            next: Instant::now(),
            deadline: Instant::now() + Duration::from_secs(5),
            failure: None,
        };
        EventLoop::new()?.run_app(&mut probe)?;
        if let Some(error) = probe.failure {
            return Err(error);
        }
        ensure!(
            probe.phase == 4,
            "Native probe did not complete every opacity phase"
        );
        println!("Native titlebar metrics and opacity probe passed.");
        Ok(())
    }
}
