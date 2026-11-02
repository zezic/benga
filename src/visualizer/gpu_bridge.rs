//! Production source geometry and compact GPU projection parameters.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use super::geometry::{
    Camera, DepthOpacity, FocusProjection, Pose, Projected, Vec2, Vec3, Wireframe,
};

/// Compact solid backdrop, shared independently of the wireframe's edge list.
pub struct GpuBodyTopology {
    /// Model-space xyz and membership of the translating slide subtree.
    pub vertices: Vec<[f32; 4]>,
    pub indices: Vec<u16>,
}

/// Immutable and shared for a model at one crease revision.
pub struct GpuTopology {
    pub body: Option<Arc<GpuBodyTopology>>,
    pub edges: Vec<[u32; 2]>,
    pub vertex_count: usize,
    /// Model-space xyz and membership of the translating slide subtree.
    pub vertices: Vec<[f32; 4]>,
    /// Cached source boxes for fixed and slide vertices.
    pub bounds: [Option<[[f32; 3]; 2]>; 2],
}

impl GpuTopology {
    #[cfg(test)]
    pub(super) fn new(wireframe: &Wireframe) -> Arc<Self> {
        Self::with_slide_vertices(wireframe, &[])
    }

    #[cfg(test)]
    pub(super) fn with_slide_vertices(wireframe: &Wireframe, slide: &[bool]) -> Arc<Self> {
        Self::with_body(wireframe, slide, None)
    }

    pub(super) fn with_body(
        wireframe: &Wireframe,
        slide: &[bool],
        body: Option<Arc<GpuBodyTopology>>,
    ) -> Arc<Self> {
        let vertices: Vec<_> = (0..wireframe.vertex_count())
            .map(|index| {
                let v = wireframe.vertex(index);
                [
                    v.x,
                    v.y,
                    v.z,
                    f32::from(slide.get(index).copied().unwrap_or(false)),
                ]
            })
            .collect();
        let mut bounds = [None; 2];
        for v in &vertices {
            let value =
                bounds[v[3] as usize].get_or_insert([[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]]);
            for axis in 0..3 {
                value[0][axis] = value[0][axis].min(v[axis]);
                value[1][axis] = value[1][axis].max(v[axis]);
            }
        }
        Arc::new(Self {
            body,
            edges: wireframe
                .edges()
                .iter()
                .map(|e| [u32::from(e[0]), u32::from(e[1])])
                .collect(),
            vertex_count: wireframe.vertex_count(),
            vertices,
            bounds,
        })
    }
}

/// Affine model-to-view transform plus the production perspective/depth rules.
/// Coordinates always use physical output pixels, independent of ribbon resolution.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuProjection {
    pub rows: [[f32; 4]; 3],
    /// Center including shake, focal length, near plane.
    pub camera: [f32; 4],
    /// Camera distance, closest/farthest opacity planes, depth fade.
    pub depth: [f32; 4],
    pub slide: [f32; 4],
}

impl GpuProjection {
    pub(super) fn new(
        camera: Camera,
        pose: Pose,
        focus: FocusProjection,
        depth: DepthOpacity,
        slide: Vec3,
    ) -> Self {
        let rotate = |v: Vec3| {
            if focus.blend == 0.0 {
                v
            } else {
                v.rotate_y(focus.rotation_yaw)
                    .rotate_x(focus.rotation_pitch)
            }
        };
        let origin = rotate(pose.orient(focus.target) * -focus.blend);
        let x = rotate(pose.orient(Vec3::new(1.0, 0.0, 0.0)));
        let y = rotate(pose.orient(Vec3::new(0.0, 1.0, 0.0)));
        let z = rotate(pose.orient(Vec3::new(0.0, 0.0, 1.0)));
        Self {
            rows: [
                [x.x, y.x, z.x, origin.x],
                [x.y, y.y, z.y, origin.y],
                [x.z, y.z, z.z, origin.z],
            ],
            camera: [
                camera.center.x + pose.shake.x,
                camera.center.y + pose.shake.y,
                if focus.blend == 0.0 {
                    camera.focal
                } else {
                    focus.focal
                },
                if focus.blend == 0.0 { 0.4 } else { focus.near },
            ],
            depth: [
                if focus.blend == 0.0 {
                    pose.distance
                } else {
                    focus.distance
                },
                depth.closest,
                depth.farthest,
                depth.fade,
            ],
            slide: [slide.x, slide.y, slide.z, 0.0],
        }
    }

    fn view(self, source: [f32; 4]) -> [f32; 3] {
        let world =
            std::array::from_fn::<_, 3, _>(|axis| source[axis] + self.slide[axis] * source[3]);
        self.rows
            .map(|row| row[0] * world[0] + row[1] * world[1] + row[2] * world[2] + row[3])
    }

