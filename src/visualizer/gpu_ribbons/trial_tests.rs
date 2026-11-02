//! GPU history regression trials. Readback is confined to this ignored test.
use std::sync::{mpsc, Arc};

use vello::{
    kurbo::Size,
    util::{DeviceHandle, RenderContext},
    wgpu,
};

use super::*;
use crate::visualizer::{Frame, Renderer};

fn model(topology: &Arc<GpuTopology>, color: [f32; 3], reset: u64) -> GpuModelFrame {
    GpuModelFrame {
        topology: Arc::clone(topology),
        projected: vec![],
        projection: None,
        styles: vec![[color[0], color[1], color[2], 0.8]],
        width: 3.0,
        motion_fade_power: 0.5,
        motion_opacity: 1.0,
        at: 1.0,
        reset,
        durations: [0.02, 0.02],
    }
}

fn pose(model: &mut GpuModelFrame, x: f32, at: f64) {
    model.projected = vec![[x, 16.0, 1.0, 1.0], [x, 48.0, 1.0, 1.0]];
    model.at = at;
}

fn capture(
    device: &DeviceHandle,
    renderer: &mut Renderer,
    models: &[GpuModelFrame],
    size: [u32; 2],
) -> anyhow::Result<Vec<u8>> {
    let [width, height] = size;
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
        "History trial background",
        wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    let target = texture(
        "History trial display",
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    device.queue.write_texture(
        background.as_image_copy(),
        &[0, 0, 0, 255].repeat((width * height) as usize),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        background.size(),
    );
    let row = (width * 4).div_ceil(256) * 256;
    let readback = device.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("History trial diagnostic readback"),
        size: u64::from(row * height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.device.create_command_encoder(&Default::default());
    renderer.draw(
        device,
        &mut encoder,
        &background.create_view(&Default::default()),
        &target.create_view(&Default::default()),
        Size::new(f64::from(width), f64::from(height)),
        Frame {
            lines: &[],
            gpu_models: models,
            model_composite: ModelComposite::Additive,
        },
        None,
        None,
        1.0,
        None,
    );
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
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
    let pixels = readback
        .slice(..)
        .get_mapped_range()?
        .chunks(row as usize)
        .flat_map(|row| row[..width as usize * 4].iter().copied())
        .collect();
    readback.unmap();
    Ok(pixels)
}

fn red(pixels: &[u8], width: usize, x: usize, y: usize) -> u8 {
    pixels[(y * width + x) * 4]
}

#[test]
#[ignore = "requires a compute-capable GPU"]
fn projected_history_survives_exchange_and_reseeds_without_stale_motion() -> anyhow::Result<()> {
    let mut context = RenderContext::new();
    let id = pollster::block_on(crate::render::prepare_device(
        &mut context,
        None,
        wgpu::DeviceDescriptor::default(),
    ))?;
    let device = &context.devices[id];
    assert!(supported(&device.device) && adapter_supported(device.adapter()));
    let topology = Arc::new(GpuTopology {
        vertices: Vec::new(),
        bounds: [None; 2],
        vertex_count: 2,
        edges: vec![[0, 1]],
    });
    let mut renderer = Renderer::new(&device.device, wgpu::TextureFormat::Rgba8Unorm);
    let mut reference = None;
    // Replaying identical sampled poses must produce identical blend order and
    // pixels. Exchange current/outgoing order while preserving both histories.
    for generation in [100, 200] {
        let mut a = model(&topology, [1.0, 0.0, 0.0], generation);
        let mut b = model(&topology, [0.0, 1.0, 0.0], generation + 1);
        pose(&mut a, 8.0, 1.0);
        capture(device, &mut renderer, std::slice::from_ref(&a), [64, 64])?;
        pose(&mut a, 40.0, 1.01);
        pose(&mut b, 56.0, 1.01);
        capture(device, &mut renderer, &[a, b], [64, 64])?;
        // Actual middle pose is x=40 even though f-2 and f0 both have x=8.
        let mut a = model(&topology, [1.0, 0.0, 0.0], generation);
        let mut b = model(&topology, [0.0, 1.0, 0.0], generation + 1);
        pose(&mut a, 8.0, 1.04);
        a.durations = [0.01, 0.03];
        pose(&mut b, 56.0, 1.04);
        b.durations = [0.01, 0.03];
        let pixels = capture(device, &mut renderer, &[b, a], [64, 64])?;
        assert!(
            red(&pixels, 64, 40, 32) > 10,
            "the actual middle pose must survive current/outgoing exchange"
        );
        assert!(
            pixels[(32 * 64 + 56) * 4 + 1] > 100,
            "outgoing history must not contaminate the green model"
        );
        if let Some(reference) = &reference {
            assert_eq!(
                &pixels, reference,
                "repeated captures must be deterministic"
            );
        } else {
            reference = Some(pixels);
        }
    }
    // A newly hidden pose must not erase the preceding visible exposure.
    let mut a = model(&topology, [1.0, 0.0, 0.0], 200);
    a.at = 1.06;
    a.projected = vec![[0.0; 4]; 2];
    let pixels = capture(device, &mut renderer, std::slice::from_ref(&a), [64, 64])?;
    assert!(
        red(&pixels, 64, 25, 32) > 0,
        "historical visible motion survives near-plane rejection"
    );
    // Switching away and back uses a new generation from the application.
    capture(device, &mut renderer, &[], [64, 64])?;
    a.reset = 300;
    pose(&mut a, 8.0, 1.08);
    let pixels = capture(device, &mut renderer, std::slice::from_ref(&a), [64, 64])?;
    assert_eq!(
        red(&pixels, 64, 25, 32),
        0,
        "switching back must reseed history"
    );
    assert!(red(&pixels, 64, 8, 32) > 100);
    pose(&mut a, 40.0, 1.10);
    capture(device, &mut renderer, std::slice::from_ref(&a), [64, 64])?;
    pose(&mut a, 8.0, 1.12);
    let pixels = capture(device, &mut renderer, std::slice::from_ref(&a), [128, 64])?;
    assert_eq!(
        red(&pixels, 128, 25, 32),
        0,
        "resize must reseed screen-space history"
    );
    assert!(red(&pixels, 128, 8, 32) > 100);
    // A backward timeline jump independently reseeds even without a generation
    // change, so the backend cannot connect distant animation moments.
    pose(&mut a, 40.0, 1.14);
    capture(device, &mut renderer, std::slice::from_ref(&a), [128, 64])?;
    pose(&mut a, 8.0, 0.5);
    let pixels = capture(device, &mut renderer, std::slice::from_ref(&a), [128, 64])?;
    assert_eq!(
        red(&pixels, 128, 25, 32),
        0,
        "timeline discontinuity must reseed history"
    );
    Ok(())
}

#[test]
#[ignore = "requires a compute-capable GPU"]
fn unequal_frame_durations_weight_the_two_actual_intervals() -> anyhow::Result<()> {
    let mut context = RenderContext::new();
    let id = pollster::block_on(crate::render::prepare_device(
        &mut context,
        None,
        wgpu::DeviceDescriptor::default(),
    ))?;
    let device = &context.devices[id];
    let topology = Arc::new(GpuTopology {
        vertices: Vec::new(),
        bounds: [None; 2],
        vertex_count: 2,
        edges: vec![[0, 1]],
    });
    let mut renderer = Renderer::new(&device.device, wgpu::TextureFormat::Rgba8Unorm);
    let mut captures = vec![];
    for (generation, durations) in [(400, [0.02, 0.02]), (500, [0.01, 0.03])] {
        let mut a = model(&topology, [1.0, 0.0, 0.0], generation);
        let mut pixels = vec![];
        for (index, at) in [
            1.0,
            1.0 + f64::from(durations[0]),
            1.0 + f64::from(durations[0] + durations[1]),
        ]
        .into_iter()
        .enumerate()
        {
            let y = 16.0 + 8.0 * index as f32;
            a.projected = vec![[20.0, y, 1.0, 1.0], [20.0, y + 16.0, 1.0, 1.0]];
            a.at = at;
            a.durations = durations;
            pixels = capture(device, &mut renderer, std::slice::from_ref(&a), [64, 64])?;
        }
        captures.push(pixels);
    }
    let old_equal = red(&captures[0], 64, 20, 18);
    let old_weighted = red(&captures[1], 64, 20, 18);
    let new_equal = red(&captures[0], 64, 20, 46);
    let new_weighted = red(&captures[1], 64, 20, 46);
    assert!(old_equal > 0 && new_equal > 0);
    assert!(
        (f32::from(old_weighted) - f32::from(old_equal) * 0.5).abs() <= 2.0,
        "older interval must receive its actual quarter-duration share"
    );
    assert!(
        (f32::from(new_weighted) - f32::from(new_equal) * 1.5).abs() <= 2.0,
        "newer interval must receive its actual three-quarter-duration share"
    );
    Ok(())
}

/// Diagnostic float capture without the desktop surface conversion. Comparing
/// the raw half-float attachment also catches changes that an 8-bit PNG hides.
pub(super) fn capture_parallel_float(
    device: &DeviceHandle,
    renderer: &mut GpuRibbonRenderer,
    frame: &GpuModelFrame,
    composite: ModelComposite,
) -> anyhow::Result<Vec<u8>> {
    capture_models_float(device, renderer, std::slice::from_ref(frame), composite)
}

pub(super) fn capture_models_float(
    device: &DeviceHandle,
    renderer: &mut GpuRibbonRenderer,
    frames: &[GpuModelFrame],
    composite: ModelComposite,
) -> anyhow::Result<Vec<u8>> {
    let size = [128_u32, 128_u32];
    let target = device.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Parallel support float comparison"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let readback = device.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Parallel support diagnostic float readback"),
        size: u64::from(size[0] * size[1] * 8),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let view = target.create_view(&Default::default());
    let mut encoder = device.device.create_command_encoder(&Default::default());
    renderer.prepare(
        &device.device,
        &device.queue,
        &mut encoder,
        size,
        size,
        frames,
    );
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Parallel support float image"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(match composite {
                        ModelComposite::Additive => wgpu::Color::BLACK,
                        ModelComposite::Multiplicative => wgpu::Color::WHITE,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        renderer.draw(&mut pass, composite);
    }
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size[0] * 8),
                rows_per_image: Some(size[1]),
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
    rx.recv()??;
    let pixels = readback.slice(..).get_mapped_range()?.to_vec();
    readback.unmap();
    Ok(pixels)
}

