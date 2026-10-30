//! Stable detail reduction driven by projected motion. Length bands control how
//! much detail survives; a weighted order within each band favors useful edges.
//! Selection affects emitted geometry only, never the complete motion history.

use super::{geometry::Wireframe, smoothstep, LineGeometry};

const SHORT_EDGE: f32 = 0.01;
const LONG_EDGE: f32 = 0.04;
const DENSITY_START: f32 = 8_000.0;
const DENSITY_FULL: f32 = 24_000.0;
const MOTION_START: f32 = 1.0;
const MOTION_STOP: f32 = 0.7;
const MOTION_FULL: f32 = 3.0;
const PRESSURE_RELEASE_SECONDS: f32 = 0.25;
const FADE_OUT_SECONDS: f32 = 0.04;
const FADE_IN_SECONDS: f32 = 0.08;
const RANK_FEATHER: f32 = 0.04;
const MIN_RETAINED: [f32; 3] = [0.10, 0.20, 0.40];

struct Edge {
    /// Normalized weighted rank within the model-space length band; lower wins.
    priority: f32,
    band: usize,
    part: u16,
    alpha: f32,
    /// Release the selection cutoff itself, so hidden geometry returns in
    /// sequence instead of all returning at low alpha on the first still frame.
    pressure: f32,
    previous: LineGeometry,
    recorded: bool,
    moving: bool,
}

pub(super) struct Thinning {
    edges: Vec<Edge>,
    /// Small models retain all detail. Larger ones gradually gain the full
    /// response, without a sharp quality discontinuity at one edge count.
    density: f32,
}

impl Thinning {
    pub fn new(wireframe: &Wireframe, seed: u64) -> Self {
        let count = wireframe.edges().len();
        let mut bands: [Vec<(usize, f32)>; 3] = std::array::from_fn(|_| Vec::new());
        let mut edges = Vec::with_capacity(count);
        let part_count = (0..count)
            .map(|index| usize::from(wireframe.edge_part(index)) + 1)
            .max()
            .unwrap_or(0);
        let mut representatives: Vec<Option<(usize, f32)>> = vec![None; part_count];
        for (index, &[a, b]) in wireframe.edges().iter().enumerate() {
            let length = (wireframe.vertex(a as usize) - wireframe.vertex(b as usize)).length();
            let band = if length < SHORT_EDGE {
                0
            } else if length < LONG_EDGE {
                1
            } else {
                2
            };
            // Exponential-race weighted order. All edges remain eligible, while
            // stronger features tend to occupy the beginning of each band.
            // Ranking happens only when the wireframe changes.
            let hash = hash(seed ^ index as u64);
            let uniform = (((hash >> 11) as f64 + 0.5) / (1_u64 << 53) as f64) as f32;
            let weight = wireframe.edge_importance(index);
            let part = wireframe.edge_part(index);
            let representative = &mut representatives[usize::from(part)];
            let value = length * weight;
            if representative.is_none_or(|(_, previous)| value > previous) {
                *representative = Some((index, value));
            }
            bands[band].push((index, -uniform.ln() / weight));
            edges.push(Edge {
                priority: 0.0,
                band,
                part,
                alpha: 1.0,
                pressure: 0.0,
                previous: LineGeometry {
                    start: super::Vec2::default(),
                    end: super::Vec2::default(),
                    start_depth_opacity: 0.0,
                    end_depth_opacity: 0.0,
                    visible: false,
                },
                recorded: false,
                moving: false,
            });
        }
        for band in &mut bands {
            band.sort_unstable_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
            let denominator = band.len() as f32;
            for (rank, &(index, _)) in band.iter().enumerate() {
                edges[index].priority = (rank as f32 + 0.5) / denominator;
            }
        }
        // Reserve one useful stroke per mesh part, keeping even a tiny part
        // present. The rest of its edges still participate in weighted thinning.
        for (index, _) in representatives.into_iter().flatten() {
            edges[index].priority = 0.0;
        }
        Self {
            edges,
            density: smoothstep((count as f32 - DENSITY_START) / (DENSITY_FULL - DENSITY_START)),
        }
    }

