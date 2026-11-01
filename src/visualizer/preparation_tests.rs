use super::*;

#[test]
#[ignore = "CPU timing diagnostic; run explicitly with --release --nocapture"]
fn measures_prepared_model_switching() {
    use std::{hint::black_box, time::Instant};

    let startup = Instant::now();
    let mut visualizer = loaded();
    let startup_ms = startup.elapsed().as_secs_f64() * 1_000.0;
    let models = prepared(&visualizer);
    let count = models.position().1;
    let (vertices, edges) = models.prepared_capacity();
    let mut preparation = Vec::with_capacity(count * 5);
    let mut sources = model::ModelSet::load(&mut Rng::new(0x5052_4550)).unwrap();
    for _ in 0..5 {
        for _ in 0..count {
            // File parsing is excluded: compare reusable model preparation with switching.
            let before = Instant::now();
            black_box(PreparedModel::new(
                sources.current(),
                visualizer.crease_threshold,
                visualizer.catalog.as_ref(),
            ));
            preparation.push(before.elapsed().as_secs_f64() * 1_000_000.0);
            sources.navigate(crate::frame_schedule::Adjustment::Increase);
        }
    }
    let mut switches = Vec::with_capacity(count * 100);
    for _ in 0..count * 100 {
        let before = Instant::now();
        visualizer.next_model();
        black_box(&visualizer.scene);
        switches.push(before.elapsed().as_secs_f64() * 1_000_000.0);
    }
    preparation.sort_by(f64::total_cmp);
    switches.sort_by(f64::total_cmp);
    eprintln!("{count} models; startup {startup_ms:.2} ms; largest {vertices} vertices / {edges} edges; prepare from loaded mesh median {:.2} us; prepared switch median {:.2} us, p95 {:.2} us (excludes animation, presentation layout, and GPU draw)", preparation[preparation.len()/2], switches[switches.len()/2], switches[switches.len()*95/100]);
}

fn loaded() -> Visualizer {
    let mut visualizer = Visualizer::new();
    visualizer.set_model_rotation(ModelRotation::Locked);
    visualizer.load_models();
    assert!(matches!(visualizer.models, Models::Ready(_)));
    visualizer
}

fn prepared(visualizer: &Visualizer) -> &model::ModelSet {
    let Models::Ready(models) = &visualizer.models else {
        panic!("the bundled models must load");
    };
    models
}

fn runtimes(scene: &Scene) -> impl Iterator<Item = &ModelRuntime> {
    scene
        .current
        .iter()
        .chain(scene.outgoing.iter())
        .chain(scene.spare.iter())
}

fn slot_pointers(scene: &Scene) -> BTreeSet<(usize, usize)> {
    runtimes(scene)
        .map(|runtime| {
            (
                runtime.vertices.as_ptr() as usize,
                runtime.current.as_ptr() as usize,
            )
        })
        .collect()
}

fn history_pointers(scene: &Scene) -> BTreeSet<usize> {
    runtimes(scene)
        .flat_map(|runtime| {
            runtime
                .history_buffers
                .iter()
                .chain(runtime.history.iter().map(|snapshot| &snapshot.lines))
                .map(|lines| lines.as_ptr() as usize)
        })
        .collect()
}

fn assert_reserved(visualizer: &Visualizer) {
    let models = prepared(visualizer);
    let vertices = models
        .prepared_models()
        .iter()
        .map(|model| model.wireframe.vertex_count())
        .max()
        .unwrap();
    let edges = models
        .prepared_models()
        .iter()
        .map(|model| model.wireframe.edges().len())
        .max()
        .unwrap();
    assert_eq!(runtimes(&visualizer.scene).count(), 2);
    for runtime in runtimes(&visualizer.scene) {
        assert!(runtime.vertices.capacity() >= vertices);
        assert!(runtime.current.capacity() >= edges);
        assert!(runtime.history.capacity() >= MAX_HISTORY);
        assert!(runtime.history_buffers.capacity() >= MAX_HISTORY);
        assert_eq!(
            runtime.history.len() + runtime.history_buffers.len(),
            MAX_HISTORY
        );
        for lines in runtime
            .history_buffers
            .iter()
            .chain(runtime.history.iter().map(|snapshot| &snapshot.lines))
        {
            assert!(lines.capacity() >= edges);
        }
    }
    assert!(visualizer.model_lines.capacity() >= edges);
    assert!(visualizer.model_ribbons.capacity() >= edges * 8);
}

fn tick(visualizer: &mut Visualizer, time: &mut AudioTime) {
    let next = time.shifted(0.1);
    visualizer.update(
        AudioInterval::new(*time, next),
        ShutterFrames::default(),
        0.1,
        &Palette::from_theme(ThemeId::default()),
        (960.0, 540.0),
        1.0,
        1.0,
    );
    *time = next;
}

