//! Coarse surface geometry used only to mask presentation graphics.
//!
//! Work happens during model loading. Neither animation nor wireframe crease
//! changes revisit the source triangles.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::SystemTime,
};

use super::{triangle_normal, Vec3};

/// A compact, indexed surface in the same normalized space as the wireframe.
#[derive(Clone, Debug, Default)]
pub struct BodyMesh {
    pub vertices: Vec<Vec3>,
    pub triangles: Vec<[u16; 3]>,
    pub vertex_parts: Vec<u16>,
    pub source_triangle_count: usize,
}

// The smooth stage limits surface error before a coarser distance merge
// removes nearby detail that the solid presentation backdrop does not need.
const CREASE_DEGREES: f32 = 35.0;
const SURFACE_ERROR: f64 = 0.006;
const MERGE_DISTANCE: f32 = 0.05;
const COLLAPSE_PASSES: usize = 4;

type CacheKey = (PathBuf, u64, Option<SystemTime>);
type CacheEntry = Arc<OnceLock<Arc<BodyMesh>>>;

impl BodyMesh {
    /// Share immutable preparation across model sets and crease refreshes. A
    /// changed file gets a fresh entry; concurrent loads prepare it only once.
    pub(super) fn prepare_cached(
        path: &Path,
        vertices: &[Vec3],
        triangles: &[[u16; 3]],
        vertex_parts: &[u16],
    ) -> Arc<Self> {
        static CACHE: OnceLock<Mutex<BTreeMap<CacheKey, CacheEntry>>> = OnceLock::new();
        let Ok(metadata) = std::fs::metadata(path) else {
            return Arc::new(Self::prepare(vertices, triangles, vertex_parts));
        };
        let key = (path.to_path_buf(), metadata.len(), metadata.modified().ok());
        let entry = CACHE
            .get_or_init(Mutex::default)
            .lock()
            .expect("body preparation cache is available")
            .entry(key)
            .or_default()
            .clone();
        entry
            .get_or_init(|| Arc::new(Self::prepare(vertices, triangles, vertex_parts)))
            .clone()
    }

    pub(super) fn prepare(vertices: &[Vec3], triangles: &[[u16; 3]], vertex_parts: &[u16]) -> Self {
        let mut mesh = Simplifier::new(vertices, triangles, vertex_parts);
        mesh.collapse_smooth_edges();
        mesh.merge_nearest_vertices(MERGE_DISTANCE);
        mesh.finish(triangles.len())
    }
}

/// Symmetric plane quadric, retaining the original surface error after each
/// collapse instead of accumulating unconstrained drift on smooth surfaces.
#[derive(Clone, Copy, Default)]
struct Quadric {
    values: [f64; 10],
    planes: u32,
}

impl Quadric {
    fn plane(normal: Vec3, point: Vec3) -> Self {
        let v = [
            f64::from(normal.x),
            f64::from(normal.y),
            f64::from(normal.z),
            f64::from(-normal.dot(point)),
        ];
        let mut values = [0.0; 10];
        let mut index = 0;
        for row in 0..4 {
            for col in row..4 {
                values[index] = v[row] * v[col];
                index += 1;
            }
        }
        Self { values, planes: 1 }
    }

    fn plus(self, other: Self) -> Self {
        Self {
            values: std::array::from_fn(|index| self.values[index] + other.values[index]),
            planes: self.planes + other.planes,
        }
    }

    fn error(self, point: Vec3) -> f64 {
        let v = [
            f64::from(point.x),
            f64::from(point.y),
            f64::from(point.z),
            1.0,
        ];
        let mut error = 0.0;
        let mut index = 0;
        for row in 0..4 {
            for col in row..4 {
                let symmetry = if row == col { 1.0 } else { 2.0 };
                error += symmetry * self.values[index] * v[row] * v[col];
                index += 1;
            }
        }
        error.max(0.0) / f64::from(self.planes.max(1))
    }
}

struct Simplifier<'a> {
    vertices: &'a [Vec3],
    vertex_parts: &'a [u16],
    triangles: Vec<Option<[u16; 3]>>,
    incident: Vec<BTreeSet<usize>>,
    roots: Vec<u16>,
    quadrics: Vec<Quadric>,
    /// Boundaries, non-manifold edges, and creases survive the smooth stage.
    protected: Vec<bool>,
    part_triangles: BTreeMap<u16, usize>,
}

