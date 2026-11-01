use super::*;

fn armed_visualizer(rate: usize) -> Visualizer {
    let mut visualizer = Visualizer::new();
    visualizer.set_samplerate(rate);
    let hold = visualizer.detectors.arrangement.gap_rms_hold;
    visualizer.observe(Spectrum::default(), hold + 0.01);
    assert!(visualizer.onsets.status().drop.armed);
    visualizer.pending_actions.clear();
    visualizer
}

fn snare(visualizer: &mut Visualizer) {
    visualizer.emit_detector_events(detection::DetectorEvents {
        snare: Some(1.0),
        ..detection::DetectorEvents::default()
    });
}

fn drop(visualizer: &mut Visualizer) {
    visualizer.emit_detector_events(detection::DetectorEvents {
        drop: Some(1.0),
        ..detection::DetectorEvents::default()
    });
}

fn silence(visualizer: &mut Visualizer, samples: usize) {
    visualizer.consume_buf(&vec![[0.0; 2]; samples], 1.0);
}

fn snare_actions(visualizer: &Visualizer) -> usize {
    visualizer
        .pending_actions
        .iter()
        .filter(|action| matches!(action, Action::Snare))
        .count()
}

#[test]
fn unarmed_snare_action_and_detection_count_are_immediate() {
    let mut visualizer = Visualizer::new();
    assert!(!visualizer.onsets.status().drop.armed);
    snare(&mut visualizer);
    assert_eq!(visualizer.trigger_counts().snare, 1);
    assert!(matches!(
        visualizer.pending_actions.pop_front(),
        Some(Action::Snare)
    ));
}

#[test]
fn armed_snare_waits_fifty_audio_milliseconds_and_releases_once() {
    for rate in [44_100, 48_000, 96_000] {
        let mut visualizer = armed_visualizer(rate);
        snare(&mut visualizer);
        assert_eq!(visualizer.trigger_counts().snare, 1);
        assert_eq!(snare_actions(&visualizer), 0);
        silence(&mut visualizer, rate / 20 - 1);
        assert_eq!(snare_actions(&visualizer), 0);
        silence(&mut visualizer, 1);
        assert_eq!(snare_actions(&visualizer), 1);
        silence(&mut visualizer, rate / 10);
        assert!(visualizer.onsets.status().drop.armed);
        assert_eq!(snare_actions(&visualizer), 1);
        assert_eq!(visualizer.trigger_counts().snare, 1);
    }
}

#[test]
fn a_drop_cancels_a_held_snare_without_erasing_its_detection() {
    let mut visualizer = armed_visualizer(48_000);
    snare(&mut visualizer);
    silence(&mut visualizer, 2_160);
    drop(&mut visualizer);
    silence(&mut visualizer, 4_800);
    assert_eq!(visualizer.trigger_counts().snare, 1);
    assert_eq!(visualizer.trigger_counts().drop, 1);
    assert_eq!(snare_actions(&visualizer), 0);
    assert!(matches!(
        visualizer.pending_actions.pop_front(),
        Some(Action::Drop(_))
    ));
}

#[test]
fn simultaneous_drop_and_snare_keep_both_counts_but_only_the_drop_action() {
    let mut visualizer = armed_visualizer(48_000);
    visualizer.emit_detector_events(detection::DetectorEvents {
        drop: Some(1.0),
        snare: Some(1.0),
        ..detection::DetectorEvents::default()
    });
    silence(&mut visualizer, 4_800);
    assert_eq!(visualizer.trigger_counts().snare, 1);
    assert_eq!(visualizer.trigger_counts().drop, 1);
    assert_eq!(snare_actions(&visualizer), 0);
}

#[test]
fn successive_snares_keep_independent_deadlines() {
    let mut visualizer = armed_visualizer(48_000);
    snare(&mut visualizer);
    silence(&mut visualizer, 960);
    snare(&mut visualizer);
    assert_eq!(visualizer.trigger_counts().snare, 2);
    silence(&mut visualizer, 1_439);
    assert_eq!(snare_actions(&visualizer), 0);
    silence(&mut visualizer, 1);
    assert_eq!(snare_actions(&visualizer), 1);
    silence(&mut visualizer, 961);
    assert_eq!(snare_actions(&visualizer), 2);
    silence(&mut visualizer, 4_800);
    assert_eq!(snare_actions(&visualizer), 2);
}

#[test]
fn audio_reset_and_samplerate_changes_discard_held_snares() {
    for change_rate in [false, true] {
        let mut visualizer = armed_visualizer(48_000);
        snare(&mut visualizer);
        if change_rate {
            visualizer.set_samplerate(96_000);
        } else {
            visualizer.reset_audio();
        }
        silence(&mut visualizer, 9_600);
        assert_eq!(snare_actions(&visualizer), 0);
        assert_eq!(visualizer.trigger_counts().snare, 0);
    }
}

#[test]
fn real_drops_cancel_only_snares_still_inside_their_grace_period() {
    // An immediate LF return wins the grace window; one starting at 100 ms
    // must preserve the expired snare, even inside one large capture callback.
    for return_at in [0, 4_800] {
        let input: Vec<_> = (0..9_600)
            .map(|sample| {
                let value = if sample < return_at {
                    0.0
                } else {
                    let time = (sample - return_at) as f32 / 48_000.0;
                    0.5 * (TAU * 120.0 * time).sin()
                };
                [value, value]
            })
            .collect();
        let expected = if return_at == 0 {
            vec!["drop"]
        } else {
            vec!["snare", "drop"]
        };
        for partition in [1, 137, 9_600] {
            let mut visualizer = armed_visualizer(48_000);
            // Keep the injected snare as the only one under test; startup
            // leakage from the LF tone must not add a percussion action.
            let mut settings = visualizer.detector_settings();
            settings.percussion.channels[1].filters.mix_rms = true;
            settings.percussion.channels[1].output_rms_db = 0.0;
            visualizer.set_detector_settings(settings);
            snare(&mut visualizer);
            for callback in input.chunks(partition) {
                visualizer.consume_buf(callback, 1.0);
            }
            assert!(!visualizer.onsets.status().drop.armed);
            assert_eq!(visualizer.trigger_counts().snare, 1);
            assert_eq!(visualizer.trigger_counts().drop, 1);
            let actions: Vec<_> = visualizer
                .pending_actions
                .iter()
                .filter_map(|action| match action {
                    Action::Snare => Some("snare"),
                    Action::Drop(_) => Some("drop"),
                    _ => None,
                })
                .collect();
            assert_eq!(
                actions, expected,
                "return at {return_at}, callback {partition}"
            );
        }
    }
}
