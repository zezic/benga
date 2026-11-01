use std::sync::Arc;

use super::{EdgeDissolve, EdgeOrder, InheritedCoverage};

fn alphas(dissolve: &EdgeDissolve, count: usize) -> Vec<f32> {
    (0..count).map(|index| dissolve.alpha(index)).collect()
}

fn selected(dissolve: &EdgeDissolve, count: usize) -> usize {
    (0..count)
        .filter(|&index| dissolve.alpha(index) > 0.0)
        .count()
}

#[test]
fn endpoints_draw_exactly_one_complete_model() {
    for (old_count, new_count) in [(0, 0), (0, 19), (23, 0), (1, 1), (97, 311)] {
        let slots = old_count.max(new_count);
        let mut old = EdgeDissolve::new(old_count, 17);
        let mut new = EdgeDissolve::new(new_count, 31);
        old.outgoing(0.0, slots);
        new.incoming(0.0, slots);
        assert!(alphas(&old, old_count).iter().all(|&alpha| alpha == 1.0));
        assert!(alphas(&new, new_count).iter().all(|&alpha| alpha == 0.0));
        old.outgoing(1.0, slots);
        new.incoming(1.0, slots);
        assert!(alphas(&old, old_count).iter().all(|&alpha| alpha == 0.0));
        assert!(alphas(&new, new_count).iter().all(|&alpha| alpha == 1.0));
    }
}

#[test]
fn transition_never_emits_more_edges_than_the_larger_model() {
    // Include empty models, tiny-to-large swaps, prime counts, and neighboring
    // counts so rounded slot assignments cannot silently overlap.
    for (old_count, new_count) in [
        (0, 0),
        (0, 127),
        (257, 0),
        (1, 1),
        (1, 1021),
        (1021, 1),
        (113, 127),
        (127, 113),
        (1000, 1001),
        (1001, 1000),
        (1024, 1024),
    ] {
        let slots = old_count.max(new_count);
        let mut old = EdgeDissolve::new(old_count, 0x4f4c44);
        let mut new = EdgeDissolve::new(new_count, 0x4e4557);
        for step in 0..=1000 {
            let progress = step as f32 / 1000.0;
            old.outgoing(progress, slots);
            new.incoming(progress, slots);
            let count = selected(&old, old_count) + selected(&new, new_count);
            assert!(
                count <= slots,
                "{old_count} -> {new_count} at {progress}: {count} edges for {slots} slots"
            );
        }
    }
}

#[test]
fn reveal_is_deterministic_and_each_edge_moves_in_only_one_direction() {
    let count = 257;
    let mut incoming = EdgeDissolve::new(count, 42);
    let mut same_seed = EdgeDissolve::new(count, 42);
    let mut different_seed = EdgeDissolve::new(count, 43);
    incoming.incoming(0.5, count);
    same_seed.incoming(0.5, count);
    different_seed.incoming(0.5, count);
    let midpoint = alphas(&incoming, count);
    assert_eq!(midpoint, alphas(&same_seed, count));
    assert_ne!(midpoint, alphas(&different_seed, count));
    assert!(midpoint.iter().any(|&alpha| alpha > 0.0 && alpha < 1.0));

    let mut outgoing = EdgeDissolve::new(count, 73);
    let mut previous_in = vec![0.0; count];
    let mut previous_out = vec![1.0; count];
    for step in 0..=400 {
        let progress = step as f32 / 400.0;
        incoming.incoming(progress, count);
        outgoing.outgoing(progress, count);
        for index in 0..count {
            let enters = incoming.alpha(index);
            let exits = outgoing.alpha(index);
            assert!(enters.is_finite() && (0.0..=1.0).contains(&enters));
            assert!(exits.is_finite() && (0.0..=1.0).contains(&exits));
            assert!(enters >= previous_in[index]);
            assert!(exits <= previous_out[index]);
            previous_in[index] = enters;
            previous_out[index] = exits;
        }
    }
}

#[test]
fn interrupted_reveal_never_resurrects_or_brightens_hidden_edges() {
    let count = 997;
    let mut dissolve = EdgeDissolve::new(count, 1234);
    dissolve.incoming(0.37, count);
    let interrupted = alphas(&dissolve, count);
    assert!(interrupted.contains(&0.0));
    assert!(interrupted.iter().any(|&alpha| alpha > 0.0));
    dissolve.freeze();
    assert_eq!(interrupted, alphas(&dissolve, count));
    dissolve.outgoing(0.0, count);
    assert_eq!(interrupted, alphas(&dissolve, count));

    let mut previous = interrupted.clone();
    for step in 0..=400 {
        dissolve.outgoing(step as f32 / 400.0, count);
        for index in 0..count {
            let alpha = dissolve.alpha(index);
            assert!(alpha <= previous[index]);
            assert!(alpha <= interrupted[index]);
            if interrupted[index] == 0.0 {
                assert_eq!(alpha, 0.0);
            }
            previous[index] = alpha;
        }
    }
    assert_eq!(selected(&dissolve, count), 0);
}

#[test]
fn show_all_discards_an_interrupted_visibility_mask() {
    let count = 199;
    let mut dissolve = EdgeDissolve::new(count, 53);
    dissolve.incoming(0.2, count);
    dissolve.freeze();
    dissolve.outgoing(0.6, count);
    dissolve.show_all();
    assert!(alphas(&dissolve, count).iter().all(|&alpha| alpha == 1.0));
    dissolve.outgoing(0.0, count);
    assert!(alphas(&dissolve, count).iter().all(|&alpha| alpha == 1.0));
}

#[test]
fn prepared_order_is_shared_without_sharing_visibility_state() {
    let count = 257;
    let order = EdgeOrder::new(count, 42);
    let mut outgoing = EdgeDissolve::from_order(order.clone());
    let mut incoming = EdgeDissolve::from_order(order.clone());
    assert!(Arc::ptr_eq(&order.ranks, &outgoing.order.ranks));
    assert!(Arc::ptr_eq(&order.ranks, &incoming.order.ranks));

    outgoing.incoming(0.37, count);
    let interrupted = alphas(&outgoing, count);
    outgoing.freeze();
    assert!(matches!(
        outgoing.inherited,
        InheritedCoverage::Selection(_)
    ));
    outgoing.outgoing(0.2, count);
    assert!(alphas(&incoming, count).iter().all(|&alpha| alpha == 1.0));
    incoming.incoming(0.37, count);
    assert_eq!(interrupted, alphas(&incoming, count));
    incoming.show_all();
    assert!(alphas(&outgoing, count)
        .iter()
        .zip(interrupted)
        .all(|(&alpha, previous)| alpha <= previous));
}

#[test]
fn repeated_freezes_preserve_combined_coverage() {
    let count = 997;
    let mut dissolve = EdgeDissolve::new(count, 1234);
    dissolve.incoming(0.37, count);
    let mut expected = alphas(&dissolve, count);
    dissolve.freeze();
    dissolve.freeze();
    assert_eq!(expected, alphas(&dissolve, count));

    for (progress, slots) in [(0.2, 1021), (0.4, 1171), (0.6, count)] {
        let mut outgoing = EdgeDissolve::new(count, 1234);
        outgoing.outgoing(progress, slots);
        for (alpha, factor) in expected.iter_mut().zip(alphas(&outgoing, count)) {
            *alpha *= factor;
        }
        dissolve.outgoing(progress, slots);
        dissolve.freeze();
        assert_eq!(expected, alphas(&dissolve, count));
    }
    dissolve.show_all();
    assert!(alphas(&dissolve, count).iter().all(|&alpha| alpha == 1.0));
}