#[test]
#[ignore = "requires a compute-capable GPU; compares original/tight parallel support in float"]
fn tight_parallel_support_preserves_float_pixels() -> anyhow::Result<()> {
    let mut context = RenderContext::new();
    let id = pollster::block_on(crate::render::prepare_device(
        &mut context,
        None,
        wgpu::DeviceDescriptor::default(),
    ))?;
    let device = &context.devices[id];
    assert!(supported(&device.device) && adapter_supported(device.adapter()));
    let renderer = |tight| GpuRibbonRenderer {
        projection_pipeline: ProjectionPipeline::new(&device.device),
        pipelines: Arc::new(RibbonPipelines::with_parallel_support(
            &device.device,
            tight,
        )),
        topology: vec![],
        slots: vec![],
        retired: vec![],
    };
    let mut original = renderer(false);
    let mut tight = renderer(true);
    let mut generation = 1000;
    // Diagonal, horizontal, vertical, viewport-crossing and nearly shortened
    // strokes. Across movement remains small enough for parallel classification.
    for (name, a, b, travel, across_travel) in [
        ("diagonal", [18.0, 18.0], [75.0, 93.0], 24.0, 0.0),
        ("horizontal", [12.0, 62.0], [90.0, 62.0], 21.0, 0.15),
        ("vertical", [64.0, 12.0], [64.0, 80.0], 19.0, 0.1),
        ("viewport", [-130.0, -100.0], [260.0, 290.0], 95.0, 0.05),
        ("shortened", [34.0, 68.0], [36.0, 69.0], 39.0, 0.0),
        (
            "large",
            [-70000.0, -70000.0],
            [70000.0, 70000.0],
            120.0,
            0.0,
        ),
    ] {
        for width in [0.2_f32, 0.7, 3.0, 9.5] {
            for dense in [false, true] {
                let copies = if dense { 48 } else { 1 };
                let topology = Arc::new(GpuTopology {
                    vertices: Vec::new(),
                    bounds: [None; 2],
                    vertex_count: 2 * copies,
                    edges: (0..copies as u32).map(|i| [i * 2, i * 2 + 1]).collect(),
                });
                for composite in [ModelComposite::Additive, ModelComposite::Multiplicative] {
                    generation += 1;
                    let mut frame = GpuModelFrame {
                        topology: Arc::clone(&topology),
                        projected: vec![],
                        projection: None,
                        styles: (0..copies)
                            .map(|i| {
                                [
                                    0.2 + (i % 3) as f32 * 0.2,
                                    0.5,
                                    0.8,
                                    if dense {
                                        0.00013 + i as f32 * 0.00011
                                    } else {
                                        0.7
                                    },
                                ]
                            })
                            .collect(),
                        width,
                        motion_fade_power: 0.5,
                        motion_opacity: 0.01,
                        at: 1.0,
                        reset: generation,
                        durations: [0.01, 0.03],
                    };
                    let delta: [f32; 2] = [b[0] - a[0], b[1] - a[1]];
                    let length = (delta[0] * delta[0] + delta[1] * delta[1]).sqrt();
                    let along = [delta[0] / length, delta[1] / length];
                    let across = [-along[1], along[0]];
                    for pose in 0..3 {
                        frame.at = 1.0 + f64::from(pose) * 0.02;
                        frame.projected.clear();
                        for index in 0..copies {
                            let shift = index as f32 * 0.35;
                            for (endpoint, point) in [a, b].into_iter().enumerate() {
                                let along_shift = travel * pose as f32;
                                let across_shift = shift + across_travel * pose as f32;
                                frame.projected.push([
                                    point[0] + along[0] * along_shift + across[0] * across_shift,
                                    point[1] + along[1] * along_shift + across[1] * across_shift,
                                    0.12 + endpoint as f32 * 0.77 - pose as f32 * 0.02,
                                    1.0,
                                ]);
                            }
                        }
                        let expected =
                            capture_parallel_float(device, &mut original, &frame, composite)?;
                        let actual = capture_parallel_float(device, &mut tight, &frame, composite)?;
                        assert_eq!(actual, expected,
                            "parallel float support changed: {name}, width={width}, dense={dense}, composite={composite:?}, pose={pose}");
                    }
                }
            }
        }
    }
    Ok(())
}
