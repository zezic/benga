//! Production GPU source/projection contracts, without allocating a GPU device.
use super::*;
use vello::kurbo::Point;

fn prepared(name: &str) -> PreparedModel {
    let wireframe = Wireframe::from_parts(
        vec![
            Vec3::new(-0.3, -0.2, -0.15),
            Vec3::new(0.3, -0.2, 0.15),
            Vec3::new(-0.3, 0.0, -0.1),
            Vec3::new(0.3, 0.0, 0.1),
            Vec3::new(-0.3, 0.2, -0.05),
            Vec3::new(0.3, 0.2, 0.05),
        ],
        vec![[0, 1], [2, 3], [4, 5]],
        vec![0, 0, 1, 1, 2, 2],
        vec![0, 1, 2],
        3,
    );
    PreparedModel {
        name: Arc::from(name),
        edge_order: transition::EdgeOrder::new(3, transition::model_seed(name)),
        slide_vertices: vec![false, false, true, true, false, false].into(),
        gpu_topology: gpu_bridge::GpuTopology::with_slide_vertices(
            &wireframe,
            &[false, false, true, true, false, false],
        ),
        wireframe: Arc::new(wireframe),
        pois: Arc::from([]),
        poi_vertices: BTreeMap::new(),
        magazine_icon: None,
    }
}

fn visualizer() -> Visualizer {
    let mut visualizer = Visualizer::new();
    // Use a small source model through the production scene/update pipeline.
    visualizer.models = Models::Failed;
    visualizer.scene.show(
        &prepared("bridge fixture"),
        ModelMotion::Swept,
        0.5,
        &StyleTuning::default(),
    );
    visualizer
}

fn step(visualizer: &mut Visualizer, start: f64, end: f64, shutter: f32, viewport: (f32, f32)) {
    visualizer.update(
        AudioInterval::new(AudioTime::ZERO.shifted(start), AudioTime::ZERO.shifted(end)),
        ShutterFrames::try_from(shutter).unwrap(),
        (end - start) as f32,
        &Palette::from_theme(ThemeId::default()),
        viewport,
        1.0,
        1.0,
    );
}

#[test]
fn active_bridge_skips_cpu_tessellation_and_publishes_three_pose_layout_support() {
    let mut visualizer = visualizer();
    let mut points = Vec::new();
    for index in 0..3 {
        step(
            &mut visualizer,
            index as f64 / 60.0,
            (index + 1) as f64 / 60.0,
            2.0,
            (1280.0, 720.0),
        );
        let frame = visualizer.frame();
        assert_eq!(frame.gpu_models.len(), 1);
        let gpu = &frame.gpu_models[0];
        assert!(
            gpu.projected.is_empty(),
            "GPU projection uploads no CPU vertex records"
        );
        assert!(gpu.projection.is_some());
        assert_eq!(gpu.styles.len(), 3);
        points.extend(
            gpu.cpu_projected()
                .iter()
                .filter(|v| v[3] > 0.0)
                .map(|v| Point::new(f64::from(v[0]), f64::from(v[1]))),
        );
        let current = visualizer.scene.current.as_ref().unwrap();
        assert!(
            current.vertices.is_empty(),
            "GPU mode must avoid the CPU vertex sweep"
        );
        for index in 0..6 {
            let anchor = current.projected_vertex(index);
            assert!(anchor.screen.x.is_finite() && anchor.screen.y.is_finite());
        }
        assert!(
            current.current.is_empty() && current.history.is_empty(),
            "GPU mode must skip per-edge CPU geometry/history as well as tessellation"
        );
    }
    let support = visualizer
        .swept_model_bounds()
        .expect("graphs need model support");
    assert!(!points.is_empty());
    assert!(
        points.into_iter().all(|point| support.contains(point)),
        "layout must reserve all three projected poses, including their swept interior"
    );
    assert_eq!(
        visualizer.scene.current.as_ref().unwrap().gpu_bounds.len(),
        3
    );
}

