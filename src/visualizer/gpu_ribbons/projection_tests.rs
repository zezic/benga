//! GPU source projection parity and history. Readback is confined to this test.
use std::sync::{mpsc, Arc};

use vello::util::{DeviceHandle, RenderContext};

use super::*;
use crate::visualizer::{
    geometry::{Camera, DepthOpacity, FocusProjection, Pose, Vec2, Vec3, Wireframe},
    gpu_bridge::GpuProjection,
};

fn prepare_and_read(
    device: &DeviceHandle,
    renderer: &mut GpuRibbonRenderer,
    frame: &GpuModelFrame,
    viewport: [u32; 2],
    extent: [u32; 2],
) -> anyhow::Result<Vec<[f32; 4]>> {
    let bytes = frame.topology.vertex_count as u64 * 3 * 16;
    let readback = device.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Projection parity history readback"),
        size: bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.device.create_command_encoder(&Default::default());
    renderer.prepare(
        &device.device,
        &device.queue,
        &mut encoder,
        viewport,
        extent,
        std::slice::from_ref(frame),
    );
    encoder.copy_buffer_to_buffer(&renderer.slots[0].history, 0, &readback, 0, bytes);
    device.queue.submit([encoder.finish()]);
    let (tx, rx) = mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    device.device.poll(wgpu::PollType::wait_indefinitely())?;
    rx.recv()??;
    let records = bytemuck::cast_slice(&readback.slice(..).get_mapped_range()?).to_vec();
    readback.unmap();
    Ok(records)
}

#[test]
#[ignore = "requires a compute-capable GPU"]
fn gpu_source_projection_matches_production_camera_and_preserves_physical_history(
) -> anyhow::Result<()> {
    let mut context = RenderContext::new();
    let id = pollster::block_on(crate::render::prepare_device(
        &mut context,
        None,
        wgpu::DeviceDescriptor::default(),
    ))?;
    let device = &context.devices[id];
    assert!(supported(&device.device) && adapter_supported(device.adapter()));
    let vertices = vec![
        Vec3::new(-0.8, -0.7, -0.9),
        Vec3::new(0.9, 0.6, 0.8),
        Vec3::new(-0.4, 0.9, 0.2),
        Vec3::new(0.7, -0.8, -0.3),
        Vec3::new(0.1, 0.2, -0.8),
        Vec3::new(-0.2, -0.1, 0.6),
    ];
    let wireframe = Wireframe::from_parts(
        vertices,
        vec![[0, 1], [2, 3], [4, 5]],
        vec![0, 0, 1, 1, 0, 1],
        vec![0, 1, 0],
        2,
    );
    let topology =
        GpuTopology::with_slide_vertices(&wireframe, &[false, false, true, true, false, true]);
    let mut frame = GpuModelFrame::new(Arc::clone(&topology));
    frame.styles = vec![[0.4, 0.7, 0.9, 0.8]; 3];
    frame.width = 3.0;
    frame.motion_opacity = 1.0;
    let mut renderer = GpuRibbonRenderer::new(&device.device);
    let count = topology.vertex_count;
    let mut previous: Option<Vec<[f32; 4]>> = None;
    for step in 0..8 {
        let viewport = if step < 5 { [5120, 2880] } else { [3840, 2160] };
        // Quality changes never change projection coordinates or clear the ring.
        let extent = if step == 4 {
            [160, 90]
        } else {
            [viewport[0] / 2, viewport[1] / 2]
        };
        let camera = Camera::new(viewport[0] as f32, viewport[1] as f32, 1.7);
        let pose = Pose {
            yaw: 0.4 + step as f32 * 0.13,
            pitch: -0.25,
            roll: 0.17,
            scale: 1.2,
            distance: 4.2,
            shake: Vec2::new(4.5, -3.2),
        };
        let blend = [0.0, 0.3, 1.0, 1.2, 1.2, 0.0, -0.2, 1.0][step];
        let focus = FocusProjection {
            target: Vec3::new(0.13, -0.11, 0.17),
            rotation_yaw: -0.6,
            rotation_pitch: 0.25,
            distance: if step == 3 { 0.12 } else { 1.5 },
            focal: camera.focal * 0.9,
            near: 0.05,
            blend,
        };
        let slide = Vec3::new(0.12, -0.08, 0.14) * step as f32;
        let depth = DepthOpacity::centered(
            if blend == 0.0 {
                pose.distance
            } else {
                focus.distance
            },
            pose.scale,
            0.72,
        );
        frame.projection = Some(GpuProjection::new(camera, pose, focus, depth, slide));
        frame.at = 1.0 + step as f64 * 0.02;
        if step == 6 {
            frame.at = 0.5;
        }
        if step == 7 {
            frame.reset += 1;
        }
        let actual = prepare_and_read(device, &mut renderer, &frame, viewport, extent)?;
        let newest = renderer.slots[0].newest as usize;
        let reseed = step == 0 || step >= 5;
        for index in 0..count {
            let source = topology.vertices[index];
            let world = wireframe.vertex(index) + slide * source[3];
            let cpu = if blend == 0.0 {
                camera.project(world, pose)
            } else {
                camera.focused_projector(pose, focus)(world)
            };
            let expected = if cpu.visible {
                [cpu.screen.x, cpu.screen.y, depth.factor(cpu.depth), 1.0]
            } else {
                [0.0; 4]
            };
            for slot in 0..3 {
                let record = actual[slot * count + index];
                if reseed || slot == newest {
                    assert_eq!(record[3], expected[3], "step {step} vertex {index}");
                    if expected[3] > 0.0 {
                        let tolerance = 0.003 + expected[0].abs().max(expected[1].abs()) * 3e-6;
                        assert!(
                            (record[0] - expected[0]).abs() < tolerance,
                            "x step {step} vertex {index}: {record:?} / {expected:?}"
                        );
                        assert!(
                            (record[1] - expected[1]).abs() < tolerance,
                            "y step {step} vertex {index}: {record:?} / {expected:?}"
                        );
                        assert!((record[2] - expected[2]).abs() < 2e-6);
                    } else {
                        assert_eq!(record, [0.0; 4]);
                    }
                } else {
                    assert_eq!(
                        record,
                        previous.as_ref().unwrap()[slot * count + index],
                        "history slot {slot} changed at step {step}"
                    );
                }
            }
        }
        assert!(
            frame.projected.is_empty(),
            "source projection has no per-frame CPU pose upload"
        );
        previous = Some(actual);
    }
    Ok(())
}

