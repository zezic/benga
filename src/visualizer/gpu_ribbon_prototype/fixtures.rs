//! Throwaway motion cases, the unchanged CPU sweep, and a sampled reference.
//!
//! The reference integrates a normalized tent shutter. It is a shape/energy
//! reference, not a pixel-identical reproduction of the production ribbon style.

use anyhow::Context;

use super::super::{
    geometry::{Camera, DepthOpacity, Pose, ScreenBounds, Vec2, Vec3, Wireframe},
    model::ModelSet,
    push_sweep, Adjustment, CreaseThreshold, Frame, LineGeometry, LineInstance, LineWidth,
    ModelComposite, MotionFade, MotionFadePower, MotionOpacity, RibbonInstance, Rng, SweepInk,
};

pub(super) const RGB: [f32; 3] = [0.3, 0.8, 1.0];
pub(super) const DEPTH_FADE: f32 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MotionCase {
    Still,
    Mild,
    Fast,
    Translate,
    AlongEdge,
    Rotate90,
    Reverse,
}

impl MotionCase {
    pub const ALL: [Self; 7] = [
        Self::Still,
        Self::Mild,
        Self::Fast,
        Self::Translate,
        Self::AlongEdge,
        Self::Rotate90,
        Self::Reverse,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Still => "still",
            Self::Mild => "mild_rotation",
            Self::Fast => "fast_rotation",
            Self::Translate => "translation",
            Self::AlongEdge => "along_edge_translation",
            Self::Rotate90 => "rotate_90",
            Self::Reverse => "reversal",
        }
    }

    /// At frame -2 and 0, Still/Mild/Fast/Translate/Rotate90 match the existing
    /// frozen benchmark's exposure endpoints. Intermediate samples follow real
    /// rotations; Reverse deliberately has equal endpoints and a moving middle.
    pub fn pose_at(self, frame: f32, height: u32) -> Pose {
        let mut pose = Pose::default();
        let fraction = (frame + 2.0) * 0.5;
        let (yaw, pitch, roll): (f32, f32, f32) = match self {
            Self::Still => (0.0, 0.0, 0.0),
            Self::Mild => (2.0, 1.0, 0.0),
            Self::Fast => (30.0, 18.0, 8.0),
            Self::Rotate90 => (90.0, 60.0, 45.0),
            Self::Translate => {
                pose.shake = Vec2::new(240.0, 120.0) * (height as f32 / 1080.0 * fraction);
                (0.0, 0.0, 0.0)
            }
            Self::AlongEdge => {
                let camera = Camera::new(1920.0, 1080.0, 1.0);
                let a = camera.project(Vec3::new(-0.85, -0.2, 0.0), pose).screen;
                let b = camera.project(Vec3::new(0.85, 0.2, 0.0), pose).screen;
                let direction = b - a;
                pose.shake =
                    direction * (240.0 * height as f32 / 1080.0 * fraction / direction.length());
                (0.0, 0.0, 0.0)
            }
            Self::Reverse => {
                let excursion = (std::f32::consts::PI * fraction).sin();
                pose.shake = Vec2::new(150.0, -100.0) * (height as f32 / 1080.0 * excursion);
                pose.yaw += 50.0_f32.to_radians() * excursion;
                pose.roll += 25.0_f32.to_radians() * excursion;
                return pose;
            }
        };
        pose.yaw += yaw.to_radians() * fraction;
        pose.pitch += pitch.to_radians() * fraction;
        pose.roll += roll.to_radians() * fraction;
        pose
    }
}

pub(super) fn camera(width: u32, height: u32) -> Camera {
    Camera::new(width as f32, height as f32, 1.0)
}

pub(super) fn line_width(width: u32, height: u32) -> f32 {
    (width.min(height) as f32 / 440.0).clamp(0.62, 2.6) * 1.4 * LineWidth::default().factor()
}

pub(super) struct ModelFixture {
    pub id: String,
    /// xyz position followed by the semantic part ID as a float.
    pub vertices: Vec<[f32; 4]>,
    pub edges: Vec<[u32; 2]>,
}

pub(super) fn load_model(id: Option<&str>) -> anyhow::Result<ModelFixture> {
    let mut models = ModelSet::load(&mut Rng::new(0x4750_555f_5249_4242))?;
    let count = models.position().1;
    let mut best = None;
    for _ in 0..count {
        let model = models.current();
        if id.is_none_or(|id| model.id == id) {
            let fixture = ModelFixture::new(
                model.id.clone(),
                model.wireframe(CreaseThreshold::default()),
            );
            if id.is_some()
                || best
                    .as_ref()
                    .is_none_or(|old: &ModelFixture| old.edges.len() < fixture.edges.len())
            {
                best = Some(fixture);
            }
            if id.is_some() {
                break;
            }
        }
        models.navigate(Adjustment::Increase);
    }
    best.context("no matching model for GPU ribbon prototype")
}