fn showcase() -> ShowcaseFrame {
    ShowcaseFrame {
        focus: showcase::FocusView {
            center: Vec3::default(),
            radius: 0.5,
            orbit_yaw: 0.2,
            orbit_pitch: 0.1,
            orbit_drift_yaw: 0.0,
            dolly_scale: 1.0,
            fov: 0.75,
            blend: 0.3,
            model_motion: 0.0,
            freeze_motion: false,
        },
        highlight_part: None,
        focus_child: None,
        mesh_attenuation: 1.0,
        slide_offset: Vec3::new(0.1, 0.0, 0.0),
        selector: showcase::SelectorView::NONE,
    }
}

fn runtime(model: &PreparedModel) -> ModelRuntime {
    ModelRuntime::new(
        model,
        Pose {
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            scale: 1.0,
            distance: 4.2,
            shake: Vec2::default(),
        },
        Style::default(),
        ModelTint::solid(0.5),
        false,
    )
}

fn runtime_frame(
    runtime: &mut ModelRuntime,
    showcase: &ShowcaseFrame,
    gpu: Option<&mut gpu_bridge::GpuModelFrame>,
) -> Vec<LineInstance> {
    let mut muted = [true; Property::COUNT];
    muted[Property::Line.index()] = false;
    let mut lines = Vec::new();
    runtime.update(
        1.0 / 60.0,
        &Music {
            bands: [0.0; BAND_COUNT],
            raw_bands: [0.0; BAND_COUNT],
            power: 0.0,
            power_level: 0.0,
            bass: 0.0,
            bass_level: 0.0,
            kick: 0.0,
            kick_onset: 0.0,
            snare_onset: 0.0,
            drop_onset: 0.0,
        },
        &Gains::default(),
        &BandRouting::default(),
        &Mixer::default(),
        &PropertyGates {
            muted,
            soloed: [false; Property::COUNT],
        },
        AudioTime::ZERO,
        AudioTime::ZERO.shifted(1.0 / 60.0),
        Camera::new(1280.0, 720.0, 1.3),
        &Palette::from_theme(ThemeId::default()),
        0.7,
        0.6,
        MotionFade {
            opacity: MotionOpacity::default(),
            power: MotionFadePower::default(),
        },
        2.3,
        showcase,
        0.0,
        Vec2::new(3.0, -2.0),
        0.0,
        &mut lines,
        &mut Vec::new(),
        gpu,
    );
    lines
}

#[test]
fn bridge_ink_and_projection_match_cpu_for_focus_slide_selector_highlight_and_dissolve() {
    let model = prepared("ink fixture");
    for scenario in 0..5 {
        let mut view = showcase();
        if scenario == 1 {
            view.selector = showcase::SelectorView {
                selected: Some(1),
                attenuation: 0.23,
                alternate_color: true,
            };
        } else if scenario >= 2 {
            view.highlight_part = Some(0);
            view.mesh_attenuation = 0.17;
            if scenario >= 3 {
                view.focus_child = Some(1);
            }
        }
        let mut cpu = runtime(&model);
        let mut gpu = runtime(&model);
        if scenario == 4 {
            cpu.edge_dissolve.incoming(0.65, 3);
            gpu.edge_dissolve.incoming(0.65, 3);
        }
        let lines = runtime_frame(&mut cpu, &view, None);
        let mut upload = gpu_bridge::GpuModelFrame::new(Arc::clone(&model.gpu_topology));
        assert!(runtime_frame(&mut gpu, &view, Some(&mut upload)).is_empty());
        assert!(upload.projected.is_empty());
        assert!(gpu.vertices.is_empty());
        let projected = upload.cpu_projected();
        assert_eq!(projected.len(), cpu.vertices.len());
        for (projected, source) in projected.iter().zip(&cpu.vertices) {
            assert!((projected[0] - source.screen.x).abs() < 0.001);
            assert!((projected[1] - source.screen.y).abs() < 0.001);
            assert_eq!(projected[3], 1.0);
        }
        let mut actual = lines.iter();
        for (index, edge) in upload.topology.edges.iter().enumerate() {
            let style = upload.styles[index];
            if style[3] == 0.0 {
                continue;
            }
            let line = actual
                .next()
                .expect("every visible styled source edge has a CPU line");
            assert_eq!(&line.start_color[..3], &style[..3]);
            assert_eq!(&line.end_color[..3], &style[..3]);
            assert!((line.start_color[3] - style[3] * projected[edge[0] as usize][2]).abs() < 1e-6);
            assert!((line.end_color[3] - style[3] * projected[edge[1] as usize][2]).abs() < 1e-6);
            assert_eq!(line.style[0], upload.width);
        }
        assert!(actual.next().is_none());
        assert!(!lines.is_empty());
        assert!(
            projected.iter().any(|v| v[2] < 0.99),
            "real depth fading is uploaded"
        );
        if scenario == 4 {
            assert!(
                upload.styles.iter().any(|style| style[3] == 0.0),
                "dissolve removes source edges"
            );
        }
    }
}