    pub(super) fn projected(self, source: [f32; 4]) -> Projected {
        let view = self.view(source);
        let raw = view[2] + self.depth[0];
        let depth = raw.max(self.camera[3]);
        let perspective = self.camera[2] / depth;
        Projected {
            screen: Vec2::new(
                self.camera[0] + view[0] * perspective,
                self.camera[1] - view[1] * perspective,
            ),
            #[cfg(test)]
            depth,
            #[cfg(test)]
            visible: raw > self.camera[3],
        }
    }

    #[cfg(test)]
    pub(super) fn record(self, source: [f32; 4]) -> [f32; 4] {
        let vertex = self.projected(source);
        if !vertex.visible || !vertex.screen.x.is_finite() || !vertex.screen.y.is_finite() {
            return [0.0; 4];
        }
        let span = self.depth[2] - self.depth[1];
        let opacity = if span <= 1e-6 {
            1.0
        } else {
            1.0 - self.depth[3] * ((vertex.depth - self.depth[1]) / span).clamp(0.0, 1.0)
        };
        [vertex.screen.x, vertex.screen.y, opacity, 1.0]
    }

    /// Conservative projected boxes for the fixed and moving subtrees. The
    /// near-plane case uses interval bounds so clipped edges remain covered.
    pub(super) fn bounds(self, topology: &GpuTopology) -> Option<vello::kurbo::Rect> {
        let mut result: Option<vello::kurbo::Rect> = None;
        for membership in [0.0, 1.0] {
            let Some([min, max]) = topology.bounds[membership as usize] else {
                continue;
            };
            let mut view_min = [f32::INFINITY; 3];
            let mut view_max = [f32::NEG_INFINITY; 3];
            for bits in 0..8 {
                let v = std::array::from_fn::<_, 3, _>(|axis| {
                    if bits & (1 << axis) == 0 {
                        min[axis]
                    } else {
                        max[axis]
                    }
                });
                let view = self.view([v[0], v[1], v[2], membership]);
                for axis in 0..3 {
                    view_min[axis] = view_min[axis].min(view[axis]);
                    view_max[axis] = view_max[axis].max(view[axis]);
                }
            }
            if view_max[2] + self.depth[0] <= self.camera[3] {
                continue;
            }
            let near = (view_min[2] + self.depth[0]).max(self.camera[3]);
            let far = (view_max[2] + self.depth[0]).max(near);
            let mut xy_min = [f32::INFINITY; 2];
            let mut xy_max = [f32::NEG_INFINITY; 2];
            for x in [view_min[0], view_max[0]] {
                for y in [view_min[1], view_max[1]] {
                    for d in [near, far] {
                        let xy = [
                            self.camera[0] + x * self.camera[2] / d,
                            self.camera[1] - y * self.camera[2] / d,
                        ];
                        for axis in 0..2 {
                            xy_min[axis] = xy_min[axis].min(xy[axis]);
                            xy_max[axis] = xy_max[axis].max(xy[axis]);
                        }
                    }
                }
            }
            if xy_min.iter().chain(xy_max.iter()).all(|v| v.is_finite()) {
                let rect = vello::kurbo::Rect::new(
                    xy_min[0] as f64,
                    xy_min[1] as f64,
                    xy_max[0] as f64,
                    xy_max[1] as f64,
                );
                result = Some(result.map_or(rect, |old| old.union(rect)));
            }
        }
        result
    }
}

/// Source projection parameters and current production ink, uploaded once/frame.
pub struct GpuModelFrame {
    pub topology: Arc<GpuTopology>,
    /// Solid coverage follows model transitions independently of line intensity.
    pub body_opacity: f32,
    /// Screen xy, depth opacity, and near-plane visibility.
    #[cfg(test)]
    pub projected: Vec<[f32; 4]>,
    /// Production uses compute projection; frozen test fixtures may supply records.
    pub projection: Option<GpuProjection>,
    /// Per-source-edge RGB and opacity, before depth/motion fading.
    pub styles: Vec<[f32; 4]>,
    pub width: f32,
    pub motion_fade_power: f32,
    pub motion_opacity: f32,
    pub at: f64,
    pub reset: u64,
    /// Actual displayed-pose intervals, oldest then newest, in seconds.
    pub durations: [f32; 2],
}

impl GpuModelFrame {
    /// CPU diagnostics only; the production render path uploads the parameters.
    #[cfg(test)]
    pub(super) fn cpu_projected(&self) -> Vec<[f32; 4]> {
        self.projection.map_or_else(
            || self.projected.clone(),
            |projection| {
                self.topology
                    .vertices
                    .iter()
                    .map(|v| projection.record(*v))
                    .collect()
            },
        )
    }

    pub(super) fn new(topology: Arc<GpuTopology>) -> Self {
        Self {
            topology,
            body_opacity: 1.0,
            #[cfg(test)]
            projected: Vec::new(),
            projection: None,
            styles: Vec::new(),
            width: 1.0,
            motion_fade_power: 0.5,
            motion_opacity: 0.0,
            at: 0.0,
            reset: generation(),
            durations: [1.0 / 60.0; 2],
        }
    }
}

pub(super) fn generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