impl ModelFixture {
    pub fn new(id: String, wireframe: Wireframe) -> Self {
        let vertices = (0..wireframe.vertex_count())
            .map(|index| {
                let vertex = wireframe.vertex(index);
                [
                    vertex.x,
                    vertex.y,
                    vertex.z,
                    f32::from(wireframe.vertex_part(index)),
                ]
            })
            .collect();
        let edges = wireframe
            .edges()
            .iter()
            .map(|&[a, b]| [u32::from(a), u32::from(b)])
            .collect();
        Self {
            id,
            vertices,
            edges,
        }
    }

    /// One long diagonal edge, exposing stationary thickness and folded sweeps.
    pub fn single_edge() -> Self {
        Self::new(
            "synthetic_single_edge".into(),
            Wireframe::from_parts(
                vec![Vec3::new(-0.85, -0.2, 0.0), Vec3::new(0.85, 0.2, 0.0)],
                vec![[0, 1]],
                vec![0; 2],
                vec![0],
                1,
            ),
        )
    }

    /// Two visibly crossing edges at different depths, retaining both motions.
    pub fn crossing_edges() -> Self {
        Self::new(
            "synthetic_crossing_edges".into(),
            Wireframe::from_parts(
                vec![
                    Vec3::new(-0.75, -0.3, -0.15),
                    Vec3::new(0.75, 0.3, -0.15),
                    Vec3::new(-0.3, 0.75, 0.15),
                    Vec3::new(0.3, -0.75, 0.15),
                ],
                vec![[0, 1], [2, 3]],
                vec![0; 4],
                vec![0; 2],
                1,
            ),
        )
    }

    /// CPU projection record matches the prototype's proposed 16-byte GPU
    /// history layout: screen x/y, depth opacity, visibility.
    pub fn project_vertices(&self, width: u32, height: u32, pose: Pose) -> Vec<[f32; 4]> {
        let mut projected = Vec::with_capacity(self.vertices.len());
        self.project_into(width, height, pose, &mut projected);
        projected
    }

    fn project_into(&self, width: u32, height: u32, pose: Pose, out: &mut Vec<[f32; 4]>) {
        let camera = camera(width, height);
        let opacity = DepthOpacity::centered(pose.distance, pose.scale, DEPTH_FADE);
        out.clear();
        out.extend(self.vertices.iter().map(|vertex| {
            let projected = camera.project(Vec3::new(vertex[0], vertex[1], vertex[2]), pose);
            [
                projected.screen.x,
                projected.screen.y,
                opacity.factor(projected.depth),
                if projected.visible { 1.0 } else { 0.0 },
            ]
        }));
    }

    pub fn baseline(
        &self,
        width: u32,
        height: u32,
        case: MotionCase,
        frame: f32,
        alpha: f32,
    ) -> Output {
        let mut output = Output::default();
        self.build_baseline(width, height, case, frame, alpha, &mut output);
        output
    }

    /// Capacity-reusing path for timing the real CPU sweep, including projection.
    pub fn build_baseline(
        &self,
        width: u32,
        height: u32,
        case: MotionCase,
        frame: f32,
        alpha: f32,
        output: &mut Output,
    ) {
        self.build_baseline_from_poses(
            width,
            height,
            case.pose_at(frame - 2.0, height),
            case.pose_at(frame, height),
            alpha,
            output,
        );
    }

    pub fn build_baseline_from_poses(
        &self,
        width: u32,
        height: u32,
        first: Pose,
        last: Pose,
        alpha: f32,
        output: &mut Output,
    ) {
        output.ribbons.clear();
        output.lines.clear();
        self.project_into(width, height, first, &mut output.first);
        self.project_into(width, height, last, &mut output.last);
        let screen = ScreenBounds::new(Vec2::new(width as f32, height as f32));
        let fade = MotionFade {
            opacity: MotionOpacity::default(),
            power: MotionFadePower::default(),
        };
        let ink = SweepInk {
            rgb: RGB,
            alpha,
            width: line_width(width, height),
        };
        for &edge in &self.edges {
            let first = line(&output.first, edge);
            let last = line(&output.last, edge);
            if first.visible && last.visible {
                push_sweep(first, last, ink, fade, screen, &mut output.ribbons);
            }
        }
    }

    /// Dense normalized tent-shutter samples for visual reference. Use a float
    /// color attachment: low per-sample alpha vanishes in an 8-bit framebuffer.
    /// Memory is O(samples * edges); large models should use a bounded count.
    pub fn reference(
        &self,
        width: u32,
        height: u32,
        case: MotionCase,
        frame: f32,
        alpha: f32,
        samples: usize,
    ) -> Output {
        self.reference_from_pose_fn(width, height, alpha, samples, |fraction| {
            case.pose_at(frame - 2.0 + 2.0 * fraction, height)
        })
    }