impl<'a> Simplifier<'a> {
    fn new(vertices: &'a [Vec3], triangles: &[[u16; 3]], vertex_parts: &'a [u16]) -> Self {
        let mut result = Self {
            vertices,
            vertex_parts,
            triangles: vec![None; triangles.len()],
            incident: vec![BTreeSet::new(); vertices.len()],
            roots: (0..vertices.len()).map(|index| index as u16).collect(),
            quadrics: vec![Quadric::default(); vertices.len()],
            protected: vec![false; vertices.len()],
            part_triangles: BTreeMap::new(),
        };
        let mut edges: BTreeMap<[u16; 2], Vec<Vec3>> = BTreeMap::new();
        for (index, &triangle) in triangles.iter().enumerate() {
            let [a, b, c] = triangle.map(|vertex| vertices[usize::from(vertex)]);
            let Some(normal) = triangle_normal(a, b, c) else {
                continue;
            };
            let part = vertex_parts[usize::from(triangle[0])];
            if triangle
                .iter()
                .any(|&vertex| vertex_parts[usize::from(vertex)] != part)
            {
                continue;
            }
            result.triangles[index] = Some(triangle);
            *result.part_triangles.entry(part).or_default() += 1;
            let quadric = Quadric::plane(normal, a);
            for vertex in triangle {
                result.incident[usize::from(vertex)].insert(index);
                result.quadrics[usize::from(vertex)] =
                    result.quadrics[usize::from(vertex)].plus(quadric);
            }
            for edge in triangle_edges(triangle) {
                edges.entry(edge).or_default().push(normal);
            }
        }
        let crease = CREASE_DEGREES.to_radians().cos();
        for ([a, b], normals) in edges {
            if !matches!(normals.as_slice(), [first, second] if first.dot(*second) >= crease) {
                result.protected[usize::from(a)] = true;
                result.protected[usize::from(b)] = true;
            }
        }
        result
    }

    fn root(&mut self, vertex: u16) -> u16 {
        let mut root = vertex;
        while self.roots[usize::from(root)] != root {
            root = self.roots[usize::from(root)];
        }
        let mut current = vertex;
        while current != root {
            let next = self.roots[usize::from(current)];
            self.roots[usize::from(current)] = root;
            current = next;
        }
        root
    }

    fn edges(&self) -> BTreeSet<[u16; 2]> {
        self.triangles
            .iter()
            .flatten()
            .flat_map(|&triangle| triangle_edges(triangle))
            .collect()
    }

    fn endpoints(&mut self, a: u16, b: u16, preserve_creases: bool) -> Option<(u16, u16, f64)> {
        let a = self.root(a);
        let b = self.root(b);
        if a == b || self.vertex_parts[usize::from(a)] != self.vertex_parts[usize::from(b)] {
            return None;
        }
        let qa = self.quadrics[usize::from(a)].plus(self.quadrics[usize::from(b)]);
        let a_error = qa.error(self.vertices[usize::from(a)]);
        let b_error = qa.error(self.vertices[usize::from(b)]);
        match (
            self.protected[usize::from(a)],
            self.protected[usize::from(b)],
        ) {
            (true, true) if preserve_creases => None,
            (true, true) if a_error <= b_error => Some((a, b, a_error)),
            (true, true) => Some((b, a, b_error)),
            (true, false) => Some((a, b, a_error)),
            (false, true) => Some((b, a, b_error)),
            (false, false) if a_error <= b_error => Some((a, b, a_error)),
            (false, false) => Some((b, a, b_error)),
        }
    }

    fn collapse_smooth_edges(&mut self) {
        for _ in 0..COLLAPSE_PASSES {
            let mut candidates: Vec<_> = self
                .edges()
                .into_iter()
                .filter_map(|[a, b]| {
                    self.endpoints(a, b, true)
                        .map(|(_, _, error)| (error, a, b))
                })
                .collect();
            candidates.sort_unstable_by(|a, b| {
                a.0.total_cmp(&b.0)
                    .then_with(|| a.1.cmp(&b.1))
                    .then_with(|| a.2.cmp(&b.2))
            });
            let mut collapsed = 0;
            for (_, a, b) in candidates {
                let Some((keep, remove, error)) = self.endpoints(a, b, true) else {
                    continue;
                };
                if error <= SURFACE_ERROR * SURFACE_ERROR && self.collapse(keep, remove) {
                    collapsed += 1;
                }
            }
            if collapsed == 0 {
                break;
            }
        }
    }

