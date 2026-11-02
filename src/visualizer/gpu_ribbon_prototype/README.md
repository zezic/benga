# Throwaway GPU ribbon prototype

The default production GPU renderer is separate from this frozen comparison
harness. See [the integration guide](../../../docs/gpu-ribbon-integration.md)
for normal launching, the R toggle, live FPS/frame-time/downsize graphs,
compatibility fallbacks and validation.

Question: can static GPU geometry and three projected frame samples replace
CPU-expanded ribbons at full resolution, while following the actual middle pose?

This experiment is on `codex/gpu-ribbon-prototype`, behind the optional
`gpu-ribbon-prototype` feature. Normal playback and saved configuration are not
changed. The prototype fixes the shutter at two equally spaced frame intervals.

## Run the native comparison

From the repository root:

```sh
cargo run --release --features gpu-ribbon-prototype --example gpu_ribbon_prototype
```

- `1`: current CPU endpoint ribbons.
- `2`: optimized GPU ribbons: shared edge preparation and classified draws (default).
- `3`: paused temporal reference. Sample count is limited by device memory;
  use sparse fixtures for a denser reference.
- `4`: GPU ribbons with inverse-bilinear per-pixel coverage.
- `5`: original GPU prototype before optimization, using the same poses.
- Left/right arrows: stationary, mild/fast rotation, translation, strong rotation,
  along-edge translation, and direction reversal.
- Space: pause/resume; comma/period: step backward/forward while paused.
- `B`: additive/dark or multiplicative/light.
- `+` / `-`: model opacity; `R`: reset; Escape: close.

The viewer uses a full-resolution float attachment for every mode before display,
so faint exposure samples survive blending. The title reports CPU preparation and
submission time, not GPU execution time. The timeline advances at a nominal 60 Hz.
Both comparison paths sample the same bounded animation.

Choose another model with `--model rif_ak47`, or use `--model single` and
`--model crossing` to inspect individual strokes and crossings. `--live-seconds 3`
automatically closes a smoke run.

## Reproduce the measured comparison

```sh
cargo run --release --features gpu-ribbon-prototype --example gpu_ribbon_prototype -- \
  --headless --paths original,prepared --samples 96 --warmup 64 \
  --extent 3840x2160 --merge-passes --float-target \
  --output target/gpu-ribbon-optimization/final
```

This saves timestamp samples, projection checks, dark/light PNGs, and sparse
128-sample float references. The methods share geometry, camera, poses,
opacity, resolution and target format. The current path uses the existing
`push_sweep`; the GPU paths upload static vertices/indices once and then a
144-byte parameter block per frame.

GPU timing encloses projection, retained background, ribbon drawing, and pass
transitions. CPU baseline projection/construction is measured separately from
upload/encode/submit. Each sample completes before Metal timestamp resolution.
The experiment measures single-frame latency, not live application FPS or queued
throughput. It does not include audio, UI, POI layout, or presentation.

`--reference-samples N` additionally captures a full-model sampled reference into
a float attachment. Large models need small counts to fit the existing renderer's
storage binding limit; sparse fixtures support dense references. `--animate N`
saves full-resolution comparison frame sequences for the reversal case.

## What is deliberately incomplete

- The first GPU path is a rigid-model experiment. Part IDs are uploaded, but POI,
  slide-part deformation, model exchanges and dissolve effects are not wired in.
- Exactly three projected history slots and two equal-duration shutter intervals
  are supported. Variable frame durations and longer shutters need a generalized
  history/time layout.
- GPU opacity remains an artistic approximation. A normalized tent time profile
  controls the exposure, with a speed gain to retain luminous trails. The sampled
  reference integrates a physical tent exposure, so its brightness can differ.
- Multiplicative blending preserves the current visual convention; multiplying
  interval contributions is not the same as averaging complete shutter images.
- Near-plane rejection drops an edge if an endpoint is behind the plane, following
  the current renderer. It does not clip moving edges against the plane.
