use std::{
    io::BufWriter,
    sync::{mpsc, Arc},
};

use vello::{kurbo::Size, util::RenderContext, wgpu};

use super::{Renderer, TextureBinding};
use crate::visualizer::{
    gpu_bridge::{GpuModelFrame, GpuTopology},
    Frame, ModelComposite,
};

#[test]
#[ignore = "requires a compute-capable GPU"]
fn reduced_ribbon_layer_retains_thin_ink_and_full_resolution_scene() -> anyhow::Result<()> {
    let mut context = RenderContext::new();
    let id = pollster::block_on(crate::render::prepare_device(
        &mut context,
        None,
        wgpu::DeviceDescriptor::default(),
    ))?;
    let device = &context.devices[id];
    assert!(Renderer::gpu_ribbons_supported(&device.device));
    let width = 64;
    let height = 64;
    let texture = |label, usage| {
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
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage,
            view_formats: &[],
        })
    };
    let background = texture(
        "Adaptive test checkerboard",
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );
    let top = texture(
        "Adaptive test one-pixel overlay",
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );
    let target = texture(
        "Adaptive test output",
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let background_view = background.create_view(&Default::default());
    let top_view = top.create_view(&Default::default());
    let target_view = target.create_view(&Default::default());
    let write_texture = |texture: &wgpu::Texture, pixels: &[u8]| {
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
    let mut top_pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height as usize {
        top_pixels[(y * width as usize + 4) * 4..][..4].copy_from_slice(&[0, 255, 0, 255]);
    }
    write_texture(&top, &top_pixels);
    let readback = device.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Adaptive test readback"),
        size: u64::from(width * height * 4),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut model = GpuModelFrame {
        topology: Arc::new(GpuTopology {
            edges: vec![[0, 1]],
            vertex_count: 2,
            vertices: Vec::new(),
            bounds: [None; 2],
        }),
        projected: vec![[31.0, 12.0, 1.0, 1.0], [31.0, 52.0, 1.0, 1.0]],
        projection: None,
        styles: vec![[1.0, 0.0, 0.0, 0.7]],
        width: 1.0,
        motion_fade_power: 0.5,
        motion_opacity: 0.0,
        at: 1.0,
        reset: 1,
        durations: [1.0 / 60.0; 2],
    };
    let directory = std::path::Path::new("target/gpu-ribbon-adaptive/images");
    std::fs::create_dir_all(directory)?;
    let mut renderer = Renderer::new(&device.device, wgpu::TextureFormat::Rgba8Unorm);
    for composite in [ModelComposite::Additive, ModelComposite::Multiplicative] {
        let mut background_pixels = vec![0u8; (width * height * 4) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let base = match composite {
                    ModelComposite::Additive => 45,
                    ModelComposite::Multiplicative => 220,
                };
                let level = base + if (x + y) % 2 == 0 { 0 } else { 20 };
                background_pixels[(y * width as usize + x) * 4..][..4]
                    .copy_from_slice(&[level, level, level, 255]);
            }
        }
        write_texture(&background, &background_pixels);
        for x in [31.0, 32.0, 35.75] {
            let mut baseline_ink = None;
            for downsize in [1.0, 2.0, 8.0] {
                renderer.benchmark_ribbon_downsize(Some(downsize));
                model.projected[0][0] = x;
                model.projected[1][0] = x;
                model.reset += 1;
                let mut encoder = device.device.create_command_encoder(&Default::default());
                renderer.draw(
                    device,
                    &mut encoder,
                    &background_view,
                    &target_view,
                    Size::new(f64::from(width), f64::from(height)),
                    Frame {
                        lines: &[],
                        model_lines: &[],
                        ribbons: &[],
                        gpu_models: std::slice::from_ref(&model),
                        gpu_active: true,
                        model_edge_capacity: 1,
                        model_composite: composite,
                    },
                    None,
                    None,
                    1.0,
                    Some(&top_view),
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
                    .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
                device.device.poll(wgpu::PollType::wait_indefinitely())?;
                rx.recv()??;
                let pixels = readback.slice(..).get_mapped_range()?.to_vec();
                readback.unmap();
                assert_eq!(
                    renderer.ribbon_render_extent(),
                    Some([
                        (width as f32 / downsize).ceil() as u32,
                        (height as f32 / downsize).ceil() as u32
                    ])
                );
                for y in 0..height as usize {
                    let offset = (y * width as usize + 4) * 4;
                    assert_eq!(
                        &pixels[offset..offset + 4],
                        &[0, 255, 0, 255],
                        "one-pixel UI line stays crisp"
                    );
                    for x in [0, 1, 60, 61] {
                        let offset = (y * width as usize + x) * 4;
                        assert_eq!(
                            &pixels[offset..offset + 4],
                            &background_pixels[offset..offset + 4],
                            "background retains its physical checkerboard"
                        );
                    }
                }
                let ink: u32 = (16..48)
                    .flat_map(|y| (16..48).map(move |x| (y * width as usize + x) * 4))
                    .map(|offset| {
                        (0..3)
                            .map(|channel| {
                                u32::from(
                                    pixels[offset + channel]
                                        .abs_diff(background_pixels[offset + channel]),
                                )
                            })
                            .sum::<u32>()
                    })
                    .sum();
                if let Some(baseline) = baseline_ink {
                    assert!(
                        ink > baseline / 4 && ink < baseline * 3,
                        "thin ink energy at x={x}, {downsize}x: {ink} vs full {baseline}"
                    );
                } else {
                    assert!(ink > 100, "fixture must contain visible ink");
                    baseline_ink = Some(ink);
                }
                if x == 31.0 {
                    let theme = match composite {
                        ModelComposite::Additive => "additive",
                        ModelComposite::Multiplicative => "multiply",
                    };
                    let output =
                        std::fs::File::create(directory.join(format!("{theme}-{downsize}x.png")))?;
                    let mut png = png::Encoder::new(BufWriter::new(output), width, height);
                    png.set_color(png::ColorType::Rgba);
                    png.set_depth(png::BitDepth::Eight);
                    png.write_header()?.write_image_data(&pixels)?;
                }
            }
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires a compute-capable GPU"]
fn ribbon_upsampling_keeps_nearest_texel_blocks_in_both_composites() -> anyhow::Result<()> {
    let mut context = RenderContext::new();
    let id = pollster::block_on(crate::render::prepare_device(
        &mut context,
        None,
        wgpu::DeviceDescriptor::default(),
    ))?;
    let device = &context.devices[id];
    let renderer = Renderer::new(&device.device, wgpu::TextureFormat::Rgba8Unorm);
    // The second case has noninteger scale in both axes. The single-texel case
    // checks the clamped edge, where no neighbor exists to interpolate with.
    for (source_size, output_size) in [([4, 4], [64, 64]), ([3, 3], [64, 43]), ([1, 1], [64, 43])] {
        let texture = |label: &str, [width, height]: [u32; 2], usage: wgpu::TextureUsages| {
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
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage,
                view_formats: &[],
            })
        };
        let source = texture(
            "Nearest upsample known source texels",
            source_size,
            wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let source_pixels: Vec<[u8; 4]> = (0..source_size[1])
            .flat_map(|y| {
                (0..source_size[0]).map(move |x| {
                    [
                        (32 + (x % 4) * 40) as u8,
                        (16 + (y % 4) * 40) as u8,
                        (24 + ((x + y) % 4) * 40) as u8,
                        255,
                    ]
                })
            })
            .collect();
        device.queue.write_texture(
            source.as_image_copy(),
            bytemuck::cast_slice(&source_pixels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(source_size[0] * 4),
                rows_per_image: Some(source_size[1]),
            },
            source.size(),
        );
        let source_view = source.create_view(&Default::default());
        let binding =
            TextureBinding::new(&device.device, &renderer.background_layout, &source_view);
        let target = texture(
            "Nearest upsample display output",
            output_size,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let target_view = target.create_view(&Default::default());
        let row_bytes = (output_size[0] * 4).div_ceil(256) * 256;
        let readback = device.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Nearest upsample test readback"),
            size: u64::from(row_bytes * output_size[1]),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        for composite in [ModelComposite::Additive, ModelComposite::Multiplicative] {
            let background = match composite {
                ModelComposite::Additive => 32u8,
                ModelComposite::Multiplicative => 192u8,
            };
            let mut encoder = device.device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Nearest upsample production composite"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: f64::from(background) / 255.0,
                                g: f64::from(background) / 255.0,
                                b: f64::from(background) / 255.0,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_pipeline(match composite {
                    ModelComposite::Additive => &renderer.ribbon_upsample_additive,
                    ModelComposite::Multiplicative => &renderer.ribbon_upsample_multiply,
                });
                pass.set_bind_group(0, &binding.bindings, &[]);
                pass.draw(0..3, 0..1);
            }
            encoder.copy_texture_to_buffer(
                target.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(row_bytes),
                        rows_per_image: Some(output_size[1]),
                    },
                },
                target.size(),
            );
            device.queue.submit([encoder.finish()]);
            let (tx, rx) = mpsc::channel();
            readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
            device.device.poll(wgpu::PollType::wait_indefinitely())?;
            rx.recv()??;
            let pixels = readback.slice(..).get_mapped_range()?;
            for y in 0..output_size[1] {
                for x in 0..output_size[0] {
                    let sx = ((u64::from(2 * x + 1) * u64::from(source_size[0]))
                        / u64::from(2 * output_size[0])) as u32;
                    let sy = ((u64::from(2 * y + 1) * u64::from(source_size[1]))
                        / u64::from(2 * output_size[1])) as u32;
                    let texel = source_pixels[(sy * source_size[0] + sx) as usize];
                    let offset = (y * row_bytes + x * 4) as usize;
                    for channel in 0..3 {
                        let expected = match composite {
                            ModelComposite::Additive => background.saturating_add(texel[channel]),
                            ModelComposite::Multiplicative => {
                                ((u32::from(background) * u32::from(texel[channel]) + 127) / 255)
                                    as u8
                            }
                        };
                        assert!(
                            pixels[offset + channel].abs_diff(expected) <= 1,
                            "nearest source texel mismatch: {source_size:?}->{output_size:?}, {composite:?}, pixel {x},{y}, channel {channel}: {} / {expected}",
                            pixels[offset + channel],
                        );
                    }
                    assert_eq!(pixels[offset + 3], 255);
                }
            }
            drop(pixels);
            readback.unmap();
        }
    }
    Ok(())
}
