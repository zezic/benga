use std::{
    sync::{mpsc, Arc},
    time::Duration,
};

use super::*;
use crate::visualizer::gpu_bridge::{GpuBodyTopology, GpuModelFrame, GpuProjection, GpuTopology};

#[test]
#[ignore = "requires a compute-capable GPU"]
fn presentation_layers_cover_infographics_then_draw_wireframe_and_poi() -> anyhow::Result<()> {
    let mut context = vello::util::RenderContext::new();
    let id = pollster::block_on(crate::render::prepare_device(
        &mut context,
        None,
        wgpu::DeviceDescriptor::default(),
    ))?;
    let device = &context.devices[id];
    let (width, height) = (64, 16);
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let texture = |label| {
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
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    };
    let background = texture("Layer test background");
    let main = texture("Layer test infographics");
    let poi = texture("Layer test POI");
    let controls = texture("Layer test controls");
    let target = texture("Layer test output");
    let [background_view, main_view, poi_view, controls_view, target_view] =
        [&background, &main, &poi, &controls, &target]
            .map(|texture| texture.create_view(&Default::default()));
    let upload = |texture: &wgpu::Texture, pixels: &[u8]| {
        device.queue.write_texture(
            texture.as_image_copy(),
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            texture.size(),
        )
    };
    upload(&main, &[0, 255, 0, 255].repeat((width * height) as usize));
    let mut poi_pixels = vec![0; (width * height * 4) as usize];
    let mut control_pixels = poi_pixels.clone();
    for y in 0..height {
        for x in 39..43 {
            let i = ((y * width + x) * 4) as usize;
            poi_pixels[i..i + 4].copy_from_slice(&[0, 0, 255, 255]);
        }
        for x in 55..59 {
            let i = ((y * width + x) * 4) as usize;
            control_pixels[i..i + 4].copy_from_slice(&[255, 255, 0, 255]);
        }
    }
    upload(&poi, &poi_pixels);
    upload(&controls, &control_pixels);
    let body = Arc::new(GpuBodyTopology {
        vertices: vec![
            [16.0, 0.0, 0.0, 0.0],
            [48.0, 0.0, 0.0, 0.0],
            [48.0, -16.0, 0.0, 0.0],
            [16.0, -16.0, 0.0, 0.0],
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
    });
    let mut model = GpuModelFrame::new(Arc::new(GpuTopology {
        body: Some(body),
        edges: vec![[0, 1]],
        vertex_count: 2,
        vertices: vec![[32.0, -3.0, 0.0, 0.0], [32.0, -13.0, 0.0, 0.0]],
        bounds: [None; 2],
    }));
    model.projection = Some(GpuProjection {
        rows: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ],
        camera: [0.0, 0.0, 1.0, 0.1],
        depth: [1.0, 1.0, 1.0, 0.0],
        slide: [0.0; 4],
    });
    model.styles = vec![[1.0, 0.0, 0.0, 1.0]];
    model.width = 4.0;
    let readback = device.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Layer test pixels"),
        size: u64::from(width * height * 4),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut renderer = Renderer::new(&device.device, format);
    renderer.set_fixed_ribbon_downsize(Some(2.0));
    for composite in [ModelComposite::Additive, ModelComposite::Multiplicative] {
        let bg: [u8; 4] = match composite {
            ModelComposite::Additive => [26, 51, 77, 255],
            ModelComposite::Multiplicative => [200, 220, 240, 255],
        };
        upload(&background, &bg.repeat((width * height) as usize));
        for show_model in [true, false] {
            let mut encoder = device.device.create_command_encoder(&Default::default());
            renderer.draw(
                device,
                &mut encoder,
                &background_view,
                &target_view,
                Size::new(f64::from(width), f64::from(height)),
                Frame {
                    lines: &[],
                    gpu_models: if show_model {
                        std::slice::from_ref(&model)
                    } else {
                        &[]
                    },
                    model_composite: composite,
                },
                Some(&main_view),
                Some(composite),
                1.0,
                Some(&poi_view),
                Some(&controls_view),
            );
            encoder.copy_texture_to_buffer(
                target.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(width * 4),
                        rows_per_image: Some(height),
                    },
                },
                target.size(),
            );
            device.queue.submit([encoder.finish()]);
            let (tx, rx) = mpsc::channel();
            readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = tx.send(result);
                });
            device.device.poll(wgpu::PollType::wait_indefinitely())?;
            rx.recv_timeout(Duration::from_secs(30))??;
            let pixels = readback.slice(..).get_mapped_range()?;
            let pixel = |x| {
                let i = ((8 * width + x) * 4) as usize;
                &pixels[i..i + 4]
            };
            let main_color = match composite {
                ModelComposite::Additive => [bg[0], 255, bg[2], 255],
                ModelComposite::Multiplicative => [0, bg[1], 0, 255],
            };
            assert_eq!(
                pixel(8),
                main_color,
                "main infographic remains outside silhouette"
            );
            assert_eq!(
                pixel(24),
                if show_model { bg } else { main_color },
                "body uses original background and clears after model removal"
            );
            if show_model {
                let wire = pixel(32);
                match composite {
                    ModelComposite::Additive => {
                        assert!(wire[0] > 240 && wire[1].abs_diff(bg[1]) < 2)
                    }
                    ModelComposite::Multiplicative => {
                        assert!(wire[0].abs_diff(bg[0]) < 2 && wire[1] < 2)
                    }
                }
            }
            assert_eq!(
                pixel(40),
                [0, 0, 255, 255],
                "POI uses normal alpha above wireframe"
            );
            assert_eq!(
                pixel(56),
                [255, 255, 0, 255],
                "controls remain above every presentation layer"
            );
            drop(pixels);
            readback.unmap();
        }
    }
    Ok(())
}
