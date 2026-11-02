//! Immutable model geometry and catalog bindings prepared before playback.

use std::{collections::BTreeMap, sync::Arc};

use super::{
    geometry::Wireframe,
    model::{Anchor, Model, SLIDE_NODE},
    showcase::FocusPoi,
    transition::{self, EdgeOrder},
    CreaseThreshold,
};

/// Shared by every runtime instance of a model at the selected crease angle.
pub(super) struct PreparedModel {
    pub name: Arc<str>,
    pub wireframe: Arc<Wireframe>,
    pub gpu_topology: Arc<super::gpu_bridge::GpuTopology>,
    pub edge_order: EdgeOrder,
    pub slide_vertices: Arc<[bool]>,
    pub pois: Arc<[FocusPoi]>,
    pub poi_vertices: BTreeMap<String, Arc<[u16]>>,
    pub magazine_icon: Option<Arc<[[f32; 4]]>>,
}

impl PreparedModel {
    pub fn new(
        model: &Model,
        crease: CreaseThreshold,
        catalog: Option<&crate::weapon::Catalog>,
        gpu_body: Option<Arc<super::gpu_bridge::GpuBodyTopology>>,
    ) -> Self {
        let wireframe = Arc::new(model.wireframe(crease));
        let edge_order =
            EdgeOrder::new(wireframe.edges().len(), transition::model_seed(&model.name));
        let slide_parts = model.node_parts(SLIDE_NODE);
        let slide_vertices: Arc<[bool]> = (0..wireframe.vertex_count())
            .map(|index| slide_parts.contains(&wireframe.vertex_part(index)))
            .collect::<Vec<_>>()
            .into();
        let gpu_body = gpu_body.unwrap_or_else(|| {
            let body = model.body();
            Arc::new(super::gpu_bridge::GpuBodyTopology {
                vertices: body
                    .vertices
                    .iter()
                    .zip(&body.vertex_parts)
                    .map(|(vertex, part)| {
                        [
                            vertex.x,
                            vertex.y,
                            vertex.z,
                            f32::from(slide_parts.contains(part)),
                        ]
                    })
                    .collect(),
                indices: body.triangles.iter().flatten().copied().collect(),
            })
        });
        let gpu_topology =
            super::gpu_bridge::GpuTopology::with_body(&wireframe, &slide_vertices, Some(gpu_body));
        let mut pois = Vec::new();
        let mut poi_vertices = BTreeMap::new();
        if let Some(weapon) = catalog.and_then(|catalog| catalog.weapons.get(&model.id)) {
            for poi in &weapon.poi {
                let Some(anchor) = model.anchor(&poi.anchor.name) else {
                    continue;
                };
                let parts = model.node_parts(&poi.anchor.name);
                let mut focus = FocusPoi::new(
                    poi.anchor.name.clone(),
                    anchor,
                    poi.id == "ejection_port" && anchor.part.is_none(),
                )
                .with_identity(poi.id.clone(), poi.label.clone());
                if anchor.part.is_some() {
                    let children = parts
                        .iter()
                        .copied()
                        .filter(|part| Some(*part) != anchor.part)
                        .collect();
                    focus = focus.with_children(children);
                }
                pois.push(focus);
                poi_vertices
                    .entry(poi.id.clone())
                    .or_insert_with(|| candidate_vertices(&wireframe, anchor, parts));
            }
        }
        let magazine_icon = magazine_icon(model, &wireframe);
        Self {
            name: Arc::from(model.name.as_str()),
            wireframe,
            gpu_topology,
            edge_order,
            slide_vertices,
            pois: pois.into(),
            poi_vertices,
            magazine_icon,
        }
    }
}

fn candidate_vertices(wireframe: &Wireframe, anchor: Anchor, parts: &[u16]) -> Arc<[u16]> {
    if anchor.part.is_some() {
        return (0..wireframe.vertex_count())
            .filter(|index| parts.contains(&wireframe.vertex_part(*index)))
            .map(|index| u16::try_from(index).expect("a model has at most u16::MAX vertices"))
            .collect::<Vec<_>>()
            .into();
    }
    let mut nearest = None;
    let mut nearest_distance = f32::INFINITY;
    for index in 0..wireframe.vertex_count() {
        let distance = (wireframe.vertex(index) - anchor.center).length();
        if distance < nearest_distance {
            nearest = Some(index);
            nearest_distance = distance;
        }
    }
    nearest
        .map(|index| u16::try_from(index).expect("a model has at most u16::MAX vertices"))
        .into_iter()
        .collect::<Vec<_>>()
        .into()
}

