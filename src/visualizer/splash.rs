//! The startup logo is vector artwork, recolored with the live app palette.

use std::time::{Duration, Instant};

use vello::{
    kurbo::{Affine, Rect},
    peniko::{Color, Fill, Mix},
    Scene,
};

use crate::{
    logo::Logo,
    text::{Fonts, Text, UiFace},
    theme::{mix, ThemeColors},
};

use super::ui::View;

const HOLD: Duration = Duration::from_secs(2);
const FADE: Duration = Duration::from_millis(500);
pub(super) struct Splash {
    logo: Logo,
    version: Text,
    started: Option<Instant>,
    finished: bool,
}

impl Splash {
    pub fn new(fonts: &mut Fonts) -> anyhow::Result<Self> {
        Ok(Self {
            logo: Logo::new()?,
            version: fonts.layout_ui(concat!("v", env!("CARGO_PKG_VERSION")), 9.0, UiFace::Light),
            started: None,
            finished: false,
        })
    }

    pub fn dismiss(&mut self) {
        self.finished = true;
    }

    pub fn visible(&self) -> bool {
        !self.finished
    }

    pub fn titlebar_logo_opacity(&self, now: Instant) -> f32 {
        if self.finished {
            return 1.0;
        }
        let Some(started) = self.started else {
            return 0.0;
        };
        let progress = (now
            .saturating_duration_since(started)
            .saturating_sub(HOLD)
            .as_secs_f32()
            / FADE.as_secs_f32())
        .clamp(0.0, 1.0);
        progress * progress * (3.0 - 2.0 * progress)
    }