#[test]
fn startup_prepares_every_model_and_reuses_both_slots_through_interrupted_switches() {
    let mut visualizer = loaded();
    let cached = prepared(&visualizer).prepared_models().to_vec();
    assert_eq!(cached.len(), prepared(&visualizer).position().1);
    assert!(visualizer.scene.current.is_some());
    assert!(visualizer.scene.spare.is_some());
    assert!(visualizer.scene.outgoing.is_none());
    assert_reserved(&visualizer);
    let slots = slot_pointers(&visualizer.scene);
    let histories = history_pointers(&visualizer.scene);
    assert_eq!(slots.len(), 2);
    assert_eq!(histories.len(), MAX_HISTORY * 2);
    let mut time = AudioTime::ZERO;
    let mut completed = 0;
    let mut interrupted = 0;
    for step in 0..cached.len() * 2 {
        if visualizer.scene.outgoing.is_some() {
            interrupted += 1;
        }
        visualizer.next_model();
        let models = prepared(&visualizer);
        let expected = &cached[models.position().0 - 1];
        assert!(Arc::ptr_eq(models.current_prepared(), expected));
        let current = visualizer.scene.current.as_ref().unwrap();
        assert!(Arc::ptr_eq(&current.wireframe, &expected.wireframe));
        assert!(Arc::ptr_eq(
            &current.slide_vertices,
            &expected.slide_vertices
        ));
        assert_eq!(visualizer.showcase.pois().as_ptr(), expected.pois.as_ptr());
        tick(&mut visualizer, &mut time);
        if step % 12 == 0 {
            for _ in 0..12 {
                tick(&mut visualizer, &mut time);
            }
            assert!(visualizer.scene.outgoing.is_none());
            assert!(visualizer.scene.spare.is_some());
            completed += 1;
        }
        assert_eq!(slot_pointers(&visualizer.scene), slots);
        assert_eq!(history_pointers(&visualizer.scene), histories);
    }
    assert!(completed > 0 && interrupted > 0);
}

#[test]
fn history_storage_survives_resets_motion_toggles_and_crease_expansion() {
    let mut visualizer = Visualizer::new();
    visualizer.set_model_rotation(ModelRotation::Locked);
    visualizer.set_crease_threshold(CreaseThreshold::MAX);
    visualizer.load_models();
    assert_reserved(&visualizer);
    let coarse_edges: usize = prepared(&visualizer)
        .prepared_models()
        .iter()
        .map(|model| model.wireframe.edges().len())
        .sum();
    visualizer.set_crease_threshold(CreaseThreshold::MIN);
    let fine_edges: usize = prepared(&visualizer)
        .prepared_models()
        .iter()
        .map(|model| model.wireframe.edges().len())
        .sum();
    assert!(fine_edges > coarse_edges);
    assert_reserved(&visualizer);
    let slots = slot_pointers(&visualizer.scene);
    let histories = history_pointers(&visualizer.scene);
    let mut time = AudioTime::ZERO;
    for _ in 0..MAX_HISTORY + 2 {
        tick(&mut visualizer, &mut time);
    }
    assert_eq!(
        visualizer.scene.current.as_ref().unwrap().history.len(),
        MAX_HISTORY
    );
    assert_eq!(history_pointers(&visualizer.scene), histories);
    visualizer.reset_audio();
    assert!(visualizer
        .scene
        .current
        .as_ref()
        .unwrap()
        .history
        .is_empty());
    assert_eq!(history_pointers(&visualizer.scene), histories);
    tick(&mut visualizer, &mut time);
    visualizer.next_model();
    tick(&mut visualizer, &mut time);
    visualizer.set_model_motion(ModelMotion::Still);
    assert!(runtimes(&visualizer.scene).all(|runtime| runtime.history.is_empty()));
    assert_eq!(history_pointers(&visualizer.scene), histories);
    visualizer.set_model_motion(ModelMotion::Swept);
    for _ in 0..prepared(&visualizer).position().1 {
        visualizer.next_model();
        tick(&mut visualizer, &mut time);
        assert_reserved(&visualizer);
        assert_eq!(slot_pointers(&visualizer.scene), slots);
        assert_eq!(history_pointers(&visualizer.scene), histories);
    }
}

#[test]
fn refreshing_prepared_geometry_preserves_the_live_showcase() {
    let mut visualizer = loaded();
    assert!(visualizer.showcase.enter_focus(&mut visualizer.rng, 0.75));
    let mut time = AudioTime::ZERO;
    for _ in 0..3 {
        tick(&mut visualizer, &mut time);
    }
    assert!(visualizer.showcase.focus_blend() > 0.0);
    let id = visualizer.showcase.target_id().map(str::to_owned);
    let target = visualizer.showcase.target_index();
    let before = visualizer.showcase.frame();
    let model_revision = visualizer.model_revision();
    let old_wireframe = Arc::clone(&visualizer.scene.current.as_ref().unwrap().wireframe);
    visualizer.set_crease_threshold(CreaseThreshold::MAX);
    let after = visualizer.showcase.frame();
    assert_eq!(visualizer.showcase.mode(), showcase::ShowcaseMode::Poi);
    assert_eq!(visualizer.showcase.target_id(), id.as_deref());
    assert_eq!(visualizer.showcase.target_index(), target);
    assert_eq!(visualizer.model_revision(), model_revision);
    assert_eq!(after.focus.center, before.focus.center);
    assert_eq!(after.focus.radius, before.focus.radius);
    assert_eq!(after.focus.blend, before.focus.blend);
    assert_eq!(after.focus.model_motion, before.focus.model_motion);
    assert_eq!(after.focus.orbit_yaw, before.focus.orbit_yaw);
    assert_eq!(after.focus.orbit_pitch, before.focus.orbit_pitch);
    assert_eq!(after.focus.fov, before.focus.fov);
    assert_eq!(after.highlight_part, before.highlight_part);
    assert_eq!(after.focus_child, before.focus_child);
    let current = visualizer.scene.current.as_ref().unwrap();
    assert!(!Arc::ptr_eq(&old_wireframe, &current.wireframe));
    assert!(Arc::ptr_eq(
        &current.wireframe,
        &prepared(&visualizer).current_prepared().wireframe
    ));
    tick(&mut visualizer, &mut time);
    assert_eq!(visualizer.showcase.target_id(), id.as_deref());
}
