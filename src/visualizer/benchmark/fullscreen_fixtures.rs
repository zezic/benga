//! Frozen, full-viewport source-edge histories for ribbon throughput tests.
//!
//! Repeated edges deliberately overlap the same pixels. They share two source
//! vertices, so the benchmark measures rasterization/blending capacity rather
//! than CPU projection, geometry transfer or model loading. All three recorded
//! poses must be submitted before freezing preparation for timed draws.
use std::sync::Arc;

use super::super::{
    gpu_bridge::{self, GpuModelFrame, GpuTopology},
    MotionOpacity,
};

const MARGIN: f32 = 16.0;
const INTERVAL_SECONDS: f32 = 1.0 / 60.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RibbonFixture {
    /// One vertical source edge sweeps monotonically across the entire viewport.
    Sweep,
    /// The actual middle pose crosses the viewport and returns to its start.
    Reversal,
    /// Two diagonal poses cross a horizontal middle pose, producing masked lobes.
    Crossing,
}

impl RibbonFixture {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Sweep => "sweep",
            Self::Reversal => "reversal",
            Self::Crossing => "crossing",
        }
    }

    pub(super) fn parse(name: &str) -> Option<Self> {
        match name {
            "sweep" => Some(Self::Sweep),
            "reversal" => Some(Self::Reversal),
            "crossing" => Some(Self::Crossing),
            _ => None,
        }
    }

    pub(super) fn description(self) -> &'static str {
        match self {
            Self::Sweep => "A full-height vertical edge moves left, center, right; its two temporal wings and middle body cover the viewport once.",
            Self::Reversal => "A full-height vertical edge moves left, right, left; both shutter intervals cover the viewport, while the middle body is offscreen.",
            Self::Crossing => "A top-left/bottom-right diagonal becomes horizontal, then bottom-left/top-right diagonal; masked crossing lobes cover approximately half a viewport per source edge.",
        }
    }

    /// Ideal continuous shaded-area equivalents, counting overlapping intervals
    /// repeatedly, not the union of the image's visible pixels. The crossing
    /// value is only nominal: body width, AA and padded viewport clipping alter
    /// its exact emitted-triangle area. This is not a fragment-counter query.
    pub(super) fn nominal_coverage_screens_per_ribbon(self) -> f64 {
        match self {
            Self::Sweep => 1.0,
            Self::Reversal => 2.0,
            Self::Crossing => 0.5,
        }
    }

    pub(super) fn model(self, width: u32, height: u32, count: usize) -> GpuModelFrame {
        assert!(width > 0 && height > 0, "fixture viewport must be nonzero");
        let mut frame = GpuModelFrame {
            topology: Arc::new(GpuTopology {
                vertex_count: 2,
                edges: vec![[0, 1]; count],
                vertices: Vec::new(),
                bounds: [None; 2],
            }),
            projected: Vec::with_capacity(2),
            projection: None,
            styles: vec![[0.3, 0.8, 1.0, 0.5]; count],
            // Production's capped viewport stroke scale at line-width factor 0.7.
            width: 2.548,
            motion_fade_power: 0.5,
            motion_opacity: MotionOpacity::default().value(),
            at: 1.0,
            reset: gpu_bridge::generation(),
            durations: [INTERVAL_SECONDS; 2],
        };
        self.set_pose(&mut frame, [width, height], 0);
        frame
    }

    pub(super) fn set_pose(self, frame: &mut GpuModelFrame, size: [u32; 2], pose: usize) {
        assert!(pose < 3, "fixture has exactly three shutter poses");
        assert!(
            size[0] > 0 && size[1] > 0,
            "fixture viewport must be nonzero"
        );
        let width = size[0] as f32;
        let height = size[1] as f32;
        let left = -MARGIN;
        let right = width + MARGIN;
        let top = -MARGIN;
        let bottom = height + MARGIN;
        let [a, b] = match self {
            Self::Sweep | Self::Reversal => {
                let x = match (self, pose) {
                    (_, 0) | (Self::Reversal, 2) => left,
                    (Self::Sweep, 1) => width * 0.5,
                    (Self::Sweep, 2) | (Self::Reversal, 1) => right,
                    _ => unreachable!("three-pose vertical history"),
                };
                [[x, top], [x, bottom]]
            }
            Self::Crossing => match pose {
                0 => [[left, top], [right, bottom]],
                1 => [[left, height * 0.5], [right, height * 0.5]],
                2 => [[left, bottom], [right, top]],
                _ => unreachable!("three-pose crossing history"),
            },
        };
        frame.projected.clear();
        frame
            .projected
            .extend([[a[0], a[1], 1.0, 1.0], [b[0], b[1], 1.0, 1.0]]);
        frame.at = 1.0 + pose as f64 / 60.0;
        frame.durations = [INTERVAL_SECONDS; 2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_source_edges_keep_production_ink_and_shared_vertices() {
        for fixture in [
            RibbonFixture::Sweep,
            RibbonFixture::Reversal,
            RibbonFixture::Crossing,
        ] {
            let frame = fixture.model(5120, 2880, 7);
            assert_eq!(frame.topology.vertex_count, 2);
            assert_eq!(frame.topology.edges, vec![[0, 1]; 7]);
            assert_eq!(frame.styles, vec![[0.3, 0.8, 1.0, 0.5]; 7]);
            assert_eq!(frame.width, 2.548);
            assert_eq!(frame.motion_fade_power, 0.5);
            assert_eq!(frame.motion_opacity, MotionOpacity::default().value());
            assert!(frame.projected.iter().all(|v| v[2] == 1.0 && v[3] == 1.0));
            assert_eq!(RibbonFixture::parse(fixture.name()), Some(fixture));
        }
        assert!(RibbonFixture::parse("unknown").is_none());
        assert!(RibbonFixture::Sweep
            .model(5120, 2880, 0)
            .topology
            .edges
            .is_empty());
    }

    #[test]
    fn sweep_spans_the_viewport_once_and_reversal_twice() {
        for fixture in [RibbonFixture::Sweep, RibbonFixture::Reversal] {
            let mut frame = fixture.model(5120, 2880, 1);
            let topology = Arc::clone(&frame.topology);
            let reset = frame.reset;
            let mut positions = Vec::new();
            let mut times = Vec::new();
            for pose in 0..3 {
                fixture.set_pose(&mut frame, [5120, 2880], pose);
                positions.push(frame.projected[0][0]);
                times.push(frame.at);
                assert!(frame.projected[0][1] < 0.0);
                assert!(frame.projected[1][1] > 2880.0);
                assert_eq!(frame.projected[0][0], frame.projected[1][0]);
                assert!(Arc::ptr_eq(&frame.topology, &topology));
                assert_eq!(frame.reset, reset);
            }
            assert!(times.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(positions[0] < 0.0);
            if fixture == RibbonFixture::Sweep {
                assert_eq!(positions[1], 2560.0);
                assert!(positions[2] > 5120.0);
                assert_eq!(fixture.nominal_coverage_screens_per_ribbon(), 1.0);
            } else {
                assert!(positions[1] > 5120.0);
                assert_eq!(positions[0], positions[2]);
                assert_eq!(fixture.nominal_coverage_screens_per_ribbon(), 2.0);
            }
        }
    }

    #[test]
    fn crossing_changes_slope_around_an_actual_horizontal_middle_pose() {
        let mut frame = RibbonFixture::Crossing.model(5120, 2880, 1);
        let mut slopes = Vec::new();
        for pose in 0..3 {
            RibbonFixture::Crossing.set_pose(&mut frame, [5120, 2880], pose);
            let [a, b] = [frame.projected[0], frame.projected[1]];
            slopes.push((b[1] - a[1]) / (b[0] - a[0]));
            assert!(a[0] < 0.0 && b[0] > 5120.0);
        }
        assert!(slopes[0] > 0.0);
        assert_eq!(slopes[1], 0.0);
        assert!(slopes[2] < 0.0);
        assert_eq!(
            RibbonFixture::Crossing.nominal_coverage_screens_per_ribbon(),
            0.5
        );
    }
}