    /// Time starts on the first drawable frame, so loading never consumes the
    /// two seconds during which the logo should be visible. No thread sleeps.
    fn opacity(&mut self, now: Instant) -> Option<f32> {
        if self.finished {
            return None;
        }
        let started = *self.started.get_or_insert(now);
        let elapsed = now.saturating_duration_since(started);
        if elapsed >= HOLD + FADE {
            self.finished = true;
            return None;
        }
        Some(1.0 - self.titlebar_logo_opacity(now))
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        view: &View,
        colors: ThemeColors,
        ink: Color,
        now: Instant,
    ) -> bool {
        let Some(opacity) = self.opacity(now) else {
            return false;
        };
        let screen = Rect::from_origin_size((0.0, 0.0), view.viewport);
        // Fade the whole composition together so overlapping logo fills never
        // show through one another during the transition to the visualizer.
        scene.push_layer(
            Fill::NonZero,
            Mix::Normal,
            opacity,
            Affine::IDENTITY,
            &screen,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            colors.background.with_alpha(0.9),
            None,
            &screen,
        );

        let bounds = self.logo.bounds();
        let dpi = view.ui_scale();
        let width = (140.0 * dpi).min(view.viewport.width * (0.65 / 3.0));
        let scale = (width / bounds.width()).min(view.viewport.height * 0.14 / bounds.height());
        let width = bounds.width() * scale;
        let height = bounds.height() * scale;
        let label_scale = dpi.min(width / self.version.width());
        let label_gap = 12.0 * label_scale;
        let x = (view.viewport.width - width) / 2.0;
        let y = (view.viewport.height - height - label_gap) / 2.0;
        self.logo.draw(
            scene,
            Rect::new(x, y, x + width, y + height),
            Affine::IDENTITY,
            colors.background,
            ink,
        );
        self.version.draw_left(
            scene,
            Affine::translate((
                x + width - self.version.width() * label_scale,
                y + height + label_gap,
            )) * Affine::scale(label_scale),
            mix(colors.background, ink, 0.65),
        );
        scene.pop_layer();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_holds_then_fades_and_never_restarts() -> anyhow::Result<()> {
        let mut fonts = Fonts::new()?;
        let mut splash = Splash::new(&mut fonts)?;
        let now = Instant::now();
        assert_eq!(splash.opacity(now), Some(1.0));
        assert_eq!(splash.opacity(now + HOLD), Some(1.0));
        assert_eq!(splash.opacity(now + HOLD + FADE / 2), Some(0.5));
        assert_eq!(splash.opacity(now + HOLD + FADE), None);
        assert_eq!(splash.opacity(now + Duration::from_secs(20)), None);
        Ok(())
    }

    #[test]
    fn titlebar_logo_crossfades_only_after_the_splash_hold() -> anyhow::Result<()> {
        let mut splash = Splash::new(&mut Fonts::new()?)?;
        let now = Instant::now();
        assert_eq!(splash.titlebar_logo_opacity(now), 0.0);
        for millis in [0, 1000, 2000, 2125, 2250, 2375, 2500, 3000] {
            let at = now + Duration::from_millis(millis);
            let splash_opacity = splash.opacity(at).unwrap_or(0.0);
            let titlebar_opacity = splash.titlebar_logo_opacity(at);
            assert!((splash_opacity + titlebar_opacity - 1.0).abs() < 0.0001);
            if millis <= 2000 {
                assert_eq!(titlebar_opacity, 0.0);
            }
            if millis == 2250 {
                assert_eq!(titlebar_opacity, 0.5);
            }
        }
        let mut skipped = Splash::new(&mut Fonts::new()?)?;
        skipped.dismiss();
        assert_eq!(skipped.titlebar_logo_opacity(now), 1.0);
        Ok(())
    }

    #[test]
    fn splash_can_be_skipped_before_its_first_frame() -> anyhow::Result<()> {
        let mut splash = Splash::new(&mut Fonts::new()?)?;
        splash.dismiss();
        assert_eq!(splash.opacity(Instant::now()), None);
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU; writes splash design previews"]
    fn renders_startup_splash_for_dark_and_light_themes() -> anyhow::Result<()> {
        use crate::theme::{Theme, ThemeId};
        use std::{fs::File, io::BufWriter};
        use vello::{
            kurbo::{Line, Size, Stroke},
            util::RenderContext,
            wgpu, RenderParams,
        };

        let output = std::env::var_os("BENGA_SPLASH_PREVIEW_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("benga-splash-previews"));
        std::fs::create_dir_all(&output)?;
        let width = 1200_u32;
        let height = 750_u32;
        let mut context = RenderContext::new();
        let id = pollster::block_on(crate::render::prepare_device(
            &mut context,
            None,
            wgpu::DeviceDescriptor::default(),
        ))?;
        let device = &context.devices[id];
        let mut renderer = crate::render::renderer(&device.device)?;
        let texture = device.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Startup splash preview"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let texture_view = texture.create_view(&Default::default());
        let row_bytes = width * 4;
        let stride = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        for light in [false, true] {
            let selected = ThemeId::all()
                .find(|theme| !light && theme.name() == "Benga")
                .or_else(|| ThemeId::all().find(|theme| theme.is_light() == light))
                .expect("bundled themes include both tones");
            let theme = Theme::new(selected);
            let mut splash = Splash::new(&mut Fonts::new()?)?;
            let mut scene = Scene::new();
            let view = View::fit(Size::new(f64::from(width), f64::from(height)));
            let ink = super::super::Palette::from_theme(selected).neutral[0];
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                theme.colors.background,
                None,
                &Rect::from_origin_size((0.0, 0.0), view.viewport),
            );
            // A wire cube underneath makes the overlay's transmission visible.
            let vertices = [
                (430.0, 315.0),
                (600.0, 230.0),
                (770.0, 315.0),
                (600.0, 400.0),
                (430.0, 455.0),
                (600.0, 370.0),
                (770.0, 455.0),
                (600.0, 540.0),
            ];
            for (a, b) in [
                (0, 1),
                (1, 2),
                (2, 3),
                (3, 0),
                (4, 5),
                (5, 6),
                (6, 7),
                (7, 4),
                (0, 4),
                (1, 5),
                (2, 6),
                (3, 7),
            ] {
                scene.stroke(
                    &Stroke::new(2.0),
                    Affine::IDENTITY,
                    ink,
                    None,
                    &Line::new(vertices[a], vertices[b]),
                );
            }
            assert!(splash.draw(&mut scene, &view, theme.colors, ink, Instant::now(),));
            renderer.render_to_texture(
                &device.device,
                &device.queue,
                &scene,
                &texture_view,
                &RenderParams {
                    base_color: Color::TRANSPARENT,
                    width,
                    height,
                    antialiasing_method: crate::render::ANTIALIASING,
                },
            )?;
            let buffer = device.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Startup splash readback"),
                size: u64::from(stride * height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = device.device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_buffer(
                texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(stride),
                        rows_per_image: Some(height),
                    },
                },
                texture.size(),
            );
            device.queue.submit([encoder.finish()]);
            let (sender, receiver) = std::sync::mpsc::channel();
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    sender
                        .send(result)
                        .expect("preview readback receiver is live");
                });
            device.device.poll(wgpu::PollType::wait_indefinitely())?;
            receiver.recv()??;
            let mapped = buffer.slice(..).get_mapped_range()?;
            let pixels: Vec<u8> = mapped
                .chunks(stride as usize)
                .flat_map(|row| row[..row_bytes as usize].iter().copied())
                .collect();
            let name = if light {
                "splash-light.png"
            } else {
                "splash-dark.png"
            };
            let mut encoder = png::Encoder::new(
                BufWriter::new(File::create(output.join(name))?),
                width,
                height,
            );
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header()?.write_image_data(&pixels)?;
            drop(mapped);
            buffer.unmap();
        }
        Ok(())
    }
}
