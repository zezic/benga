//! Fixed geometry for the motion benchmark. Both exposure endpoints are explicit;
//! no audio, frame pacing, random animation, or history lookup enters a sample.

use super::super::{
    geometry::{Camera, DepthOpacity, Pose, ScreenBounds, Vec2, Wireframe},
    lerp_line,
    model::ModelSet,
    push_sweep, Adjustment, CreaseThreshold, Frame, LineGeometry, LineInstance, LineWidth,
    ModelComposite, MotionFade, MotionFadePower, MotionOpacity, RibbonInstance, Rng, SweepInk,
};

pub(super) struct ModelFixture {
    pub id: String,
    wireframe: Wireframe,
    /// Rank 0 is the longest model-space edge. Equal lengths use stable edge order.
    length_ranks: Vec<usize>,
}

/// Rank actual extracted wireframes, rather than GLB file size or mesh triangles.
/// The seed affects catalog starting position only; every model is visited once.
pub(super) fn load_ranked() -> anyhow::Result<Vec<ModelFixture>> {
    let mut models = ModelSet::load(&mut Rng::new(0x4245_4e47_415f_5045))?;
    let count = models.position().1;
    let mut fixtures = Vec::with_capacity(count);
    for _ in 0..count {
        let model = models.current();
        let wireframe = model.wireframe(CreaseThreshold::default());
        let mut sorted: Vec<_> = wireframe
            .edges()
            .iter()
            .enumerate()
            .map(|(index, &[a, b])| {
                let length = (wireframe.vertex(a as usize) - wireframe.vertex(b as usize)).length();
                (index, length)
            })
            .collect();
        sorted.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut length_ranks = vec![0; sorted.len()];
        for (rank, &(index, _)) in sorted.iter().enumerate() {
            length_ranks[index] = rank;
        }
        fixtures.push(ModelFixture {
            id: model.id.clone(),
            wireframe,
            length_ranks,
        });
        models.navigate(Adjustment::Increase);
    }
    fixtures.sort_by(|a, b| b.edge_count().cmp(&a.edge_count()).then(a.id.cmp(&b.id)));
    Ok(fixtures)
}

impl ModelFixture {
    pub fn vertex_count(&self) -> usize {
        self.wireframe.vertex_count()
    }

    pub fn edge_count(&self) -> usize {
        self.wireframe.edges().len()
    }

    /// Build a frozen pair once. Use `reproject` to measure projection without
    /// allocator or object-length ranking setup in the timed loop.
    pub fn prepare(&self, width: u32, height: u32, motion: Motion) -> FrozenPair {
        let (first_pose, last_pose) = motion.poses(height as f32 / 1080.0);
        let mut pair = FrozenPair {
            edges: Vec::with_capacity(self.edge_count()),
            first_vertices: Vec::with_capacity(self.vertex_count()),
            last_vertices: Vec::with_capacity(self.vertex_count()),
            length_ranks: self.length_ranks.clone(),
            bounds: ScreenBounds::new(Vec2::new(width as f32, height as f32)),
            line_width: 0.0,
            camera: Camera::new(width as f32, height as f32, motion.zoom()),
            camera_zoom: motion.zoom(),
            first_pose,
            last_pose,
            mean_displacement_px: 0.0,
            max_displacement_px: 0.0,
        };
        self.reproject(width, height, motion, &mut pair);
        pair
    }

