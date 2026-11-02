//! The production presentation pipeline targeting readable pixels without a window.

use std::{sync::mpsc, time::Duration};

use anyhow::{ensure, Context};
use vello::{kurbo::Size, peniko::Color, util::RenderContext, wgpu, RenderParams, Scene};

use super::{
    BackgroundCache, Frame, PlateMask, PlateMaskImage, RasterAssetCache, SceneTexture,
    SolidBackground, ANTIALIASING,
};
use crate::visualizer::{Renderer as VisualizerRenderer, UiFrame};

const READBACK_TIMEOUT: Duration = Duration::from_secs(30);

/// Reuses the desktop's scene layers, motion blur and infographics without a surface.
pub struct OffscreenRenderer {
    context: RenderContext,
    device_id: usize,
    renderer: vello::Renderer,
    visualizer: VisualizerRenderer,
    background_cache: BackgroundCache,
    solid_background: SolidBackground,
    raster_assets: RasterAssetCache,
    plate_mask: PlateMask,
    background: SceneTexture,
    foreground: Option<SceneTexture>,
    background_ready: bool,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    readback: wgpu::Buffer,
    row_bytes: u32,
    padded_row_bytes: u32,
    params: RenderParams,
}

impl OffscreenRenderer {
    pub fn new(width: u32, height: u32) -> anyhow::Result<Self> {
        let (row_bytes, padded_row_bytes, buffer_bytes) = readback_layout(width, height)?;
        let mut context = RenderContext::new();
        let device_id = pollster::block_on(super::prepare_device(
            &mut context,
            None,
            wgpu::DeviceDescriptor::default(),
        ))?;
        let handle = &context.devices[device_id];
        ensure!(
            VisualizerRenderer::gpu_ribbons_supported(&handle.device)
                && VisualizerRenderer::gpu_ribbons_adapter_supported(handle.adapter()),
            "Benga requires a GPU with compute, indirect drawing and float blending support"
        );
        let limits = handle.device.limits();
        ensure!(
            width <= limits.max_texture_dimension_2d && height <= limits.max_texture_dimension_2d,
            "render size {width}x{height} exceeds the GPU's maximum texture dimension of {}",
            limits.max_texture_dimension_2d
        );
        ensure!(
            buffer_bytes <= limits.max_buffer_size,
            "render size {width}x{height} needs a {buffer_bytes}-byte readback buffer, exceeding the GPU limit of {} bytes",
            limits.max_buffer_size
        );
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let target = handle.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Headless video target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&Default::default());
        let readback = handle.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Headless video readback"),
            size: buffer_bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let background = SceneTexture::new(&handle.device, width, height);
        let mut renderer = super::renderer(&handle.device)?;
        let plate_mask = PlateMask::new(&handle.device, &mut renderer, width, height);
        let visualizer = VisualizerRenderer::new(&handle.device, format);
        Ok(Self {
            context,
            device_id,
            renderer,
            visualizer,
            background_cache: BackgroundCache::new(),
            solid_background: SolidBackground::default(),
            raster_assets: RasterAssetCache::new(),
            plate_mask,
            background,
            foreground: None,
            background_ready: false,
            target,
            target_view,
            readback,
            row_bytes,
            padded_row_bytes,
            params: RenderParams {
                base_color: Color::TRANSPARENT,
                width,
                height,
                antialiasing_method: ANTIALIASING,
            },
        })
    }

    /// Install this mask in [`crate::visualizer::Ui`] before drawing its first frame.
    pub fn plate_mask_image(&self) -> PlateMaskImage {
        self.plate_mask.image()
    }

    pub fn gpu_ribbon_edge_capacity(&self) -> usize {
        let limits = self.context.devices[self.device_id].device.limits();
        (u64::from(limits.max_storage_buffer_binding_size).min(limits.max_buffer_size) / (6 * 64))
            as usize
    }

    /// Rasterizes one presentation frame and returns tightly packed, top-to-bottom RGBA8.
    pub fn render(&mut self, scene: &Scene, frame: UiFrame<'_>) -> anyhow::Result<Vec<u8>> {
        let handle = &self.context.devices[self.device_id];
        if !frame.plate_instances.is_empty() {
            self.plate_mask.render(
                &handle.device,
                &handle.queue,
                frame.plate_instances,
                frame.plate_ink,
            );
            self.renderer
                .mark_override_image_dirty(&self.plate_mask.image().image);
        }
        self.raster_assets
            .prepare(&mut self.renderer, handle, &frame.raster_assets)?;
        let solid_color = if let Frame::Solid(color) = frame.background {
            Some(color)
        } else {
            None
        };
        if solid_color.is_none()
            && (matches!(frame.background, Frame::Rebuilt(_)) || !self.background_ready)
        {
            let background = self.background_cache.compose(
                &mut self.renderer,
                handle,
                frame.background,
                &self.params,
            )?;
            self.renderer.render_to_texture(
                &handle.device,
                &handle.queue,
                background,
                &self.background.view,
                &self.params,
            )?;
            self.background_ready = true;
        }
        let foreground = if frame.foreground {
            Some(SceneTexture::rasterize(
                &mut self.foreground,
                &mut self.renderer,
                handle,
                scene,
                &self.params,
            )?)
        } else {
            None
        };
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        let background_view = if let Some(color) = solid_color {
            self.solid_background
                .prepare(&handle.device, &mut encoder, color, &self.params)
        } else {
            &self.background.view
        };
        self.visualizer.draw(
            handle,
            &mut encoder,
            background_view,
            &self.target_view,
            Size::new(f64::from(self.params.width), f64::from(self.params.height)),
            frame.overlay,
            foreground,
            frame.overlay_composite,
            frame.overlay_opacity,
            None,
        );
        encoder.copy_texture_to_buffer(
            self.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_row_bytes),
                    rows_per_image: Some(self.params.height),
                },
            },
            self.target.size(),
        );
        handle.queue.submit([encoder.finish()]);
        let (tx, rx) = mpsc::channel();
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        let result = (|| {
            handle.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(READBACK_TIMEOUT),
            })?;
            rx.recv_timeout(READBACK_TIMEOUT)
                .context("headless frame readback timed out")??;
            let mapped = self.readback.slice(..).get_mapped_range()?;
            let mut pixels =
                Vec::with_capacity(self.row_bytes as usize * self.params.height as usize);
            for row in mapped.chunks_exact(self.padded_row_bytes as usize) {
                pixels.extend_from_slice(&row[..self.row_bytes as usize]);
            }
            Ok(pixels)
        })();
        // Also cancel a pending map after a timeout, so this renderer can be dropped safely.
        self.readback.unmap();
        result
    }
}