#[test]
fn instant_model_swaps_publish_only_the_new_complete_model() {
    let mut visualizer = visualizer();
    step(&mut visualizer, 0.0, 0.02, 2.0, (1280.0, 720.0));
    let old_generation = visualizer.gpu_models[0].reset;
    let next = prepared("instant fixture");
    visualizer.scene.switch(&next, ModelMotion::Swept, 0.6);
    assert!(
        visualizer.scene.outgoing.is_none(),
        "an instant switch must retire the old model before the next frame"
    );
    assert_eq!(visualizer.scene.transition, 1.0);
    let current = visualizer.scene.current.as_ref().unwrap();
    assert_eq!(current.active_edges, next.wireframe.edges().len());
    assert!((0..current.active_edges).all(|index| current.edge_dissolve.alpha(index) == 1.0));
    assert!(current.history.is_empty() && current.current.is_empty());
    assert!(current.gpu_bounds.is_empty());
    step(&mut visualizer, 0.02, 0.04, 2.0, (1280.0, 720.0));
    assert_eq!(visualizer.gpu_models.len(), 1);
    let frame = &visualizer.gpu_models[0];
    assert_ne!(frame.reset, old_generation);
    assert!(Arc::ptr_eq(&frame.topology, &next.gpu_topology));
    assert_eq!(frame.styles.len(), next.wireframe.edges().len());
}

#[test]
fn instant_swaps_reuse_prepared_topology_with_fresh_history() {
    let mut visualizer = visualizer();
    let first_model = prepared("return fixture");
    visualizer.scene.set_prepared(&first_model);
    step(&mut visualizer, 0.0, 0.02, 2.0, (1280.0, 720.0));
    let first_topology = Arc::clone(&visualizer.gpu_models[0].topology);
    let first_generation = visualizer.gpu_models[0].reset;
    let second_model = prepared("second fixture");
    visualizer
        .scene
        .switch(&second_model, ModelMotion::Swept, 0.6);
    step(&mut visualizer, 0.02, 0.04, 2.0, (1280.0, 720.0));
    assert_eq!(visualizer.gpu_models.len(), 1);
    assert_ne!(visualizer.gpu_models[0].reset, first_generation);
    assert!(!Arc::ptr_eq(
        &visualizer.gpu_models[0].topology,
        &first_topology
    ));
    assert!(Arc::ptr_eq(
        &visualizer.gpu_models[0].topology,
        &second_model.gpu_topology
    ));
    let second_generation = visualizer.gpu_models[0].reset;
    visualizer
        .scene
        .switch(&first_model, ModelMotion::Swept, 0.4);
    step(&mut visualizer, 0.04, 0.06, 2.0, (1280.0, 720.0));
    assert!(Arc::ptr_eq(
        &visualizer.gpu_models[0].topology,
        &first_model.gpu_topology
    ));
    assert_eq!(visualizer.gpu_models.len(), 1);
    assert_ne!(visualizer.gpu_models[0].reset, second_generation);
    assert_ne!(visualizer.gpu_models[0].reset, first_generation);
    // A preparation revision gets new immutable topology and a fresh history.
    let revision = prepared("return fixture");
    let previous = visualizer.gpu_models[0].reset;
    visualizer.scene.set_prepared(&revision);
    step(&mut visualizer, 0.06, 0.08, 2.0, (1280.0, 720.0));
    assert!(!Arc::ptr_eq(
        &visualizer.gpu_models[0].topology,
        &first_model.gpu_topology
    ));
    assert_ne!(visualizer.gpu_models[0].reset, previous);
}

