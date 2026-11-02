//! Benchmark-only readback of active prepared raster support.
//! This is continuous geometric area, not a fragment or visible-pixel query.
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{ensure, Context};
use serde_json::{json, Value};
use vello::{util::DeviceHandle, wgpu};

use super::{FrameParams, GpuRibbonRenderer};

const TIMEOUT: Duration = Duration::from_secs(30);
const CLASSES: [&str; 4] = ["regular", "masked_crossing", "partial_body", "parallel"];
const SCOPE: &str = "Continuous clipped raster-support area of active prepared primitives; overlap is counted repeatedly. Triangles use captured GPU clip coordinates; parallel rectangles use the captured records, frame uniforms and shader's f32 support equations. This is not a fragment query, union area, opacity-weighted light or visible brightness. Diagnostic GPU readback and CPU analysis occur after completion and outside reported frame timings; never used by playback.";

#[derive(Clone, Copy, Default)]
struct Totals {
    primitives: [usize; 4],
    screens: [f64; 4],
}

impl Totals {
    fn add(&mut self, class: usize, screens: f64) {
        self.primitives[class] += 1;
        self.screens[class] += screens;
    }

    fn merge(&mut self, other: &Self) {
        for class in 0..4 {
            self.primitives[class] += other.primitives[class];
            self.screens[class] += other.screens[class];
        }
    }

    fn area(&self) -> f64 {
        self.screens.iter().sum()
    }

    fn classes(&self) -> Value {
        let mut result = serde_json::Map::new();
        for (class, name) in CLASSES.into_iter().enumerate() {
            result.insert(
                name.to_owned(),
                json!({
                    "active_primitives":self.primitives[class],
                    "screen_layers":self.screens[class],
                }),
            );
        }
        Value::Object(result)
    }
}

/// Read selected existing GPU buffers once, after the frame being diagnosed.
fn read_buffers(handle: &DeviceHandle, buffers: &[&wgpu::Buffer]) -> anyhow::Result<Vec<Vec<u8>>> {
    let bytes: u64 = buffers.iter().map(|buffer| buffer.size()).sum();
    let readback = handle.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Prepared ribbon geometric coverage readback"),
        size: bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = handle.device.create_command_encoder(&Default::default());
    let mut offset = 0;
    for buffer in buffers {
        ensure!(buffer.size() % 4 == 0, "coverage buffer alignment");
        encoder.copy_buffer_to_buffer(buffer, 0, &readback, offset, buffer.size());
        offset += buffer.size();
    }
    handle.queue.submit([encoder.finish()]);
    let (tx, rx) = mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    let result = (|| {
        handle.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(TIMEOUT),
        })?;
        rx.recv_timeout(TIMEOUT)
            .context("coverage readback callback timed out")??;
        let mapped = readback.slice(..).get_mapped_range()?;
        let mut offset = 0;
        let mut result = Vec::with_capacity(buffers.len());
        for buffer in buffers {
            let end = offset + buffer.size() as usize;
            result.push(mapped[offset..end].to_vec());
            offset = end;
        }
        Ok(result)
    })();
    readback.unmap();
    result
}

fn word(bytes: &[u8], index: usize) -> u32 {
    u32::from_ne_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
}

fn float(bytes: &[u8], index: usize) -> f32 {
    f32::from_bits(word(bytes, index))
}