- Both GPU paths need visual assessment at reversals, folded sweeps, and subpixel
  strokes before any production replacement.

## Implementation

`gpu.rs` owns static topology, a three-slot projected-vertex ring, compute
projection, and procedural draw pipelines. `ribbons.wgsl` contains the projection
and shading. `fixtures.rs` provides deterministic motion, the unchanged CPU
baseline, and sampled references. `live.rs` is the native comparison window.
The parent module runs offscreen measurements and readbacks.

`prepared.rs` / `prepared.wgsl` now prepare each edge once, emit only active
triangles, and use separate regular, masked-crossing, partial-body and parallel
draws. Stable GPU prefix/scatter compaction keeps edge/triangle order consistent
between frames; it adds no CPU geometry transfer. `background.rs` combines the
retained background and GPU model into one pass. `baseline.wgsl` freezes the first
prototype for same-process comparisons.

## Optimization results (2026-11-02)

The optimizations were evaluated in estimated payoff order: replace per-pixel
crossing solves with triangle lobes; prepare shared edge data and classify draws;
combine passes; then test tighter bounds and indexed coverage. Intermediate
variants were measured separately. Crossing geometry alone increased repeated
vertex setup and regressed several cases; the shared preparation and shader
specialization are needed together.

Final comparison: Apple M4 / Metal, 69,225-edge model, 3840 × 2160 **Rgba16Float**
(the viewer's format), 64 warmup frames and 96 single-frame timestamp samples per
method. Both methods run in the same process with a merged background/model pass.
The original control uses the frozen initial GPU shader, not the CPU renderer.

| Motion | Original GPU median / p95 | Optimized median / p95 |
| --- | ---: | ---: |
| Stationary | 1.54 / 1.67 ms | 1.46 / 1.59 ms |
| Fast rotation | 4.25 / 4.62 ms | 4.71 / 8.34 ms |
| Translation | 15.64 / 19.85 ms | 15.15 / 18.98 ms |
| Along-edge translation | 8.96 / 11.88 ms | 11.99 / 16.09 ms |
| Strong rotation | 29.16 / 33.79 ms | 7.87 / 11.64 ms |
| Reversal | 29.08 / 34.62 ms | 22.01 / 25.71 ms |

Strong rotation improved by about 3.7× in this run. Other runs showed the same
large reduction in this stress case, but ordinary-motion results remain mixed.
Along-edge translation regressed in the final paired run. GPU scheduling/power
variation is substantial, so these are local workload measurements, not a promise
of a universal speedup or sustained application FPS. Shared edge preparation,
including deterministic compaction, took approximately 0.27–0.78 ms.

The optimized crossing gradients are a geometric/stylized approximation of the
old inverse-coverage shading. They can change local brightness and shape at
crossings. Full resolution, the three actual poses and source edges are retained;
compare viewer keys `2` and `5` to judge the visual tradeoff.

The preparation path launches only 138,450 body triangles at rest, instead of
issuing 18 vertex invocations for every edge and degenerating the unused wings.
Regular fragments carry no temporal endpoint geometry or general coverage solve.
Only partial body edges read attachment data; pure parallel motion has a dedicated
cap-time shader. CPU frame updates remain 144 bytes.

Atomic append order initially caused tiny frame-to-frame brightness variation in
dense float blending. Deterministic compaction fixed it: all final repeated dense
captures were pixel-identical in both dark and light styles. Sparse direct versus
prepared comparisons matched exactly except one crossing case with a one-level
8-bit display difference from grouping. Zero-opacity, wholly offscreen, and
historical-sweep visibility checks passed, and projected history is kept intact.

Lower-priority experiments remain optional:

- `--tight-support` uses conservatively padded hulls. It helped some broad
  coverage cases but regressed others and changed a few edge pixels. It is
  **disabled by default** and is not used by the optimized prepared path.
- `--indexed-coverage` reuses four vertices for each full-coverage quad. All
  compared images were pixel-identical, but timing results were mixed. It is
  opt-in; the optimized triangle path already emits its active vertices only.
- `--paths gpu` retains the direct triangle-generation intermediate for ablation.
  `--paths original,prepared` compares the original and final paths.

Measurements and validation images are in ignored build output:

- `target/gpu-ribbon-optimization/baseline/`: fresh original-executable baseline.
- `01-crossing-geometry/`: geometric crossing replacement alone.
- `02-prepared/`: shared preparation and initial compaction.
- `03-merged/`: isolated pass-combination comparison.
- `04-specialized-float/`: specialized draws before deterministic ordering.
- `05-original-bounds/` and `05-tight-bounds/`: support-bound comparison.
- `06-final-float/`: final same-process comparison and stable-image checks.
- `07-indexed-coverage/`: indexed comparison with identical output pixels.

Normal builds remain feature-gated and passed `cargo check --offline`. The native
surface and GPU shader checks pass. The existing repository-wide formatting check
has an unrelated pre-existing difference in `percussion_integration_tests.rs`;
prototype Rust files are formatted separately.

## Initial prototype verdict, before optimization (2026-11-02)

The architecture works: static topology stays on the GPU, and a new projected
history slot needs only a 144-byte CPU parameter update. On the heaviest default
model (`mach_m249para`, 31,168 vertices / 69,225 edges), the ring occupies 1,496,064
bytes. GPU/CPU projection agreed to within 0.000245 pixels across all three slots
at 4K. The real middle pose captures motion that the endpoint-only reversal case
misses.

Apple M4 / Metal, 64 samples per method, full-resolution 3840 × 2160 Bgra8Unorm:

| Motion | Current GPU span | Fast GPU span | Coverage GPU span | Current CPU upload/encode | Fast CPU upload/encode |
| --- | ---: | ---: | ---: | ---: | ---: |
| Stationary | 1.00 ms | 1.54 ms | 3.07 ms | 0.96 ms | 0.04 ms |
| Fast rotation | 4.65 ms | 5.58 ms | 15.34 ms | 3.59 ms | 0.09 ms |
| Translation | 19.98 ms | 16.32 ms | 69.11 ms | 4.38 ms | 0.12 ms |
| Along-edge translation | 10.46 ms | 8.99 ms | 39.08 ms | 3.35 ms | 0.05 ms |
| Strong rotation | 5.74 ms | 27.09 ms | 33.29 ms | 3.09 ms | 0.07 ms |
| Reversal | 0.99 ms | 20.48 ms | 108.13 ms | 1.05 ms | 0.07 ms |

The current path additionally spends approximately 1.6–3.4 ms preparing the two
projected endpoint poses and CPU ribbons in this frozen harness. This is measured
separately and differs from production's one-new-pose history update. Stage
medians must not be added to claim application FPS. GPU timestamps span the
enclosing compute/background/ribbon passes; independent pass intervals may
overlap. Normal application UI/audio/presentation costs are absent.

The fast path avoids the current rotation case's 32.2 MB generated ribbon upload.
It improves GPU time for translations but increases vertex work at rest and uses
an expensive per-pixel fallback for folded/crossing sweeps. The reversal cases
also render much more motion than the current path; equal endpoint positions
make the current algorithm almost stationary. These are different quality/cost
results, not equivalent-image speedups.

**Decision:** retain GPU-resident topology and shared projected history as the
promising direction. Use vertex-colored geometry as the performance candidate.
The general inverse-coverage variant is too expensive on this workload. Before
production integration, replace or narrow the fast path's expensive crossing
fallback, assess brightness/coverage at stroke boundaries, and wire real animation
state and shutter timing. No production renderer replacement is made here.

The exact measurements and previews are in
`target/gpu-ribbon-prototype/results.json` (ignored build output). Native surface
smoke rendering passed, all sparse float captures were finite, and the unchanged
default library suite passed: 577 tests, 32 ignored, no failures.
