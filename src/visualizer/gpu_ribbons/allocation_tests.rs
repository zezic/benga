//! Exercise the production two-model exchange and retain buffer handles only
//! for diagnostic identity checks. Playback never reads GPU data back.
use std::{collections::BTreeSet, time::Instant};

use vello::util::RenderContext;

use super::*;
use crate::visualizer::{
    geometry::{Camera, DepthOpacity, FocusProjection, Pose, Vec3, Wireframe},
    gpu_bridge::GpuProjection,
    model::ModelSet,
    CreaseThreshold, Rng,
};

fn frame(topology: Arc<GpuTopology>, reset: u64, at: f64) -> GpuModelFrame {
    let mut frame = GpuModelFrame::new(topology);
    frame.reset = reset;
    frame.at = at;
    frame.styles = vec![[0.2, 0.5, 0.8, 0.15]; frame.topology.edges.len()];
    frame.projection = Some(GpuProjection::new(
        Camera::new(128.0, 128.0, 1.0),
        Pose::default(),
        FocusProjection {
            target: Vec3::default(),
            rotation_yaw: 0.0,
            rotation_pitch: 0.0,
            distance: 4.0,
            focal: 128.0,
            near: 0.4,
            blend: 0.0,
        },
        DepthOpacity::centered(4.0, 1.0, 0.5),
        Vec3::default(),
    ));
    frame
}