/// Sutherland-Hodgman clipping of a convex triangle/rectangle. Fixed scratch
/// arrays avoid allocating a polygon for every one of a model's triangles.
fn clipped_area(points: &[[f64; 2]], bounds: [f64; 4]) -> f64 {
    let mut input = [[0.0; 2]; 12];
    let mut output = [[0.0; 2]; 12];
    input[..points.len()].copy_from_slice(points);
    let mut count = points.len();
    for (axis, limit, lower) in [
        (0, bounds[0], true),
        (0, bounds[2], false),
        (1, bounds[1], true),
        (1, bounds[3], false),
    ] {
        if count == 0 {
            return 0.0;
        }
        let inside = |point: [f64; 2]| {
            if lower {
                point[axis] >= limit
            } else {
                point[axis] <= limit
            }
        };
        let mut written = 0;
        let mut previous = input[count - 1];
        let mut previous_inside = inside(previous);
        for current in input[..count].iter().copied() {
            let current_inside = inside(current);
            if previous_inside != current_inside {
                let fraction = (limit - previous[axis]) / (current[axis] - previous[axis]);
                output[written] = [
                    previous[0] + (current[0] - previous[0]) * fraction,
                    previous[1] + (current[1] - previous[1]) * fraction,
                ];
                written += 1;
            }
            if current_inside {
                output[written] = current;
                written += 1;
            }
            previous = current;
            previous_inside = current_inside;
        }
        count = written;
        std::mem::swap(&mut input, &mut output);
    }
    if count < 3 {
        return 0.0;
    }
    (0..count)
        .map(|i| {
            let a = input[i];
            let b = input[(i + 1) % count];
            a[0] * b[1] - a[1] * b[0]
        })
        .sum::<f64>()
        .abs()
        * 0.5
}

fn triangle_screens(record: &[u8]) -> anyhow::Result<f64> {
    let points = [
        [f64::from(float(record, 0)), f64::from(float(record, 1))],
        [f64::from(float(record, 2)), f64::from(float(record, 3))],
        [f64::from(float(record, 4)), f64::from(float(record, 5))],
    ];
    ensure!(
        points.iter().flatten().all(|v| v.is_finite()),
        "nonfinite active GPU triangle"
    );
    // Clip space has area 4; the transform to pixels preserves this ratio.
    Ok(clipped_area(&points, [-1.0, -1.0, 1.0, 1.0]) / 4.0)
}

fn parallel_screens(record: &[u8], params: &FrameParams, tight: bool) -> anyhow::Result<f64> {
    let along = [float(record, 12), float(record, 13)];
    let across = [-along[1], along[0]];
    let points = [
        [float(record, 0), float(record, 1)],
        [float(record, 2), float(record, 3)],
        [float(record, 4), float(record, 5)],
        [float(record, 6), float(record, 7)],
    ];
    ensure!(
        points
            .iter()
            .flatten()
            .chain(along.iter())
            .all(|v| v.is_finite()),
        "nonfinite active GPU parallel interval"
    );
    let dot = |a: [f32; 2], b: [f32; 2]| a[0] * b[0] + a[1] * b[1];
    let mut low = [1e20_f32; 2];
    let mut high = [-1e20_f32; 2];
    for point in points {
        let local = [dot(point, along), dot(point, across)];
        for axis in 0..2 {
            low[axis] = low[axis].min(local[axis]);
            high[axis] = high[axis].max(local[axis]);
        }
    }
    // Legacy CPU fixtures used zero in the render-scale lanes. Live frames
    // always carry at least one physical pixel per model-target pixel.
    let pixel = params.viewport[2].max(params.viewport[3]).max(1.0);
    let padding = params.style[0].max(pixel) * 0.5 + pixel * 0.75;
    for axis in 0..2 {
        low[axis] -= padding;
        high[axis] += padding;
    }
    if tight {
        let coordinate_scale = low
            .into_iter()
            .chain(high)
            .map(f32::abs)
            .fold(0.0, f32::max);
        if coordinate_scale < 65536.0 {
            let center = dot(
                [
                    (points[0][0] + points[2][0]) * 0.5,
                    (points[0][1] + points[2][1]) * 0.5,
                ],
                across,
            );
            let support = params.style[0].max(0.05) * 0.5 + pixel * 0.5 + 0.125;
            low[1] = low[1].max(center - support);
            high[1] = high[1].min(center + support);
        }
    }
    let rectangle = [
        [low[0], low[1]],
        [high[0], low[1]],
        [high[0], high[1]],
        [low[0], high[1]],
    ]
    .map(|local| {
        [
            f64::from(along[0] * local[0] + across[0] * local[1]),
            f64::from(along[1] * local[0] + across[1] * local[1]),
        ]
    });
    let viewport = [f64::from(params.viewport[0]), f64::from(params.viewport[1])];
    ensure!(
        viewport.iter().all(|v| v.is_finite() && *v > 0.0),
        "invalid coverage viewport"
    );
    Ok(
        clipped_area(&rectangle, [0.0, 0.0, viewport[0], viewport[1]])
            / (viewport[0] * viewport[1]),
    )
}