/// Normalized projection of the first magazine child, when at least three exist.
fn magazine_icon(model: &Model, wireframe: &Wireframe) -> Option<Arc<[[f32; 4]]>> {
    let parts = model.node_parts("magazine");
    let parent = model.anchor("magazine").and_then(|anchor| anchor.part);
    let children: Vec<u16> = parts
        .iter()
        .copied()
        .filter(|part| Some(*part) != parent)
        .collect();
    if children.len() < 3 {
        return None;
    }
    let part = children[0];
    let mut low = [f32::INFINITY; 3];
    let mut high = [f32::NEG_INFINITY; 3];
    for index in 0..wireframe.vertex_count() {
        if wireframe.vertex_part(index) != part {
            continue;
        }
        let vertex = wireframe.vertex(index);
        for (axis, value) in [vertex.x, vertex.y, vertex.z].into_iter().enumerate() {
            low[axis] = low[axis].min(value);
            high[axis] = high[axis].max(value);
        }
    }
    if !low[0].is_finite() {
        return None;
    }
    let spans = [high[0] - low[0], high[1] - low[1], high[2] - low[2]];
    let (u, v) = [(0, 1), (0, 2), (1, 2)]
        .into_iter()
        .max_by(|a, b| (spans[a.0] * spans[a.1]).total_cmp(&(spans[b.0] * spans[b.1])))
        .expect("axis pairs exist");
    let center_u = (low[u] + high[u]) * 0.5;
    let center_v = (low[v] + high[v]) * 0.5;
    let scale = 1.0 / spans[u].max(spans[v]).max(1e-5);
    let mut projected = vec![None; wireframe.vertex_count()];
    for index in 0..wireframe.vertex_count() {
        if wireframe.vertex_part(index) != part {
            continue;
        }
        let vertex = wireframe.vertex(index);
        let coordinates = [vertex.x, vertex.y, vertex.z];
        projected[index] = Some([
            (coordinates[u] - center_u) * scale,
            (coordinates[v] - center_v) * scale,
        ]);
    }
    let mut lines = Vec::new();
    for (index, edge) in wireframe.edges().iter().enumerate() {
        if wireframe.edge_part(index) != part {
            continue;
        }
        let (Some(a), Some(b)) = (
            projected[usize::from(edge[0])],
            projected[usize::from(edge[1])],
        ) else {
            continue;
        };
        lines.push([a[0], a[1], b[0], b[1]]);
    }
    (!lines.is_empty()).then(|| lines.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visualizer::geometry::Vec3;

    fn fixture() -> Wireframe {
        Wireframe::from_parts(
            vec![
                Vec3::new(-1.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 2.0, 0.0),
                Vec3::new(0.0, 3.0, 0.0),
            ],
            vec![[0, 1], [2, 3]],
            vec![0, 0, 1, 1],
            vec![0, 1],
            2,
        )
    }

    fn anchor(center: Vec3, part: Option<u16>) -> Anchor {
        Anchor {
            center,
            radius: 1.0,
            size: Vec3::new(1.0, 1.0, 1.0),
            part,
        }
    }

    #[test]
    fn mesh_candidates_include_children_and_keep_model_vertex_indices() {
        let wireframe = fixture();
        let root = anchor(Vec3::default(), Some(0));
        assert_eq!(
            &*candidate_vertices(&wireframe, root, &[0, 1]),
            &[0, 1, 2, 3]
        );
        assert_eq!(&*candidate_vertices(&wireframe, root, &[1]), &[2, 3]);
        assert!(candidate_vertices(&wireframe, root, &[]).is_empty());
    }

    #[test]
    fn marker_candidate_uses_nearest_vertex_and_first_index_for_ties() {
        let wireframe = fixture();
        assert_eq!(
            &*candidate_vertices(&wireframe, anchor(Vec3::default(), None), &[]),
            &[0]
        );
        assert_eq!(
            &*candidate_vertices(&wireframe, anchor(Vec3::new(0.0, 2.8, 0.0), None), &[]),
            &[3]
        );
    }
}