    /// The callback takes normalized shutter time 0..1. This lets the live
    /// preview use precisely the same bounded motion sampler as GPU history.
    pub fn reference_from_pose_fn(
        &self,
        width: u32,
        height: u32,
        alpha: f32,
        samples: usize,
        pose_at: impl Fn(f32) -> Pose,
    ) -> Output {
        let samples = samples.max(1);
        let mut output = Output::default();
        let mut projected = Vec::with_capacity(self.vertices.len());
        let weights: Vec<_> = (0..samples)
            .map(|sample| {
                let time = (sample as f32 + 0.5) / samples as f32;
                1.0 - (2.0 * time - 1.0).abs()
            })
            .collect();
        let total: f32 = weights.iter().sum();
        let half = (line_width(width, height) * 0.5).max(0.5);
        let screen = ScreenBounds::new(Vec2::new(width as f32, height as f32));
        for (sample, weight) in weights.into_iter().enumerate() {
            let fraction = (sample as f32 + 0.5) / samples as f32;
            let pose = pose_at(fraction);
            self.project_into(width, height, pose, &mut projected);
            for &edge in &self.edges {
                let geometry = line(&projected, edge);
                if !geometry.visible {
                    continue;
                }
                let direction = geometry.end - geometry.start;
                let length = direction.length();
                let normal = if length > 1e-6 {
                    direction.perpendicular() * (half / length)
                } else {
                    Vec2::new(0.0, half)
                };
                let a = geometry.start - normal;
                let b = geometry.end - normal;
                let c = geometry.end + normal;
                let d = geometry.start + normal;
                let start = [
                    RGB[0],
                    RGB[1],
                    RGB[2],
                    alpha * weight / total * geometry.start_depth_opacity,
                ];
                let end = [
                    RGB[0],
                    RGB[1],
                    RGB[2],
                    alpha * weight / total * geometry.end_depth_opacity,
                ];
                for triangle in [
                    RibbonInstance::triangle([a, b, c], [start, end, end]).coverage(1 | 2),
                    RibbonInstance::triangle([a, c, d], [start, end, start]).coverage(2 | 4),
                ] {
                    if triangle.intersects(screen) {
                        output.ribbons.push(triangle);
                    }
                }
            }
        }
        output
    }
}

fn line(projected: &[[f32; 4]], [a, b]: [u32; 2]) -> LineGeometry {
    let a = projected[a as usize];
    let b = projected[b as usize];
    LineGeometry {
        start: Vec2::new(a[0], a[1]),
        end: Vec2::new(b[0], b[1]),
        start_depth_opacity: a[2],
        end_depth_opacity: b[2],
        visible: a[3] > 0.5 && b[3] > 0.5,
    }
}

#[derive(Default)]
pub(super) struct Output {
    pub lines: Vec<LineInstance>,
    pub ribbons: Vec<RibbonInstance>,
    first: Vec<[f32; 4]>,
    last: Vec<[f32; 4]>,
}

impl Output {
    pub fn frame(&self, composite: ModelComposite) -> Frame<'_> {
        Frame {
            lines: &[],
            model_lines: &self.lines,
            ribbons: &self.ribbons,
            model_composite: composite,
            model_edge_capacity: 0,
        }
    }
}

/// Basic display-image diagnostics; shape/energy agreement is easier to inspect
/// alongside the images than to collapse into one pass/fail score.
pub(super) fn image_metrics(a: &[u8], b: &[u8]) -> serde_json::Value {
    assert_eq!(a.len(), b.len());
    assert_eq!(a.len() % 4, 0);
    let pixels = a.len() / 4;
    let mut absolute = 0.0;
    let mut squared = 0.0;
    let mut maximum = 0_u8;
    let mut luminance = [0.0; 2];
    let mut nonzero = [0_usize; 2];
    let mut saturated = [0_usize; 2];
    for (a, b) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for channel in 0..3 {
            let difference = a[channel].abs_diff(b[channel]);
            absolute += f64::from(difference) / 255.0;
            squared += (f64::from(difference) / 255.0).powi(2);
            maximum = maximum.max(difference);
        }
        for (index, pixel) in [a, b].into_iter().enumerate() {
            luminance[index] += (0.2126 * f64::from(pixel[0])
                + 0.7152 * f64::from(pixel[1])
                + 0.0722 * f64::from(pixel[2]))
                / 255.0;
            nonzero[index] += usize::from(pixel[..3].iter().any(|&value| value != 0));
            saturated[index] += usize::from(pixel[..3].contains(&255));
        }
    }
    let denominator = (pixels * 3).max(1) as f64;
    serde_json::json!({
        "mean_absolute_rgb":absolute / denominator,
        "rmse_rgb":(squared / denominator).sqrt(),
        "max_absolute_rgb":f64::from(maximum) / 255.0,
        "mean_luminance_a":luminance[0] / pixels.max(1) as f64,
        "mean_luminance_b":luminance[1] / pixels.max(1) as f64,
        "nonzero_pixels_a":nonzero[0],"nonzero_pixels_b":nonzero[1],
        "saturated_pixels_a":saturated[0],"saturated_pixels_b":saturated[1],
    })
}