    /// Project two fixed exposure poses using the production camera and depth
    /// opacity, retaining all vector capacities after the initial prepare.
    pub fn reproject(&self, width: u32, height: u32, motion: Motion, pair: &mut FrozenPair) {
        assert_eq!(pair.length_ranks.len(), self.edge_count());
        let camera = Camera::new(width as f32, height as f32, motion.zoom());
        let (first_pose, last_pose) = motion.poses(height as f32 / 1080.0);
        let project = |pose: Pose, vertices: &mut Vec<(super::super::Projected, f32)>| {
            let depth = DepthOpacity::centered(pose.distance, pose.scale, 0.5);
            vertices.clear();
            vertices.extend((0..self.wireframe.vertex_count()).map(|index| {
                let vertex = camera.project(self.wireframe.vertex(index), pose);
                (vertex, depth.factor(vertex.depth))
            }));
        };
        project(first_pose, &mut pair.first_vertices);
        project(last_pose, &mut pair.last_vertices);
        let mut mean_displacement_px = 0.0;
        let mut max_displacement_px = 0.0_f32;
        pair.edges.clear();
        for &[a, b] in self.wireframe.edges() {
            let line = |vertices: &[(super::super::Projected, f32)]| {
                let (a, a_opacity) = vertices[a as usize];
                let (b, b_opacity) = vertices[b as usize];
                LineGeometry {
                    start: a.screen,
                    end: b.screen,
                    start_depth_opacity: a_opacity,
                    end_depth_opacity: b_opacity,
                    visible: a.visible && b.visible,
                }
            };
            let first = line(&pair.first_vertices);
            let last = line(&pair.last_vertices);
            let displacement = (last.start - first.start)
                .length()
                .max((last.end - first.end).length());
            mean_displacement_px += displacement;
            max_displacement_px = max_displacement_px.max(displacement);
            pair.edges.push((first, last));
        }
        mean_displacement_px /= self.edge_count().max(1) as f32;
        // Matches Visualizer::update: clamp viewport scale before applying the
        // persisted default 60% line width.
        pair.line_width = (width.min(height) as f32 / 440.0).clamp(0.62, 2.6)
            * 1.4
            * LineWidth::default().factor();
        pair.bounds = ScreenBounds::new(camera.viewport);
        pair.camera = camera;
        pair.camera_zoom = motion.zoom();
        pair.first_pose = first_pose;
        pair.last_pose = last_pose;
        pair.mean_displacement_px = mean_displacement_px;
        pair.max_displacement_px = max_displacement_px;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Motion {
    Still,
    Mild,
    Fast,
    ZoomedFast,
    Rotate90,
    Flip160,
    Translate,
}

impl Motion {
    pub const ALL: [Self; 7] = [
        Self::Still,
        Self::Mild,
        Self::Fast,
        Self::ZoomedFast,
        Self::Rotate90,
        Self::Flip160,
        Self::Translate,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Still => "still",
            Self::Mild => "mild_rotation",
            Self::Fast => "fast_rotation",
            Self::ZoomedFast => "zoomed_fast_rotation",
            Self::Rotate90 => "rotate_90",
            Self::Flip160 => "flip_160",
            Self::Translate => "translation",
        }
    }

    fn zoom(self) -> f32 {
        if self == Self::ZoomedFast {
            2.0
        } else {
            1.0
        }
    }

    fn poses(self, pixel_scale: f32) -> (Pose, Pose) {
        let first = Pose::default();
        let mut last = first;
        let (yaw, pitch, roll): (f32, f32, f32) = match self {
            Self::Still => (0.0, 0.0, 0.0),
            Self::Mild => (2.0, 1.0, 0.0),
            Self::Fast | Self::ZoomedFast => (30.0, 18.0, 8.0),
            Self::Rotate90 => (90.0, 60.0, 45.0),
            Self::Flip160 => (10.0, 160.0, 0.0),
            Self::Translate => {
                last.shake = Vec2::new(240.0, 120.0) * pixel_scale;
                (0.0, 0.0, 0.0)
            }
        };
        last.yaw += yaw.to_radians();
        last.pitch += pitch.to_radians();
        last.roll += roll.to_radians();
        (first, last)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Detail {
    Sharp,
    Full,
    LowAlpha,
    Retain75,
    Retain50,
    Retain25,
    SmallBodies,
    BodyOnly,
    Uniform25,
    Cap64,
}

impl Detail {
    pub const ALL: [Self; 10] = [
        Self::Sharp,
        Self::Full,
        Self::LowAlpha,
        Self::Retain75,
        Self::Retain50,
        Self::Retain25,
        Self::SmallBodies,
        Self::BodyOnly,
        Self::Uniform25,
        Self::Cap64,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Sharp => "sharp",
            Self::Full => "full_blur",
            Self::LowAlpha => "alpha_10pct",
            Self::Retain75 => "longest_75pct",
            Self::Retain50 => "longest_50pct",
            Self::Retain25 => "longest_25pct",
            Self::SmallBodies => "small_half_body_only",
            Self::BodyOnly => "body_only",
            Self::Uniform25 => "uniform_25pct",
            Self::Cap64 => "blur_cap_64px",
        }
    }

    fn retained_percent(self) -> usize {
        match self {
            Self::Retain75 => 75,
            Self::Retain50 => 50,
            Self::Retain25 => 25,
            _ => 100,
        }
    }
}

pub(super) struct FrozenPair {
    edges: Vec<(LineGeometry, LineGeometry)>,
    first_vertices: Vec<(super::super::Projected, f32)>,
    last_vertices: Vec<(super::super::Projected, f32)>,
    length_ranks: Vec<usize>,
    bounds: ScreenBounds,
    pub line_width: f32,
    camera: Camera,
    camera_zoom: f32,
    pub first_pose: Pose,
    pub last_pose: Pose,
    pub mean_displacement_px: f32,
    pub max_displacement_px: f32,
}

impl FrozenPair {
    /// Explicit synthetic settings and endpoint poses for the result artifact.
    pub fn settings(&self) -> serde_json::Value {
        let pose = |pose: Pose| {
            serde_json::json!({
                "yaw_deg": pose.yaw.to_degrees(),
                "pitch_deg": pose.pitch.to_degrees(),
                "roll_deg": pose.roll.to_degrees(),
                "scale": pose.scale,
                "distance": pose.distance,
                "shake_px": [pose.shake.x, pose.shake.y],
            })
        };
        serde_json::json!({
            "first_pose": pose(self.first_pose),
            "last_pose": pose(self.last_pose),
            "viewport_px": [self.camera.viewport.x, self.camera.viewport.y],
            "camera_focal_px": self.camera.focal,
            "camera_zoom": self.camera_zoom,
            "line_width_factor": LineWidth::default().factor(),
            "line_width_px": self.line_width,
            "motion_opacity_floor": MotionOpacity::default().value(),
            "motion_fade_power": MotionFadePower::default().power(),
            "crease_threshold_deg": CreaseThreshold::default().degrees(),
            "depth_fade": 0.5,
            "base_alpha": 1.0,
            "rgb": [0.3, 0.8, 1.0],
            "sharp_reference": "interpolated_exposure_midpoint",
            "blur_cap_control_px": 64.0,
            "uniform_retention_control": "top_two_bits_zero_of_splitmix64_edge_index",
        })
    }

    /// Reuses emitted-geometry capacity after warmup. Every variant shares
    /// identical projected exposure endpoints and object-space length ranking.
    pub fn build(&self, detail: Detail, output: &mut FrozenOutput) {
        output.lines.clear();
        output.ribbons.clear();
        output.retained_edges = 0;
        let fade = MotionFade {
            opacity: MotionOpacity::default(),
            power: MotionFadePower::default(),
        };
        let retained_count = self.edges.len() * detail.retained_percent() / 100;
        let body_rank = self.edges.len() / 2;
        for (index, &(first, last)) in self.edges.iter().enumerate() {
            if !first.visible
                || !last.visible
                || self.length_ranks[index] >= retained_count
                || (detail == Detail::Uniform25 && !uniform_quarter(index))
            {
                continue;
            }
            output.retained_edges += 1;
            let mut ink = SweepInk {
                rgb: [0.3, 0.8, 1.0],
                alpha: if detail == Detail::LowAlpha { 0.1 } else { 1.0 },
                width: self.line_width,
            };
            if detail == Detail::Sharp {
                // Match the full sweep's median body for a controlled footprint
                // comparison; ordinary no-blur rendering uses its current pose.
                let middle = lerp_line(first, last, 0.5);
                let line = LineInstance::flat(
                    middle.start,
                    middle.end,
                    [
                        ink.rgb[0],
                        ink.rgb[1],
                        ink.rgb[2],
                        middle.start_depth_opacity,
                    ],
                    [ink.rgb[0], ink.rgb[1], ink.rgb[2], middle.end_depth_opacity],
                    ink.width,
                );
                if line.intersects(self.bounds) {
                    output.lines.push(line);
                }
            } else if detail == Detail::BodyOnly
                || (detail == Detail::SmallBodies && self.length_ranks[index] >= body_rank)
            {
                // Preserve the full sweep's peak attenuation, then draw only its
                // oriented median body through the same production sweep path.
                let movement = (last.start - first.start)
                    .length()
                    .max((last.end - first.end).length());
                ink.alpha *= fade.factor(movement, ink.width);
                let middle = lerp_line(first, last, 0.5);
                push_sweep(middle, middle, ink, fade, self.bounds, &mut output.ribbons);
            } else if detail == Detail::Cap64 {
                let (capped_first, capped_last) = cap_sweep(first, last, 64.0);
                // Shortening the exposure geometry must not brighten the peak:
                // divide out the cap's new falloff before push_sweep applies it.
                let original_movement = sweep_movement(first, last);
                let capped_movement = sweep_movement(capped_first, capped_last);
                ink.alpha *= fade.factor(original_movement, ink.width)
                    / fade.factor(capped_movement, ink.width);
                push_sweep(
                    capped_first,
                    capped_last,
                    ink,
                    fade,
                    self.bounds,
                    &mut output.ribbons,
                );
            } else {
                push_sweep(first, last, ink, fade, self.bounds, &mut output.ribbons);
            }
        }
    }
}

/// A topology edge index deterministically picks its membership. No animation
/// state or random generator is involved, so moving the model cannot reshuffle it.
fn uniform_quarter(index: usize) -> bool {
    let mut hash = (index as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
    hash = (hash ^ (hash >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    hash = (hash ^ (hash >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    hash ^= hash >> 31;
    hash >> 62 == 0
}

fn sweep_movement(first: LineGeometry, last: LineGeometry) -> f32 {
    (last.start - first.start)
        .length()
        .max((last.end - first.end).length())
}

/// Contract both exposure endpoints around their unchanged midpoint. Endpoint
/// depths use the same interpolation as screen positions, preserving the body's
/// depth opacity while making the maximum endpoint displacement at most `cap`.
fn cap_sweep(first: LineGeometry, last: LineGeometry, cap: f32) -> (LineGeometry, LineGeometry) {
    let movement = sweep_movement(first, last);
    if movement <= cap {
        return (first, last);
    }
    let middle = lerp_line(first, last, 0.5);
    let fraction = cap / movement;
    (
        lerp_line(middle, first, fraction),
        lerp_line(middle, last, fraction),
    )
}

#[derive(Default)]
pub(super) struct FrozenOutput {
    pub lines: Vec<LineInstance>,
    pub ribbons: Vec<RibbonInstance>,
    pub retained_edges: usize,
}

impl FrozenOutput {
    pub fn frame(&self, composite: ModelComposite) -> Frame<'_> {
        Frame {
            lines: &[],
            model_lines: &self.lines,
            ribbons: &self.ribbons,
            model_composite: composite,
        }
    }

    /// Sum of actual emitted triangle areas, including overlaps. This is an
    /// overdraw proxy, not a fragment count; triangles are not viewport-clipped.
    pub fn triangle_area_px2(&self) -> f64 {
        let ribbons: f64 = self
            .ribbons
            .iter()
            .map(|ribbon| {
                let [ax, ay, bx, by] = ribbon.start;
                let [cx, cy] = ribbon.end;
                f64::from(((bx - ax) * (cy - ay) - (by - ay) * (cx - ax)).abs()) * 0.5
            })
            .sum();
        // Crisp lines actually rasterize the shader's axis-aligned padded quad,
        // not an oriented line-width strip. Include that footprint for controls.
        let lines: f64 = self
            .lines
            .iter()
            .map(|line| {
                let [ax, ay, bx, by] = line.endpoints;
                let padding = line.style[0].max(1.0) * 0.5 + 1.5;
                f64::from(((bx - ax).abs() + 2.0 * padding) * ((by - ay).abs() + 2.0 * padding))
            })
            .sum();
        ribbons + lines
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::geometry::Vec3;
    use super::*;

    fn fixture() -> ModelFixture {
        ModelFixture {
            id: "fixture".to_owned(),
            wireframe: Wireframe::from_edges(
                vec![
                    Vec3::new(-0.8, -0.3, 0.0),
                    Vec3::new(0.8, 0.3, 0.0),
                    Vec3::new(-0.1, -0.4, 0.1),
                    Vec3::new(0.1, 0.4, 0.1),
                    Vec3::new(-0.05, -0.2, 0.2),
                    Vec3::new(0.05, 0.2, 0.2),
                    Vec3::new(-0.025, -0.1, 0.3),
                    Vec3::new(0.025, 0.1, 0.3),
                ],
                vec![[0, 1], [2, 3], [4, 5], [6, 7]],
            ),
            length_ranks: vec![0, 1, 2, 3],
        }
    }

    #[test]
    fn fixed_pairs_are_repeatable_and_length_culling_removes_geometry() {
        let model = fixture();
        let pair = model.prepare(1920, 1080, Motion::Translate);
        let mut output = FrozenOutput::default();
        pair.build(Detail::Full, &mut output);
        let full = output.ribbons.clone();
        let full_area = output.triangle_area_px2();
        pair.build(Detail::Full, &mut output);
        assert_eq!(output.ribbons, full);
        assert_eq!(output.retained_edges, 4);
        pair.build(Detail::Retain25, &mut output);
        assert_eq!(output.retained_edges, 1);
        assert_eq!(output.ribbons.len(), full.len() / 4);
        assert!(output.triangle_area_px2() < full_area);
    }

    #[test]
    fn low_alpha_keeps_geometry_and_small_bodies_reduce_swept_coverage() {
        let pair = fixture().prepare(1920, 1080, Motion::Translate);
        let mut output = FrozenOutput::default();
        pair.build(Detail::Full, &mut output);
        let count = output.ribbons.len();
        let area = output.triangle_area_px2();
        pair.build(Detail::LowAlpha, &mut output);
        assert_eq!(output.ribbons.len(), count);
        assert_eq!(output.triangle_area_px2(), area);
        pair.build(Detail::SmallBodies, &mut output);
        assert_eq!(output.retained_edges, 4);
        assert!(output.ribbons.len() < count);
        assert!(output.triangle_area_px2() < area);
        let still = fixture().prepare(1920, 1080, Motion::Still);
        assert_eq!(still.max_displacement_px, 0.0);
    }

    #[test]
    fn uniform_retention_is_stable_and_reduces_actual_geometry() {
        let retained = (0..10_000).filter(|&index| uniform_quarter(index)).count();
        assert!((2_300..=2_700).contains(&retained));
        let pair = fixture().prepare(1920, 1080, Motion::Translate);
        let mut output = FrozenOutput::default();
        pair.build(Detail::Full, &mut output);
        let full_count = output.ribbons.len();
        let full_area = output.triangle_area_px2();
        pair.build(Detail::Uniform25, &mut output);
        let kept = output.ribbons.clone();
        assert_eq!(output.retained_edges, 1);
        assert_eq!(output.ribbons.len(), full_count / 4);
        assert!(output.triangle_area_px2() < full_area);
        pair.build(Detail::Uniform25, &mut output);
        assert_eq!(output.ribbons, kept);
    }

    #[test]
    fn sweep_cap_preserves_midpoint_and_peak_but_reduces_area() {
        let pair = fixture().prepare(1920, 1080, Motion::Translate);
        let mut output = FrozenOutput::default();
        pair.build(Detail::Full, &mut output);
        let full_area = output.triangle_area_px2();
        let peak = |output: &FrozenOutput| {
            output
                .ribbons
                .iter()
                .flat_map(|ribbon| {
                    [
                        ribbon.start_color[3],
                        ribbon.end_color[3],
                        ribbon.third_color[3],
                    ]
                })
                .fold(0.0_f32, f32::max)
        };
        let full_peak = peak(&output);
        pair.build(Detail::Cap64, &mut output);
        assert_eq!(output.retained_edges, 4);
        assert!(output.triangle_area_px2() < full_area);
        assert!((peak(&output) - full_peak).abs() < 1e-5);
        for &(first, last) in &pair.edges {
            let (capped_first, capped_last) = cap_sweep(first, last, 64.0);
            assert!((sweep_movement(capped_first, capped_last) - 64.0).abs() < 1e-3);
            let original_middle = lerp_line(first, last, 0.5);
            let capped_middle = lerp_line(capped_first, capped_last, 0.5);
            assert!((original_middle.start - capped_middle.start).length() < 1e-3);
            assert!((original_middle.end - capped_middle.end).length() < 1e-3);
            assert!(
                (original_middle.start_depth_opacity - capped_middle.start_depth_opacity).abs()
                    < 1e-6
            );
            assert!(
                (original_middle.end_depth_opacity - capped_middle.end_depth_opacity).abs() < 1e-6
            );
        }
        let still = fixture().prepare(1920, 1080, Motion::Still);
        still.build(Detail::Full, &mut output);
        let full = output.ribbons.clone();
        still.build(Detail::Cap64, &mut output);
        assert_eq!(output.ribbons, full);
    }
}
