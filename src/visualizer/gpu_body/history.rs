//! Previous displayed body poses, with the same identity and reseeding rules
//! as the wireframe's three-pose GPU history.

use std::sync::{Arc, Weak};

use super::super::gpu_bridge::{GpuModelFrame, GpuProjection, GpuTopology};

struct BodyPose {
    topology: Weak<GpuTopology>,
    reset: u64,
    viewport: [u32; 2],
    at: f64,
    projection: GpuProjection,
}

#[derive(Default)]
pub(super) struct BodyProjectionHistory {
    poses: Vec<BodyPose>,
    retired: Vec<BodyPose>,
}

impl BodyProjectionHistory {
    pub fn begin_frame(&mut self) {
        std::mem::swap(&mut self.poses, &mut self.retired);
        self.retired.append(&mut self.poses);
    }

    /// Advance even when body opacity is zero, just as wireframe history does.
    pub fn projection(
        &mut self,
        frame: &GpuModelFrame,
        viewport: [u32; 2],
        current: GpuProjection,
    ) -> GpuProjection {
        let topology = Arc::downgrade(&frame.topology);
        let previous = self
            .retired
            .iter()
            .position(|pose| pose.topology.ptr_eq(&topology) && pose.reset == frame.reset)
            .map(|index| self.retired.remove(index));
        let projection = previous
            .filter(|pose| {
                pose.viewport == viewport
                    && pose.at.is_finite()
                    && frame.at.is_finite()
                    && frame.at > pose.at
                    && frame.at - pose.at <= 0.25
            })
            .map_or(current, |pose| pose.projection);
        self.poses.push(BodyPose {
            topology,
            reset: frame.reset,
            viewport,
            at: frame.at,
            projection: current,
        });
        projection
    }

    pub fn finish_frame(&mut self) {
        self.retired.retain(|pose| pose.topology.strong_count() > 0);
        self.retired
            .truncate(2_usize.saturating_sub(self.poses.len()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> GpuModelFrame {
        let mut frame = GpuModelFrame::new(Arc::new(GpuTopology {
            body: None,
            edges: Vec::new(),
            vertex_count: 0,
            vertices: Vec::new(),
            bounds: [None; 2],
        }));
        frame.at = 1.0;
        frame
    }

    fn pose(value: f32) -> GpuProjection {
        GpuProjection {
            rows: [[value; 4], [value + 1.0; 4], [value + 2.0; 4]],
            camera: [value + 3.0; 4],
            depth: [value + 4.0; 4],
            slide: [value + 5.0; 4],
        }
    }

    fn assert_pose(actual: GpuProjection, expected: GpuProjection) {
        assert_eq!(bytemuck::bytes_of(&actual), bytemuck::bytes_of(&expected));
    }

    fn advance(
        history: &mut BodyProjectionHistory,
        frame: &GpuModelFrame,
        viewport: [u32; 2],
        projection: GpuProjection,
    ) -> GpuProjection {
        history.begin_frame();
        let previous = history.projection(frame, viewport, projection);
        history.finish_frame();
        previous
    }

    #[test]
    fn previous_displayed_pose_includes_camera_and_slide_even_while_invisible() {
        let mut history = BodyProjectionHistory::default();
        let mut frame = frame();
        assert_pose(
            advance(&mut history, &frame, [64, 64], pose(1.0)),
            pose(1.0),
        );
        frame.at = 1.02;
        frame.body_opacity = 0.0;
        assert_pose(
            advance(&mut history, &frame, [64, 64], pose(2.0)),
            pose(1.0),
        );
        frame.at = 1.04;
        frame.body_opacity = 1.0;
        assert_pose(
            advance(&mut history, &frame, [64, 64], pose(3.0)),
            pose(2.0),
        );
    }

    #[test]
    fn current_outgoing_exchange_preserves_each_generation_of_shared_geometry() {
        let mut history = BodyProjectionHistory::default();
        let mut outgoing = frame();
        advance(&mut history, &outgoing, [64, 64], pose(1.0));
        let mut incoming = GpuModelFrame::new(Arc::clone(&outgoing.topology));
        outgoing.at = 1.02;
        incoming.at = 1.02;
        history.begin_frame();
        assert_pose(
            history.projection(&incoming, [64, 64], pose(10.0)),
            pose(10.0),
        );
        assert_pose(
            history.projection(&outgoing, [64, 64], pose(2.0)),
            pose(1.0),
        );
        history.finish_frame();
        outgoing.at = 1.04;
        incoming.at = 1.04;
        history.begin_frame();
        assert_pose(
            history.projection(&outgoing, [64, 64], pose(3.0)),
            pose(2.0),
        );
        assert_pose(
            history.projection(&incoming, [64, 64], pose(11.0)),
            pose(10.0),
        );
        history.finish_frame();
    }

    #[test]
    fn resize_reset_seek_and_invalid_times_reseed_to_current() {
        for case in 0..9 {
            let mut history = BodyProjectionHistory::default();
            let mut frame = frame();
            advance(&mut history, &frame, [64, 64], pose(1.0));
            frame.at = 1.02;
            let mut viewport = [64, 64];
            match case {
                0 => viewport = [128, 64],
                1 => frame.reset += 1,
                2 => frame.at = 1.0,
                3 => frame.at = 0.5,
                4 => frame.at = 1.26,
                5 => frame.at = f64::NAN,
                6 => frame.at = f64::INFINITY,
                7 => frame.topology = super::tests::frame().topology,
                8 => {
                    frame.at = f64::NAN;
                    advance(&mut history, &frame, viewport, pose(2.0));
                    frame.at = 1.02;
                }
                _ => unreachable!(),
            }
            assert_pose(
                advance(&mut history, &frame, viewport, pose(3.0)),
                pose(3.0),
            );
        }
    }

    #[test]
    fn temporary_empty_frames_retain_history_without_retaining_geometry() {
        let mut history = BodyProjectionHistory::default();
        let mut frame = frame();
        advance(&mut history, &frame, [64, 64], pose(1.0));
        history.begin_frame();
        history.finish_frame();
        frame.at = 1.02;
        assert_pose(
            advance(&mut history, &frame, [64, 64], pose(2.0)),
            pose(1.0),
        );
        let source = Arc::downgrade(&frame.topology);
        drop(frame);
        history.begin_frame();
        history.finish_frame();
        assert!(source.upgrade().is_none());
        assert!(history.retired.is_empty());
    }
}