#[test]
#[ignore = "requires a compute-capable GPU; measures real model-swap allocations"]
fn gpu_model_swaps_reuse_two_high_water_working_sets() -> anyhow::Result<()> {
    let mut models = ModelSet::load(&mut Rng::new(0x5357_4150))?;
    models.prepare(CreaseThreshold::default(), None);
    let mut topology: Vec<_> = models
        .prepared_models()
        .iter()
        .map(|model| Arc::clone(&model.gpu_topology))
        .collect();
    topology.sort_by_key(|model| std::cmp::Reverse((model.edges.len(), model.vertex_count)));
    anyhow::ensure!(topology.len() >= 3, "three bundled models are required");
    // Use three catalog models which fit within the larger of the first two
    // slots, so later swaps must not need larger scratch buffers.
    let first = Arc::clone(&topology[0]);
    let second = &topology[1];
    let third = topology
        .iter()
        .skip(2)
        .find(|model| model.vertex_count <= first.vertex_count)
        .expect("a smaller third model is available");
    let topology = [first, Arc::clone(second), Arc::clone(third)];
    let mut context = RenderContext::new();
    let id = pollster::block_on(crate::render::prepare_device(
        &mut context,
        None,
        wgpu::DeviceDescriptor::default(),
    ))?;
    let device = &context.devices[id];
    assert!(supported(&device.device) && adapter_supported(device.adapter()));
    let extent = [128, 128];
    let texture = device.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Model-swap allocation trial"),
        size: wgpu::Extent3d {
            width: extent[0],
            height: extent[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    let mut renderer = GpuRibbonRenderer::new(&device.device);
    let mut scratch = BTreeSet::new();
    let mut histories = BTreeSet::new();
    let mut prepare_ms = Vec::new();
    let mut completion_ms = Vec::new();
    // A -> incoming B + outgoing A -> B -> incoming C + outgoing B.
    for (step, live) in [&[0][..], &[1, 0], &[1], &[2, 1]].into_iter().enumerate() {
        let frames: Vec<_> = live
            .iter()
            .map(|&index| {
                frame(
                    Arc::clone(&topology[index]),
                    index as u64 + 1,
                    1.0 + step as f64 / 60.0,
                )
            })
            .collect();
        let mut encoder = device.device.create_command_encoder(&Default::default());
        let before = Instant::now();
        renderer.prepare(
            &device.device,
            &device.queue,
            &mut encoder,
            extent,
            extent,
            &frames,
        );
        prepare_ms.push(before.elapsed().as_secs_f64() * 1_000.0);
        for slot in &renderer.slots {
            scratch.insert(slot.ribbons.benchmark_buffers().triangles.clone());
            histories.insert(slot.history.clone());
        }
        assert!(renderer.slots.len() + renderer.retired.len() <= 2);
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Model-swap allocation trial draw"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            renderer.draw(&mut pass, ModelComposite::Additive);
        }
        device.queue.submit([encoder.finish()]);
        device.device.poll(wgpu::PollType::wait_indefinitely())?;
        completion_ms.push(before.elapsed().as_secs_f64() * 1_000.0);
    }
    eprintln!(
        "model exchange edges={:?}, vertices={:?}; unique large scratch allocations={}; prepare_ms={prepare_ms:?}; prepare/draw completion_ms={completion_ms:?}",
        topology.each_ref().map(|model| model.edges.len()),
        topology.each_ref().map(|model| model.vertex_count),
        scratch.len(),
    );
    assert_eq!(
        scratch.len(),
        2,
        "the third model fits the warmed two-slot capacity and must reuse the retired working set"
    );
    assert_eq!(histories.len(), 2, "projection rings must also be reused");
    // A smaller source uses different projection-ring and compaction strides.
    // Compare actual float pixels to a fresh renderer, including its first
    // reseeded pose and subsequent motion. This catches stale high-water tails.
    let small = GpuTopology::new(&Wireframe::from_parts(
        vec![Vec3::new(0.0, -0.7, 0.0), Vec3::new(0.0, 0.7, 0.0)],
        vec![[0, 1]],
        vec![0, 0],
        vec![0],
        1,
    ));
    let mut small_frame = frame(small, 10, 1.0 + 4.0 / 60.0);
    small_frame.styles = vec![[0.3, 0.8, 0.5, 0.7]];
    small_frame.width = 2.0;
    small_frame.motion_opacity = 1.0;
    // Interrupt C+B immediately with the small source+C, preserving C's live
    // history while reusing B's working set for the new incoming source.
    let outgoing_history = renderer.slots[0].history.clone();
    let mut outgoing = frame(Arc::clone(&topology[2]), 3, 1.0 + 3.0 / 60.0);
    let mut fresh = GpuRibbonRenderer::new(&device.device);
    trial_tests::capture_parallel_float(device, &mut fresh, &outgoing, ModelComposite::Additive)?;
    outgoing.at = small_frame.at;
    let frames = [small_frame, outgoing];
    let expected =
        trial_tests::capture_models_float(device, &mut fresh, &frames, ModelComposite::Additive)?;
    let actual = trial_tests::capture_models_float(
        device,
        &mut renderer,
        &frames,
        ModelComposite::Additive,
    )?;
    assert_eq!(
        actual, expected,
        "interrupted exchange preserves outgoing history"
    );
    assert_eq!(renderer.slots[1].history, outgoing_history);
    assert!(renderer.slots.iter().all(|slot| {
        scratch.contains(slot.ribbons.benchmark_buffers().triangles)
            && histories.contains(&slot.history)
    }));
    let [mut small_frame, _] = frames;
    for generation in [10, 20] {
        let mut fresh = GpuRibbonRenderer::new(&device.device);
        small_frame.reset = generation;
        for step in 0..3 {
            let mut projection = frame(Arc::clone(&small_frame.topology), 0, 0.0)
                .projection
                .unwrap();
            projection.camera[0] += step as f32 * 12.0;
            small_frame.projection = Some(projection);
            small_frame.at = generation as f64 + step as f64 / 60.0;
            let expected = trial_tests::capture_parallel_float(
                device,
                &mut fresh,
                &small_frame,
                ModelComposite::Additive,
            )?;
            let actual = trial_tests::capture_parallel_float(
                device,
                &mut renderer,
                &small_frame,
                ModelComposite::Additive,
            )?;
            assert!(
                expected
                    .chunks_exact(8)
                    .any(|pixel| pixel[..6].iter().any(|byte| *byte != 0)),
                "visible fixture"
            );
            assert_eq!(actual, expected, "generation {generation}, pose {step}");
            assert!(renderer.slots.len() + renderer.retired.len() <= 2);
            assert!(
                scratch.contains(renderer.slots[0].ribbons.benchmark_buffers().triangles),
                "a smaller source must reuse existing scratch storage"
            );
        }
    }
    // Empty frames can occur when changing backends. They retire both slots;
    // returning to either source must still reuse buffers and reseed history.
    let mut encoder = device.device.create_command_encoder(&Default::default());
    renderer.prepare(
        &device.device,
        &device.queue,
        &mut encoder,
        extent,
        extent,
        &[],
    );
    assert!(renderer.slots.is_empty());
    assert_eq!(renderer.retired.len(), 2);
    device.queue.submit([encoder.finish()]);
    small_frame.reset += 1;
    small_frame.at += 1.0;
    let mut fresh = GpuRibbonRenderer::new(&device.device);
    let expected = trial_tests::capture_parallel_float(
        device,
        &mut fresh,
        &small_frame,
        ModelComposite::Additive,
    )?;
    let actual = trial_tests::capture_parallel_float(
        device,
        &mut renderer,
        &small_frame,
        ModelComposite::Additive,
    )?;
    assert_eq!(actual, expected, "return from empty frame reseeds history");
    assert!(scratch.contains(renderer.slots[0].ribbons.benchmark_buffers().triangles));
    assert!(histories.contains(&renderer.slots[0].history));
    Ok(())
}