fn readback_layout(width: u32, height: u32) -> anyhow::Result<(u32, u32, u64)> {
    ensure!(
        width > 0 && height > 0,
        "render dimensions must be positive"
    );
    let row_bytes = width.checked_mul(4).context("render width is too large")?;
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_row_bytes = row_bytes
        .checked_add(alignment - 1)
        .map(|bytes| bytes / alignment * alignment)
        .context("render width is too large")?;
    let buffer_bytes = u64::from(padded_row_bytes) * u64::from(height);
    ensure!(
        buffer_bytes <= usize::MAX as u64,
        "render size exceeds this platform's address space"
    );
    Ok((row_bytes, padded_row_bytes, buffer_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visualizer::{Frame as VisualizerFrame, ModelComposite};
    use vello::{
        kurbo::{Affine, Rect},
        peniko::Fill,
    };

    #[test]
    fn invalid_dimensions_fail_before_requesting_a_gpu() {
        assert!(OffscreenRenderer::new(0, 100).is_err());
        assert!(OffscreenRenderer::new(100, 0).is_err());
        assert!(OffscreenRenderer::new(u32::MAX, 100).is_err());
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn offscreen_frames_preserve_rgba_rows_foreground_and_background_changes() -> anyhow::Result<()>
    {
        let (width, height) = (67, 5);
        let mut renderer = OffscreenRenderer::new(width, height)?;
        let mut background = Scene::new();
        let mut foreground = Scene::new();
        fn frame(background: Frame<'_>) -> UiFrame<'_> {
            UiFrame {
                background,
                raster_assets: Vec::new(),
                overlay: VisualizerFrame {
                    lines: &[],
                    model_composite: ModelComposite::Additive,
                    gpu_models: &[],
                },
                foreground: true,
                overlay_composite: None,
                overlay_opacity: 1.0,
                plate_instances: &[],
                plate_ink: Color::BLACK,
            }
        }
        background.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(255, 0, 0),
            None,
            &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
        );
        let first = renderer.render(&foreground, frame(Frame::Rebuilt(&background)))?;
        assert_eq!(first, [255, 0, 0, 255].repeat((width * height) as usize));

        foreground.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(0, 0, 255),
            None,
            &Rect::new(0.0, 0.0, 20.0, f64::from(height)),
        );
        let second = renderer.render(&foreground, frame(Frame::Retained(&background)))?;
        for row in second.chunks_exact(width as usize * 4) {
            assert_eq!(&row[..80], [0, 0, 255, 255].repeat(20));
            assert_eq!(&row[80..], [255, 0, 0, 255].repeat(width as usize - 20));
        }

        background.reset();
        background.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(0, 255, 0),
            None,
            &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
        );
        foreground.reset();
        let third = renderer.render(&foreground, frame(Frame::Rebuilt(&background)))?;
        assert_eq!(third, [0, 255, 0, 255].repeat((width * height) as usize));
        Ok(())
    }

    #[test]
    #[ignore = "requires a compute-capable GPU"]
    fn offscreen_solid_backgrounds_match_vector_backdrops_and_preserve_retained_layers(
    ) -> anyhow::Result<()> {
        let (width, height) = (67, 5);
        let mut renderer = OffscreenRenderer::new(width, height)?;
        let mut vector = Scene::new();
        let mut foreground = Scene::new();
        foreground.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(40, 100, 240, 128),
            None,
            &Rect::new(0.0, 0.0, 20.0, f64::from(height)),
        );
        fn frame(background: Frame<'_>) -> UiFrame<'_> {
            UiFrame {
                background,
                raster_assets: Vec::new(),
                overlay: VisualizerFrame {
                    lines: &[],
                    model_composite: ModelComposite::Additive,
                    gpu_models: &[],
                },
                foreground: true,
                overlay_composite: None,
                overlay_opacity: 1.0,
                plate_instances: &[],
                plate_ink: Color::BLACK,
            }
        }
        for color in [
            Color::from_rgb8(12, 23, 34),
            Color::new([0.31, 0.48, 0.67, 1.0]),
            Color::from_rgb8(240, 230, 220),
            Color::from_rgba8(130, 60, 210, 128),
        ] {
            vector.reset();
            vector.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                color,
                None,
                &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            );
            let expected = renderer.render(&foreground, frame(Frame::Rebuilt(&vector)))?;
            // Repeat to exercise the retained solid texture, then return to the
            // vector cache and back without changing either background's color.
            for background in [
                Frame::Solid(color),
                Frame::Solid(color),
                Frame::Retained(&vector),
                Frame::Solid(color),
            ] {
                let actual = renderer.render(&foreground, frame(background))?;
                assert_eq!(actual.len(), expected.len());
                assert!(actual
                    .iter()
                    .zip(&expected)
                    .all(|(actual, expected)| actual.abs_diff(*expected) <= 1));
            }
        }
        Ok(())
    }
}
