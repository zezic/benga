//! Production GPU preparation, rasterization and full-presentation benchmarks.
//! Each benchmark module documents its explicit replay and output controls.

mod fullscreen_capacity;
mod fullscreen_fixtures;
mod pipeline;
mod poi_workload;
mod zoom_coverage;
mod zoom_workload;

use serde_json::{json, Value};

fn summary(samples: &[f64]) -> Value {
    assert!(!samples.is_empty());
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let percentile = |fraction: f64| {
        sorted[((sorted.len() as f64 * fraction).ceil() as usize).saturating_sub(1)]
    };
    json!({
        "min": sorted[0], "median": percentile(0.5),
        "p95": percentile(0.95), "max": sorted[sorted.len() - 1],
    })
}

#[test]
fn benchmark_percentiles_use_nearest_rank() {
    let values: Vec<_> = (1..=100).rev().map(f64::from).collect();
    let stats = summary(&values);
    assert_eq!(stats["median"], 50.0);
    assert_eq!(stats["p95"], 95.0);
    assert_eq!(stats["max"], 100.0);
}