#[test]
fn resize_switch_and_timeline_discontinuities_reseed_without_resetting_animation() {
    let mut visualizer = visualizer();
    step(&mut visualizer, 0.0, 0.02, 2.0, (1280.0, 720.0));
    step(&mut visualizer, 0.02, 0.05, 2.0, (1280.0, 720.0));
    step(&mut visualizer, 0.05, 0.09, 2.0, (1280.0, 720.0));
    let durations = visualizer.gpu_models[0].durations;
    assert!((durations[0] - 0.03).abs() < 1e-6 && (durations[1] - 0.04).abs() < 1e-6);
    let old = visualizer.gpu_models[0].reset;
    step(&mut visualizer, 0.09, 0.11, 2.0, (1920.0, 1080.0));
    assert_ne!(visualizer.gpu_models[0].reset, old);
    let old = visualizer.gpu_models[0].reset;
    let pose = visualizer.scene.current.as_ref().unwrap().pose;
    let time = visualizer.time;
    visualizer.set_model_motion(ModelMotion::Still);
    visualizer.set_model_motion(ModelMotion::Swept);
    assert_eq!(visualizer.time, time);
    assert_eq!(
        visualizer.scene.current.as_ref().unwrap().pose.yaw,
        pose.yaw
    );
    step(&mut visualizer, 0.11, 0.13, 2.0, (1920.0, 1080.0));
    assert_ne!(visualizer.gpu_models[0].reset, old);
    for (start, end) in [(0.01, 0.03), (1.0, 1.02)] {
        let old = visualizer.gpu_models[0].reset;
        step(&mut visualizer, start, end, 2.0, (1920.0, 1080.0));
        assert_ne!(visualizer.gpu_models[0].reset, old);
        assert_eq!(
            visualizer.scene.current.as_ref().unwrap().gpu_bounds.len(),
            1
        );
    }
}

#[test]
fn legacy_exposure_settings_keep_the_fixed_two_frame_gpu_path() {
    for shutter in [1.0, 2.0, 2.5, 3.0, 16.0] {
        let mut visualizer = visualizer();
        step(&mut visualizer, 0.0, 0.02, shutter, (1280.0, 720.0));
        assert_eq!(visualizer.frame().gpu_models.len(), 1);
        assert!(visualizer.gpu_ribbon_status().contains("shutter 2"));
        let current = visualizer.scene.current.as_ref().unwrap();
        assert!(current.vertices.is_empty() && current.current.is_empty());
        assert!(current.history.is_empty());
    }
}

#[test]
fn model_capacity_is_validated_before_gpu_frames_are_built() {
    let mut visualizer = visualizer();
    assert!(visualizer.validate_gpu_models(2).is_err());
    assert!(visualizer.validate_gpu_models(3).is_ok());
    step(&mut visualizer, 0.0, 0.02, 2.0, (1280.0, 720.0));
    assert_eq!(visualizer.frame().gpu_models.len(), 1);
}

#[test]
fn blur_off_uses_gpu_projection_with_one_pose_at_every_shutter() {
    for shutter in [1.0, 2.0, 2.5, 16.0] {
        let mut visualizer = visualizer();
        step(&mut visualizer, 0.0, 0.02, 2.0, (1280.0, 720.0));
        let swept_generation = visualizer.gpu_models[0].reset;
        visualizer.set_model_motion(ModelMotion::Still);
        step(&mut visualizer, 0.02, 0.04, shutter, (1280.0, 720.0));
        let frame = visualizer.frame();
        assert_eq!(frame.gpu_models.len(), 1);
        assert!(visualizer.gpu_ribbon_status().starts_with("GPU lines"));
        let first_generation = frame.gpu_models[0].reset;
        assert_ne!(first_generation, swept_generation);
        assert!(frame.gpu_models[0].projected.is_empty());
        assert!(frame.gpu_models[0].projection.is_some());
        let current = visualizer.scene.current.as_ref().unwrap();
        assert!(
            current.vertices.is_empty() && current.current.is_empty() && current.history.is_empty()
        );
        assert_eq!(
            current.gpu_bounds.len(),
            1,
            "blur-off layout uses only the current pose"
        );

        step(&mut visualizer, 0.04, 0.06, shutter, (1280.0, 720.0));
        let second_generation = visualizer.gpu_models[0].reset;
        assert_ne!(
            second_generation, first_generation,
            "each blur-off frame must reseed all GPU history poses"
        );
        assert_eq!(
            visualizer.scene.current.as_ref().unwrap().gpu_bounds.len(),
            1
        );

        visualizer.set_model_motion(ModelMotion::Swept);
        step(&mut visualizer, 0.06, 0.08, 2.0, (1280.0, 720.0));
        let reenabled_generation = visualizer.gpu_models[0].reset;
        assert_ne!(reenabled_generation, second_generation);
        assert_eq!(
            visualizer.scene.current.as_ref().unwrap().gpu_bounds.len(),
            1
        );
        step(&mut visualizer, 0.08, 0.10, 2.0, (1280.0, 720.0));
        assert_eq!(
            visualizer.gpu_models[0].reset, reenabled_generation,
            "swept mode retains its temporal history again"
        );
        assert_eq!(
            visualizer.scene.current.as_ref().unwrap().gpu_bounds.len(),
            2
        );
    }
}