    /// `frame_seconds` is the actual interval between projected poses, rather
    /// than the clamped animation dt or shutter exposure. Motion is measured in
    /// viewport extents per second and therefore does not change with FPS/DPI.
    /// `dt` controls finite alpha fades. No work is allocated or sorted here.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        geometry: &[LineGeometry],
        dt: f32,
        frame_seconds: f32,
        viewport_min: f32,
        strength: f32,
        focus_parts: &[Option<u16>],
    ) {
        assert_eq!(
            self.edges.len(),
            geometry.len(),
            "thinning follows the complete wireframe"
        );
        let strength = if strength.is_finite() {
            strength.clamp(0.0, 2.0)
        } else {
            0.0
        };
        let enabled = strength > 0.0 && self.density > 0.0;
        let floors = MIN_RETAINED.map(|retained| retained.powf(strength));
        let motion_scale = if frame_seconds.is_finite()
            && frame_seconds > 0.0
            && viewport_min.is_finite()
            && viewport_min > 0.0
        {
            1.0 / (frame_seconds * viewport_min)
        } else {
            0.0
        };
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        for (edge, &current) in self.edges.iter_mut().zip(geometry) {
            let speed = if enabled && edge.recorded && current.visible && edge.previous.visible {
                perpendicular_displacement(edge.previous, current) * motion_scale
            } else {
                0.0
            };
            edge.previous = current;
            edge.recorded = true;
            if !enabled {
                // A manual Off choice restores the complete model immediately.
                edge.alpha = 1.0;
                edge.pressure = 0.0;
                edge.moving = false;
                continue;
            }
            let speed = if speed.is_finite() {
                speed.max(0.0)
            } else {
                0.0
            };
            if speed >= MOTION_START {
                edge.moving = true;
            } else if speed <= MOTION_STOP {
                edge.moving = false;
            }
            // Ordinary motion retains full detail. Once recovery has finished,
            // avoid rank, focus, and alpha work until costly motion begins.
            if !edge.moving && edge.pressure == 0.0 && edge.alpha == 1.0 {
                continue;
            }
            let pressure = if edge.moving {
                smoothstep((speed - MOTION_STOP) / (MOTION_FULL - MOTION_STOP))
            } else {
                0.0
            };
            // Predict costly motion immediately, then move the rank cutoff back
            // gradually. Alpha-only recovery would submit every hidden edge on
            // the first quiet frame while its old shutter sweep is still wide.
            edge.pressure = if pressure >= edge.pressure {
                pressure
            } else {
                approach(edge.pressure, pressure, dt / PRESSURE_RELEASE_SECONDS)
            };
            let retained = 1.0 - self.density * edge.pressure * (1.0 - floors[edge.band]);
            // Important POI/selected parts get roughly twice as much survival
            // priority, while still participating in reduction at high pressure.
            let focused = focus_parts.contains(&Some(edge.part));
            let priority = edge.priority * if focused { 0.45 } else { 1.0 };
            let target = smoothstep((retained * (1.0 + RANK_FEATHER) - priority) / RANK_FEATHER);
            let step = dt
                / if target < edge.alpha {
                    FADE_OUT_SECONDS
                } else {
                    FADE_IN_SECONDS
                };
            edge.alpha = approach(edge.alpha, target, step);
        }
    }

    pub fn alpha(&self, index: usize) -> f32 {
        self.edges[index].alpha
    }

    /// Temporal resets must not interpret an unrelated pose as one huge move.
    pub fn reset_motion(&mut self) {
        for edge in &mut self.edges {
            edge.recorded = false;
            edge.moving = false;
            edge.alpha = 1.0;
            edge.pressure = 0.0;
        }
    }
}

fn approach(current: f32, target: f32, step: f32) -> f32 {
    if target > current {
        (current + step).min(target)
    } else {
        (current - step).max(target)
    }
}

/// Along-stroke movement does not expand blur wings. Use the median edge's
/// perpendicular displacement; degenerate projected edges use endpoint travel.
fn perpendicular_displacement(previous: LineGeometry, current: LineGeometry) -> f32 {
    let start = current.start - previous.start;
    let end = current.end - previous.end;
    if start == super::Vec2::default() && end == super::Vec2::default() {
        return 0.0;
    }
    let direction = (previous.end - previous.start) + (current.end - current.start);
    let squared = direction.dot(direction);
    if squared > 1e-12 {
        let cross =
            |movement: super::Vec2| (movement.x * direction.y - movement.y * direction.x).abs();
        cross(start).max(cross(end)) / squared.sqrt()
    } else {
        start.dot(start).max(end.dot(end)).sqrt()
    }
}

fn hash(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests;
