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
        fixtures.push(ModelFixture {
            id: model.id.clone(),
            wireframe,
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
    /// allocator setup in the timed loop.
    pub fn prepare(&self, width: u32, height: u32, motion: Motion) -> FrozenPair {
        let (first_pose, last_pose) = motion.poses(height as f32 / 1080.0);
        let mut pair = FrozenPair {
            edges: Vec::with_capacity(self.edge_count()),
            first_vertices: Vec::with_capacity(self.vertex_count()),
            last_vertices: Vec::with_capacity(self.vertex_count()),
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
}

impl Detail {
    pub const ALL: [Self; 3] = [Self::Sharp, Self::Full, Self::LowAlpha];

    pub fn name(self) -> &'static str {
        match self {
            Self::Sharp => "sharp",
            Self::Full => "full_blur",
            Self::LowAlpha => "alpha_10pct",
        }
    }
}

pub(super) struct FrozenPair {
    edges: Vec<(LineGeometry, LineGeometry)>,
    first_vertices: Vec<(super::super::Projected, f32)>,
    last_vertices: Vec<(super::super::Projected, f32)>,
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
        })
    }

    /// Reuses emitted-geometry capacity after warmup. Every variant shares
    /// identical projected exposure endpoints.
    pub fn build(&self, detail: Detail, output: &mut FrozenOutput) {
        output.lines.clear();
        output.ribbons.clear();
        output.retained_edges = 0;
        let fade = MotionFade {
            opacity: MotionOpacity::default(),
            power: MotionFadePower::default(),
        };
        for &(first, last) in &self.edges {
            if !first.visible || !last.visible {
                continue;
            }
            output.retained_edges += 1;
            let ink = SweepInk {
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
            } else {
                push_sweep(first, last, ink, fade, self.bounds, &mut output.ribbons);
            }
        }
    }
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
            model_edge_capacity: 0,
            gpu_active: false,
            gpu_models: &[],
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
        }
    }

    #[test]
    fn fixed_pairs_keep_complete_repeatable_geometry() {
        let model = fixture();
        let pair = model.prepare(1920, 1080, Motion::Translate);
        let mut output = FrozenOutput::default();
        pair.build(Detail::Full, &mut output);
        let full = output.ribbons.clone();
        pair.build(Detail::Full, &mut output);
        assert_eq!(output.ribbons, full);
        assert_eq!(output.retained_edges, 4);
    }

    #[test]
    fn low_alpha_keeps_geometry_and_swept_coverage() {
        let pair = fixture().prepare(1920, 1080, Motion::Translate);
        let mut output = FrozenOutput::default();
        pair.build(Detail::Full, &mut output);
        let count = output.ribbons.len();
        let area = output.triangle_area_px2();
        pair.build(Detail::LowAlpha, &mut output);
        assert_eq!(output.ribbons.len(), count);
        assert_eq!(output.triangle_area_px2(), area);
        assert_eq!(output.retained_edges, 4);
        let still = fixture().prepare(1920, 1080, Motion::Still);
        assert_eq!(still.max_displacement_px, 0.0);
    }
}
