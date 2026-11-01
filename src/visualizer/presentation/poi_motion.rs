//! Shared motion for POI text and instrument callouts.

use vello::kurbo::{Point, Vec2};

use super::super::detection::Spring;
use super::Stage;

const CARD_STIFFNESS: f32 = 80.0;
const CARD_DAMPING: f32 = 10.0;
const COAST_FRICTION: f64 = 3.0;
pub(super) const CARD_COAST_SECONDS: f32 = 0.5;
pub(super) const GRAPH_COAST_SECONDS: f32 = 1.1;

pub(super) fn advance_springs(x: &mut Spring, y: &mut Spring, dt: f32) {
    x.advance(dt, CARD_STIFFNESS, CARD_DAMPING);
    y.advance(dt, CARD_STIFFNESS, CARD_DAMPING);
}

#[derive(Clone, Copy)]
pub(super) struct Coast {
    position: Point,
    velocity: Vec2,
    age: f64,
    duration: f64,
}

impl Coast {
    pub(super) fn from_springs(x: &Spring, y: &Spring, duration_seconds: f32) -> Self {
        assert!(duration_seconds.is_finite() && duration_seconds > 0.0);
        Self {
            position: Point::new(finite(x.value()), finite(y.value())),
            velocity: Vec2::new(finite(x.velocity()), finite(y.velocity())),
            age: 0.0,
            duration: f64::from(duration_seconds),
        }
    }

    pub(super) fn position(&self) -> Point {
        self.position
    }

    pub(super) fn velocity(&self) -> Vec2 {
        self.velocity
    }

    pub(super) fn advance(&mut self, dt: f32, width: f64, height: f64, stage: &Stage) {
        let dt = finite(dt).max(0.0);
        let step = dt.min((self.duration - self.age).max(0.0));
        let decay = (-COAST_FRICTION * step).exp();
        self.position += self.velocity * ((1.0 - decay) / COAST_FRICTION);
        self.velocity *= decay;
        self.age += dt;
        // The viewport edge is not a wall: a fading card may slide partly
        // offscreen. Only contain pathological spring values far beyond any
        // useful draw region, keeping ordinary release motion continuous.
        let limit_x = stage.width.max(width.max(0.0)).max(1.0) * 16.0;
        let limit_y = stage.height.max(height.max(0.0)).max(1.0) * 16.0;
        let clamp = |position: &mut f64, velocity: &mut f64, low: f64, high: f64| {
            let bounded = if low <= high {
                position.clamp(low, high)
            } else {
                (low + high) * 0.5
            };
            if bounded != *position {
                *velocity = 0.0;
                *position = bounded;
            }
        };
        clamp(
            &mut self.position.x,
            &mut self.velocity.x,
            -limit_x,
            limit_x,
        );
        clamp(
            &mut self.position.y,
            &mut self.velocity.y,
            -limit_y,
            limit_y,
        );
    }

    pub(super) fn alpha(&self) -> f32 {
        let progress = (self.age / self.duration).clamp(0.0, 1.0) as f32;
        1.0 - progress * progress * (3.0 - 2.0 * progress)
    }

    pub(super) fn finished(&self) -> bool {
        self.age >= self.duration
    }
}

fn finite(value: f32) -> f64 {
    if value.is_finite() {
        f64::from(value)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Size;

    fn moving() -> Coast {
        let mut x = Spring::default();
        let mut y = Spring::default();
        x.reset(600.0);
        y.reset(400.0);
        x.impulse(160.0);
        y.impulse(-80.0);
        Coast::from_springs(&x, &y, GRAPH_COAST_SECONDS)
    }

    #[test]
    fn retirement_preserves_velocity_then_friction_slows_it_over_a_visible_tail() {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut coast = moving();
        assert_eq!(coast.velocity(), Vec2::new(160.0, -80.0));
        assert_eq!(coast.position(), Point::new(600.0, 400.0));
        coast.advance(0.1, 200.0, 100.0, &stage);
        assert!(coast.position().x > 610.0);
        assert!(coast.velocity().x > 100.0 && coast.velocity().x < 160.0);
        assert!(coast.alpha() > 0.95);
        coast.advance(0.7, 200.0, 100.0, &stage);
        assert!(!coast.finished());
        assert!(coast.velocity().x < 20.0);
        assert!(coast.alpha() > 0.0);
        coast.advance(0.4, 200.0, 100.0, &stage);
        assert!(coast.finished());
        assert_eq!(coast.alpha(), 0.0);
    }

    #[test]
    fn coast_integrates_the_same_path_at_different_frame_rates_and_crosses_edges_smoothly() {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut fast = moving();
        let mut slow = moving();
        for _ in 0..120 {
            fast.advance(1.0 / 120.0, 200.0, 100.0, &stage);
        }
        for _ in 0..30 {
            slow.advance(1.0 / 30.0, 200.0, 100.0, &stage);
        }
        assert!(fast.position().distance(slow.position()) < 1e-8);
        assert!((fast.velocity() - slow.velocity()).length() < 1e-8);
        let mut edge = moving();
        edge.position = Point::new(1880.0, 40.0);
        edge.advance(0.2, 200.0, 100.0, &stage);
        assert!(edge.position().x > 1880.0);
        assert!(edge.position().y < 40.0);
        assert!(edge.velocity().x > 0.0 && edge.velocity().y < 0.0);
        assert!(edge.position().x.is_finite() && edge.position().y.is_finite());
    }

    #[test]
    fn regular_cards_finish_at_half_a_second_while_graphs_keep_their_longer_tail() {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut x = Spring::default();
        let mut y = Spring::default();
        x.reset(600.0);
        y.reset(400.0);
        x.impulse(160.0);
        let mut card = Coast::from_springs(&x, &y, CARD_COAST_SECONDS);
        let mut graph = Coast::from_springs(&x, &y, GRAPH_COAST_SECONDS);
        card.advance(0.25, 200.0, 100.0, &stage);
        graph.advance(0.25, 200.0, 100.0, &stage);
        assert_eq!(card.position(), graph.position());
        assert_eq!(card.velocity(), graph.velocity());
        assert!(card.alpha() > 0.0 && card.velocity().x > 0.0);
        card.advance(0.25, 200.0, 100.0, &stage);
        graph.advance(0.5, 200.0, 100.0, &stage);
        assert!(card.finished());
        assert_eq!(card.alpha(), 0.0);
        assert!(!graph.finished());
        assert!(graph.alpha() > 0.0 && graph.velocity().x > 0.0);
    }
}