#[test]
fn gpu_projection_parameters_match_camera_and_bound_clipped_moving_parts() {
    let model = prepared("projection parameter fixture");
    let camera = Camera::new(5120.0, 2880.0, 1.9);
    let slide = Vec3::new(0.37, -0.11, 0.22);
    for blend in [0.0, 0.3, 1.0, 1.2, -0.2] {
        for distance in [0.25, 1.7, 4.2] {
            let pose = Pose {
                yaw: 0.7,
                pitch: -0.4,
                roll: 0.25,
                scale: 1.3,
                distance,
                shake: Vec2::new(5.0, -3.0),
            };
            let focus = FocusProjection {
                target: Vec3::new(0.1, -0.12, 0.2),
                rotation_yaw: -0.5,
                rotation_pitch: 0.3,
                distance,
                focal: camera.focal * 1.2,
                near: 0.05,
                blend,
            };
            let depth = DepthOpacity::centered(distance, pose.scale, 0.7);
            let projection = gpu_bridge::GpuProjection::new(camera, pose, focus, depth, slide);
            let bounds = projection.bounds(&model.gpu_topology);
            for source in &model.gpu_topology.vertices {
                let world = Vec3::new(source[0], source[1], source[2]) + slide * source[3];
                let cpu = if blend == 0.0 {
                    camera.project(world, pose)
                } else {
                    camera.focused_projector(pose, focus)(world)
                };
                let actual = projection.projected(*source);
                assert_eq!(actual.visible, cpu.visible);
                assert!((actual.depth - cpu.depth).abs() < 1e-5);
                // Near-plane projections can be tens of thousands of pixels.
                let tolerance = 0.002 + cpu.screen.x.abs().max(cpu.screen.y.abs()) * 2e-6;
                assert!((actual.screen.x - cpu.screen.x).abs() < tolerance);
                assert!((actual.screen.y - cpu.screen.y).abs() < tolerance);
                let record = projection.record(*source);
                if cpu.visible {
                    assert!((record[2] - depth.factor(cpu.depth)).abs() < 1e-6);
                    assert!(bounds
                        .expect("visible vertex has layout bounds")
                        .inflate(0.01, 0.01)
                        .contains(Point::new(cpu.screen.x as f64, cpu.screen.y as f64)));
                } else {
                    assert_eq!(record, [0.0; 4]);
                }
            }
        }
    }
}

#[test]
fn lazy_gpu_anchors_wait_for_a_pose_and_reset_with_prepared_geometry() {
    let mut visualizer = visualizer();
    assert!(visualizer.vertex_screen(0).is_none());
    visualizer.activate_diagnosis((1280.0, 720.0), 1.0, Vec3::default());
    assert!(visualizer.diagnosis.vertex.is_none());
    step(&mut visualizer, 0.0, 0.02, 2.0, (1280.0, 720.0));
    let expected = visualizer.gpu_models[0].cpu_projected()[0];
    assert_eq!(
        visualizer.vertex_screen(0),
        Some([expected[0], expected[1]])
    );
    assert!(visualizer.vertex_screen(6).is_none());
    visualizer
        .scene
        .set_prepared(&prepared("anchor replacement"));
    assert!(
        visualizer.vertex_screen(0).is_none(),
        "old view cannot project replacement geometry before its first pose"
    );
}