    /// Merge closest neighboring vertices first. Only connected pairs can
    /// satisfy the link condition, so enumerating mesh edges avoids quadratic
    /// spatial candidate lists on dense surfaces and keeps nearby shells apart.
    fn merge_nearest_vertices(&mut self, distance: f32) {
        let mut candidates: Vec<_> = self
            .edges()
            .into_iter()
            .filter_map(|[a, b]| {
                let delta = self.vertices[usize::from(a)] - self.vertices[usize::from(b)];
                let squared = delta.dot(delta);
                (squared <= distance * distance).then_some((squared, a, b))
            })
            .collect();
        candidates.sort_unstable_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        for (_, a, b) in candidates {
            let Some((keep, remove, _)) = self.endpoints(a, b, false) else {
                continue;
            };
            let delta = self.vertices[usize::from(keep)] - self.vertices[usize::from(remove)];
            if delta.dot(delta) <= distance * distance {
                self.collapse(keep, remove);
            }
        }
    }

    fn neighbors(&self, vertex: u16) -> BTreeSet<u16> {
        self.incident[usize::from(vertex)]
            .iter()
            .filter_map(|&index| self.triangles[index])
            .flatten()
            .filter(|&other| other != vertex)
            .collect()
    }

    /// The link condition prevents pinching or opening a surface. The normal
    /// check prevents a valid topological collapse from folding a face over.
    fn collapse(&mut self, keep: u16, remove: u16) -> bool {
        let keep_incident = &self.incident[usize::from(keep)];
        let remove_incident = &self.incident[usize::from(remove)];
        let shared: Vec<_> = keep_incident
            .intersection(remove_incident)
            .copied()
            .collect();
        if shared.is_empty() || shared.len() > 2 {
            return false;
        }
        let part = self.vertex_parts[usize::from(keep)];
        if self.part_triangles[&part] <= shared.len() {
            return false;
        }
        let expected: BTreeSet<_> = shared
            .iter()
            .filter_map(|&index| self.triangles[index])
            .flatten()
            .filter(|&vertex| vertex != keep && vertex != remove)
            .collect();
        let common: BTreeSet<_> = self
            .neighbors(keep)
            .intersection(&self.neighbors(remove))
            .copied()
            .collect();
        if common != expected {
            return false;
        }
        for &index in remove_incident {
            let triangle = self.triangles[index].expect("incident triangles exist");
            if triangle.contains(&keep) {
                continue;
            }
            let replacement = triangle.map(|vertex| if vertex == remove { keep } else { vertex });
            let normal = |triangle: [u16; 3]| {
                let [a, b, c] = triangle.map(|vertex| self.vertices[usize::from(vertex)]);
                triangle_normal(a, b, c)
            };
            let (Some(before), Some(after)) = (normal(triangle), normal(replacement)) else {
                return false;
            };
            if before.dot(after) < CREASE_DEGREES.to_radians().cos() {
                return false;
            }
        }
        let affected: Vec<_> = remove_incident.iter().copied().collect();
        for index in affected {
            let old = self.triangles[index]
                .take()
                .expect("incident triangles exist");
            for vertex in old {
                self.incident[usize::from(vertex)].remove(&index);
            }
            if old.contains(&keep) {
                *self
                    .part_triangles
                    .get_mut(&part)
                    .expect("the part has triangles") -= 1;
            } else {
                let new = old.map(|vertex| if vertex == remove { keep } else { vertex });
                for vertex in new {
                    self.incident[usize::from(vertex)].insert(index);
                }
                self.triangles[index] = Some(new);
            }
        }
        self.roots[usize::from(remove)] = keep;
        self.quadrics[usize::from(keep)] =
            self.quadrics[usize::from(keep)].plus(self.quadrics[usize::from(remove)]);
        true
    }

    fn finish(self, source_triangle_count: usize) -> BodyMesh {
        let mut result = BodyMesh {
            source_triangle_count,
            ..BodyMesh::default()
        };
        let mut remap = vec![None; self.vertices.len()];
        let mut seen = BTreeSet::new();
        for triangle in self.triangles.into_iter().flatten() {
            let mut key = triangle;
            key.sort_unstable();
            if !seen.insert(key) {
                continue;
            }
            result.triangles.push(triangle.map(|vertex| {
                *remap[usize::from(vertex)].get_or_insert_with(|| {
                    let index =
                        u16::try_from(result.vertices.len()).expect("source vertices fit u16");
                    result.vertices.push(self.vertices[usize::from(vertex)]);
                    result
                        .vertex_parts
                        .push(self.vertex_parts[usize::from(vertex)]);
                    index
                })
            }));
        }
        result
    }
}