#[test]
#[ignore = "requires a compute-capable GPU"]
fn coarse_targets_preserve_motion_classification_in_physical_pixels() -> anyhow::Result<()> {
    let mut context = RenderContext::new();
    let id = pollster::block_on(crate::render::prepare_device(
        &mut context,
        None,
        wgpu::DeviceDescriptor::default(),
    ))?;
    let device = &context.devices[id];
    assert!(supported(&device.device) && adapter_supported(device.adapter()));
    let wireframe = Wireframe::from_parts(
        vec![Vec3::new(0.0, -1.0, 0.0), Vec3::new(0.0, 1.0, 0.0)],
        vec![[0, 1]],
        vec![0, 0],
        vec![0],
        1,
    );
    let topology = GpuTopology::new(&wireframe);
    let mut renderer = GpuRibbonRenderer::new(&device.device);
    for extent in [[128, 128], [16, 16], [1, 1]] {
        let mut frame = GpuModelFrame::new(Arc::clone(&topology));
        frame.styles = vec![[0.4, 0.7, 0.9, 0.8]];
        frame.width = 3.0;
        frame.motion_opacity = 1.0;
        for (step, x) in [20.0, 40.0, 60.0].into_iter().enumerate() {
            frame.at = 1.0 + step as f64 * 0.02;
            frame.projected = vec![[x, 16.0, 1.0, 1.0], [x, 112.0, 1.0, 1.0]];
            let mut encoder = device.device.create_command_encoder(&Default::default());
            renderer.prepare(
                &device.device,
                &device.queue,
                &mut encoder,
                [128, 128],
                extent,
                std::slice::from_ref(&frame),
            );
            device.queue.submit([encoder.finish()]);
        }
        let captured = renderer.benchmark_coverage(device)?;
        let model = &captured["models"][0];
        assert_eq!(
            model["active_parallel_intervals"].as_u64(),
            Some(0),
            "20 physical pixels of normal travel exceeds the 1.5-pixel physical half-width at model extent {extent:?}: {model}"
        );
        assert!(
            model["classes"]["regular"]["active_primitives"]
                .as_u64()
                .is_some_and(|count| count > 0),
            "ordinary triangle support remains active at model extent {extent:?}: {model}"
        );
    }
    Ok(())
}