pub(super) fn measure(
    renderer: &GpuRibbonRenderer,
    handle: &DeviceHandle,
) -> anyhow::Result<Value> {
    let start = Instant::now();
    let mut total = Totals::default();
    let mut models = Vec::new();
    let mut bytes_read = 0_u64;
    for (slot_index, slot) in renderer.slots.iter().enumerate() {
        let buffers = slot.ribbons.benchmark_buffers();
        let sources = [
            buffers.counters,
            buffers.triangles,
            buffers.parallel,
            buffers.indices,
            &slot.uniforms,
        ];
        bytes_read += sources.iter().map(|buffer| buffer.size()).sum::<u64>();
        let captured = read_buffers(handle, &sources)?;
        let [counters, triangles, parallel, indices, uniforms]: [Vec<u8>; 5] =
            captured.try_into().unwrap();
        let params: FrameParams = bytemuck::pod_read_unaligned(&uniforms);
        let edges = buffers.edge_count as usize;
        let counts = [
            word(&counters, 17) as usize,
            word(&counters, 21) as usize,
            word(&counters, 25) as usize,
            word(&counters, 5) as usize,
        ];
        ensure!(
            word(&counters, 8) as usize == edges,
            "prepared source-edge count mismatch"
        );
        ensure!(
            counts[..3].iter().all(|n| *n <= edges * 6) && counts[3] <= edges * 2,
            "prepared active primitive count exceeds storage"
        );
        ensure!(
            counts[..3].iter().sum::<usize>() == word(&counters, 1) as usize,
            "prepared triangle category counts do not sum"
        );
        let mut per_model = Totals::default();
        let mut per_edge = vec![Totals::default(); edges];
        for class in 0..4 {
            let category_offset = class * edges * 6;
            for instance in 0..counts[class] {
                let index = word(&indices, category_offset + instance) as usize;
                let (edge, screens) = if class < 3 {
                    ensure!(index < edges * 6, "active triangle index exceeds storage");
                    let record = &triangles[index * 64..index * 64 + 64];
                    (word(record, 9) as usize >> 6, triangle_screens(record)?)
                } else {
                    ensure!(index < edges * 2, "active parallel index exceeds storage");
                    let record = &parallel[index * 64..index * 64 + 64];
                    (
                        index / 2,
                        parallel_screens(record, &params, buffers.tight_parallel_support)?,
                    )
                };
                ensure!(
                    edge < edges,
                    "active primitive source-edge index exceeds topology"
                );
                per_model.add(class, screens);
                per_edge[edge].add(class, screens);
            }
        }
        total.merge(&per_model);
        let mut ranking: Vec<_> = per_edge
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.area() > 0.0)
            .collect();
        ranking.sort_unstable_by(|(a_id, a), (b_id, b)| {
            b.area().total_cmp(&a.area()).then(a_id.cmp(b_id))
        });
        let top_edges: Vec<_> = ranking
            .into_iter()
            .take(20)
            .map(|(edge, area)| {
                json!({
                    "source_edge":edge,"source_vertices":slot.topology.edges[edge],
                    "screen_layers":area.area(),"classes":area.classes(),
                })
            })
            .collect();
        models.push(json!({"slot":slot_index,"animation_generation":slot.reset,
            "viewport":[params.viewport[0],params.viewport[1]],"source_vertices":slot.topology.vertex_count,
            "source_edges":edges,"stroke_width_pixels":params.style[0],
            "parallel_support":if buffers.tight_parallel_support {"tight"} else {"original"},
            "active_triangles":counts[..3].iter().sum::<usize>(),"active_parallel_intervals":counts[3],
            "total_screen_layers":per_model.area(),"classes":per_model.classes(),"top_source_edges":top_edges}));
    }
    Ok(
        json!({"models":models,"total_screen_layers":total.area(),"classes":total.classes(),
        "gpu_buffer_bytes_read":bytes_read,"diagnostic_wall_ms":start.elapsed().as_secs_f64()*1000.0,
        "scope":SCOPE}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convex_clipping_measures_viewport_intersection_and_counts_overlap_separately() {
        let viewport = [-1.0, -1.0, 1.0, 1.0];
        let half = [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0]];
        assert_eq!(clipped_area(&half, viewport), 2.0);
        assert_eq!(clipped_area(&[half[2], half[1], half[0]], viewport), 2.0);
        assert_eq!(
            clipped_area(
                &[[-2.0, -2.0], [2.0, -2.0], [2.0, 2.0], [-2.0, 2.0]],
                viewport
            ),
            4.0
        );
        assert_eq!(
            clipped_area(&[[2.0, 0.0], [3.0, 0.0], [2.0, 1.0]], viewport),
            0.0
        );
        assert_eq!(
            clipped_area(
                &[[-2.0, -1.0], [0.0, -1.0], [0.0, 1.0], [-2.0, 1.0]],
                viewport
            ),
            2.0
        );
        // Two overlapping full-screen primitives count as two layers, not one.
        assert_eq!(
            2.0 * clipped_area(
                &[[-2.0, -2.0], [2.0, -2.0], [2.0, 2.0], [-2.0, 2.0]],
                viewport
            ) / 4.0,
            2.0
        );
    }

    #[test]
    fn compact_triangle_decoder_uses_gpu_clip_coordinates_and_source_flag_lane() {
        let mut record = [0_u8; 64];
        for (index, value) in [-1.0_f32, -1.0, 1.0, -1.0, 1.0, 1.0]
            .into_iter()
            .enumerate()
        {
            record[index * 4..index * 4 + 4].copy_from_slice(&value.to_ne_bytes());
        }
        record[36..40].copy_from_slice(&(17_u32 << 6 | 2).to_ne_bytes());
        assert_eq!(word(&record, 9) >> 6, 17);
        assert_eq!(triangle_screens(&record).unwrap(), 0.5);
    }

    #[test]
    fn parallel_support_reconstructs_the_same_oriented_strip() {
        let mut record = [0_u8; 64];
        // Horizontal interval, first/last identical, long enough to cross view.
        for (index, value) in [
            (0, -16.0_f32),
            (1, 50.0),
            (2, 116.0),
            (3, 50.0),
            (4, -16.0),
            (5, 50.0),
            (6, 116.0),
            (7, 50.0),
            (12, 1.0),
            (13, 0.0),
        ] {
            record[index * 4..index * 4 + 4].copy_from_slice(&value.to_ne_bytes());
        }
        let mut params = FrameParams {
            rotation: [[0.0; 4]; 3],
            viewport: [100.0, 100.0, 0.0, 0.0],
            camera: [0.0; 4],
            pose: [0.0; 4],
            style: [2.0, 1.0, 0.5, 0.0],
            tint: [1.0; 4],
            history: [0; 4],
            temporal: [1.0, 1.0, 0.0, 0.0],
        };
        assert!((parallel_screens(&record, &params, false).unwrap() - 0.035).abs() < 1e-9);
        assert!((parallel_screens(&record, &params, true).unwrap() - 0.0325).abs() < 1e-9);

        // Explicit full resolution agrees with the legacy zero-scale fixture.
        params.viewport[2..].copy_from_slice(&[1.0, 1.0]);
        assert!((parallel_screens(&record, &params, false).unwrap() - 0.035).abs() < 1e-9);
        assert!((parallel_screens(&record, &params, true).unwrap() - 0.0325).abs() < 1e-9);

        // A target pixel spans four physical pixels across the strip. Its
        // raster support is ten physical pixels high, or 6.25 when tightened.
        // The horizontal ends are clipped by the 100-pixel-wide viewport.
        params.viewport[2..].copy_from_slice(&[2.0, 4.0]);
        assert!((parallel_screens(&record, &params, false).unwrap() - 0.1).abs() < 1e-9);
        assert!((parallel_screens(&record, &params, true).unwrap() - 0.0625).abs() < 1e-9);

        // At a 1x1 model target, the expanded support covers the whole view.
        params.viewport[2..].copy_from_slice(&[100.0, 100.0]);
        assert_eq!(parallel_screens(&record, &params, false).unwrap(), 1.0);
        assert_eq!(parallel_screens(&record, &params, true).unwrap(), 1.0);
    }
}