fn triangle_edges([a, b, c]: [u16; 3]) -> [[u16; 2]; 3] {
    [
        [a.min(b), a.max(b)],
        [b.min(c), b.max(c)],
        [c.min(a), c.max(a)],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(steps: u16, height: impl Fn(f32, f32) -> f32) -> (Vec<Vec3>, Vec<[u16; 3]>) {
        let mut vertices = Vec::new();
        for y in 0..=steps {
            for x in 0..=steps {
                let (x, y) = (
                    f32::from(x) / f32::from(steps),
                    f32::from(y) / f32::from(steps),
                );
                vertices.push(Vec3::new(x, y, height(x, y)));
            }
        }
        let mut triangles = Vec::new();
        for y in 0..steps {
            for x in 0..steps {
                let a = y * (steps + 1) + x;
                let b = a + 1;
                let c = a + steps + 1;
                triangles.extend([[a, b, c], [b, c + 1, c]]);
            }
        }
        (vertices, triangles)
    }

    #[test]
    fn planar_tessellation_collapses_heavily_with_a_close_silhouette() {
        let (vertices, triangles) = grid(24, |_, _| 0.0);
        let body = BodyMesh::prepare(&vertices, &triangles, &vec![0; vertices.len()]);
        assert!(
            body.triangles.len() < triangles.len() / 6,
            "{} -> {}",
            triangles.len(),
            body.triangles.len()
        );
        for corner in [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
        ] {
            assert!(body
                .vertices
                .iter()
                .any(|&vertex| (vertex - corner).length() <= MERGE_DISTANCE));
        }
        let area: f32 = body
            .triangles
            .iter()
            .map(|&[a, b, c]| {
                (body.vertices[usize::from(b)] - body.vertices[usize::from(a)])
                    .cross(body.vertices[usize::from(c)] - body.vertices[usize::from(a)])
                    .length()
                    * 0.5
            })
            .sum();
        assert!((area - 1.0).abs() < 0.01, "area: {area}");
    }

    #[test]
    fn creases_and_distinct_parts_remain_in_the_backdrop() {
        let (vertices, triangles) = grid(16, |x, _| (x - 0.5).abs());
        let mut both_vertices = vertices.clone();
        both_vertices.extend(vertices.iter().copied());
        let offset = vertices.len() as u16;
        let mut both_triangles = triangles.clone();
        both_triangles.extend(
            triangles
                .iter()
                .map(|triangle| triangle.map(|vertex| vertex + offset)),
        );
        let mut parts = vec![0; vertices.len()];
        parts.extend(vec![1; vertices.len()]);
        let body = BodyMesh::prepare(&both_vertices, &both_triangles, &parts);
        for part in [0, 1] {
            for vertex in vertices.iter().filter(|v| v.x == 0.5) {
                assert!(body
                    .vertices
                    .iter()
                    .zip(&body.vertex_parts)
                    .any(|(v, &p)| v == vertex && p == part));
            }
        }
        assert!(body.triangles.iter().all(|triangle| triangle
            .iter()
            .all(|&vertex| body.vertex_parts[usize::from(vertex)]
                == body.vertex_parts[usize::from(triangle[0])])));
    }

    #[test]
    fn nearest_merge_reduces_close_curved_vertices_without_degenerate_faces() {
        let (vertices, triangles) = grid(20, |x, y| (x * 8.0).sin() * (y * 8.0).sin() * 0.03);
        let parts = vec![0; vertices.len()];
        let mut mesh = Simplifier::new(&vertices, &triangles, &parts);
        let before = mesh.triangles.iter().flatten().count();
        mesh.merge_nearest_vertices(0.08);
        let body = mesh.finish(triangles.len());
        assert!(body.triangles.len() < before / 2);
        assert!(body.triangles.iter().all(|&[a, b, c]| a != b
            && b != c
            && a != c
            && triangle_normal(
                body.vertices[a as usize],
                body.vertices[b as usize],
                body.vertices[c as usize]
            )
            .is_some()));
        assert_eq!(body.vertices.len(), body.vertex_parts.len());
    }

    #[test]
    fn tiny_parts_keep_a_surface() {
        let vertices = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.001, 0.0, 0.0),
            Vec3::new(0.0, 0.001, 0.0),
        ];
        let body = BodyMesh::prepare(&vertices, &[[0, 1, 2]], &[0, 0, 0]);
        assert_eq!(body.triangles, [[0, 1, 2]]);
    }
}
