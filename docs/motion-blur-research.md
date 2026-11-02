# Motion blur rendering alternatives

Research date: 2026-11-02. This note compares rendering approaches for Benga's
thin, overlapping, transparent-looking wireframes. It is a source-based design
assessment, not a measurement of any proposed replacement.

## Recommendation

Keep the analytical swept-edge approach as the main quality path. First measure
and reduce unnecessary primitives, fragment work, and framebuffer traffic. Move
projection or geometry generation to GPU shaders if CPU preparation or uploads
are a measured constraint. A general 3D engine does not automatically improve
this particular effect.

These recommendations are engineering inferences from the code and sources
below. No speedup is claimed without an implementation and comparison on the
target hardware.

## Measurements of the current implementation

Measured locally on 2026-11-02 with Apple M4 / Metal, a release build, and the
existing offscreen benchmarks. The heaviest default wireframe is
`mach_m249para`: 31,168 vertices and 69,225 edges at the current 0-degree crease
threshold. No renderer implementation was changed for this analysis.

The confirming frozen-geometry run used 128 timestamp samples per case at
3840 × 2160, single-frame batches, and 16 warmup frames per case:

| Case | Emitted triangles | Summed triangle area | CPU ribbon build median | GPU pass median / p95 |
| --- | ---: | ---: | ---: | ---: |
| Stationary, full blur | 138,450 | 0.965 M px² | 1.278 ms | 0.992 / 1.374 ms |
| Fast rotation, full blur | 402,536 | 19.947 M px² | 2.975 ms | 4.278 / 5.561 ms |
| Translation, full blur | 414,814 | 137.581 M px² | 3.137 ms | 22.270 / 28.745 ms |
| Translation, 10% alpha | 414,814 | 137.581 M px² | 3.083 ms | 21.808 / 26.883 ms |

The translation case displaces endpoints by approximately 537 pixels and is a
stress test, not typical playback. Its repeated full-blur measurement at the end
was 22.899 ms median. Empty-background GPU pass median was 0.168 ms.
Timings vary with system load and GPU scheduling; these are indicative local
measurements, not cross-device guarantees.

GPU timestamps include the retained-background composite and model draw, and
exclude geometry uploads, Vello UI rasterization, and display presentation.
Triangle area sums overlaps before viewport clipping; it is an overdraw proxy,
not a hardware fragment count. The frozen fixture projects two poses and uses a
synthetic tint/opacity, rather than replaying an application's complete animation.
The fast-rotation case rotates 30/18/8 degrees around its three axes.

The near-equal triangle counts for rotation and translation, but much greater
translation cost and area, support a substantial fragment/blending cost for wide
sweeps. Reducing alpha leaves primitive and covered-pixel work intact; exact-zero
geometry is already filtered. This does not isolate shader ALU, raster setup,
tiling, or framebuffer bandwidth from one another.

Each uploaded `RibbonInstance` occupies 80 bytes. The fast-rotation case therefore
uploads 32.20 MB of ribbons per frame, approximately 1.93 GB/s at 60 FPS. Its
separately measured CPU upload/encode/submit median was 3.111 ms. This is a reason
to reduce repeated triangle data as well as fragment coverage. CPU projection of
the two fixture poses was approximately 0.696 ms; that figure does not include
production history or UI work and is not a pure estimate of savings from GPU
projection. CPU/GPU stage medians should not be added to predict application FPS.

A separate production-pipeline run used normal controlled animation, default
configuration, a simulated 60 FPS timeline, 32 warmup frames and 32 measured
frames per component case. At 4K:

| Components submitted | Full completion median | Final compositor GPU pass median |
| --- | ---: | ---: |
| Full | 12.884 ms | 3.419 ms |
| Omit model | 6.529 ms | 1.122 ms |
| Omit foreground | 11.061 ms | 2.918 ms |

This bounded replay shows model upload/draw is material alongside the rest of
the presentation. The omit-model case still builds CPU model geometry; it
removes submission only. The final compositor timestamp includes other layers,
so its `model_gpu_ms` field is not an isolated model measurement. Full completion
is synchronized offscreen work, not live frame pacing or sustained throughput.
The retained background was reused throughout measured frames. This short
normal replay does not cover a full showcase, POI flight, or every model.

Artifacts:

- [Confirming frozen run](../target/motion-analysis-confirm/results.json)
- [Frozen summary CSV](../target/motion-analysis-confirm/summary.csv)
- [Initial 1080p/4K run](../target/motion-analysis-current/results.json)
- [Full presentation/component controls](../target/motion-analysis-pipeline/results.json)
- [Source provenance](../target/motion-analysis-confirm/provenance.json)

These artifacts live under ignored `target/` and may be removed by build cleanup.

Reproduce the confirming run with:

```sh
BENGA_MOTION_BENCH_DIR=target/motion-analysis-confirm \
BENGA_MOTION_BENCH_SAMPLES=128 \
BENGA_MOTION_BENCH_EXTENTS=3840x2160 \
BENGA_MOTION_BENCH_MOTIONS=still,fast_rotation,translation \
cargo test --release --offline --lib measures_frozen_motion \
  -- --ignored --nocapture --test-threads=1
```

## Implementation findings and priorities

The current history stores up to 17 complete screen-space edge snapshots, bounded
to two seconds. Only the shutter's first and last endpoints enter the rendered
sweep; its middle body is their screen-space interpolation. Longer shutters
increase swept area rather than multiply draws by the number of history samples.
This is a stylized sweep with an opacity falloff, not an exact time integral.
Curved rotation and reversals can be poorly represented, including motion whose
start and end nearly coincide. See
[`exposure_poses`](../src/visualizer.rs#L4796) and
[`push_sweep`](../src/visualizer.rs#L4914).

Each edge emits two body triangles at rest, six triangles with ordinary wings,
or eight in a crossing sweep, before visibility rejection. The model is already
batched into one ribbon draw. It does not draw a separate crisp-line underlay
while swept rendering is active. History/buffer allocation is recycled, fully
transparent geometry is skipped, and offscreen triangles are rejected. These
existing optimizations should be preserved.

The next useful experiments are:

1. Split broad wings from the median body and rasterize those soft wings into a
   reduced-resolution target, retaining the body and small sweeps at full
   resolution. Half width/height quarters the target pixels, with extra target,
   composition, triangle and sampling costs. Preserve additive sums and
   multiplicative transmittance rather than applying ordinary alpha composition
   indiscriminately. Check body/wing seams, thin details and both theme styles.
2. Replace per-triangle CPU uploads with compact edge records and shared tint
   data, then evaluate GPU expansion/projection with static vertex/index buffers.
   Repeating expensive expansion separately for every output vertex can offset
   savings. Compute projection of shared vertices is an option; generating the
   same large triangle buffer on GPU adds memory traffic and does not remove
   fragment work. Keep only necessary anchor/layout projections on CPU and avoid
   synchronous readback.
3. Reduce per-fragment support-distance and crossing-weight arithmetic by
   preparing reusable coefficients per primitive. Validate finite-segment caps,
   joins, coverage and subpixel strokes against the existing images before
   claiming equivalent output.
4. Continue model cleanup and introduce stable semantic or projected-detail
   levels if needed. Optimize visual contribution and swept area together;
   keeping only the longest edges can retain much of the expensive coverage.
   An arbitrary faint-edge cutoff can remove visible accumulated ink.
5. For quality, compare against multiple samples within the shutter interval.
   Adaptively divide only visibly curved trajectories, preserving opacity across
   subdivisions. This adds work and should be a quality choice, not presented as
   a general performance optimization. Dense shutter sampling is useful for
   export/reference rendering even if unsuitable for continuous playback.

There is also a separate improvement in
[`lines.wgsl`](../src/visualizer/lines.wgsl#L30): it rasterizes each crisp line's
axis-aligned padded rectangle. An oriented narrow quad could avoid most of that
rectangle for long diagonal lines. The confirming stationary sharp case cost
2.174 ms versus the ribbon body's 0.992 ms with the same triangle count. This
supports investigating the footprint; it is not a measured speedup for an
oriented-quad implementation. This issue concerns Still mode and diagnostic
lines, not the steady swept model's tightly rasterized triangles.

## What the current renderer already does

[`renderer.rs`](../src/visualizer/renderer.rs) uploads line/ribbon data and draws
custom GPU pipelines. [`ribbons.wgsl`](../src/visualizer/ribbons.wgsl) shades swept
triangles with interpolated opacity, additional wing masks, and analytical
support-edge coverage. Model ink uses additive or multiplicative blending;
geometry pipelines have no depth attachment. The GPU already rasterizes the
wireframes and their blur. Moving the remaining preparation onto the GPU would
change where geometry is produced, rather than introduce GPU rasterization.

The deliberately visible overlap is central to the look. A depth-tested opaque
mesh would remove contributions currently visible through the model. An
ordinary triangle-mesh wireframe would also need to preserve the authored
feature-edge selection rather than expose every triangulation edge. These are
design constraints for any replacement.

## Options and tradeoffs

| Approach | Potential benefit | Fit and limitations |
| --- | --- | --- |
| Improve current analytical ribbons | Preserve existing exposure and overlap behavior while reducing work | Best first path; measure primitive/setup cost, covered fragments, and composition separately. |
| Project vertices or generate ribbons in vertex/compute shaders | Reduce CPU calculations and per-frame generated-geometry uploads | Useful if those costs dominate; moving the same work to GPU does not remove the resulting triangle count or overdraw. |
| Standard color/depth/velocity post-process | Replace per-edge swept geometry with image-space filtering | Multiple wire depths and motion directions share a pixel; conventional buffers lose this information. Thin lines can be missed or diluted by sampling. |
| Multiple shutter-time renders accumulated into one frame | A useful high-quality reference; directly samples curved motion and changing opacity | More renders per output frame; requires sufficient temporal sampling to avoid separated line copies. Better suited to export/reference rendering. |
| Reuse previous display frames or TAA | Amortize samples across frames | History validation, changing poses and subpixel visibility complicate the result. A display-frame trail does not by itself implement a bounded shutter exposure. |
| General analytical transparent-triangle renderer | More complete treatment of time-dependent transparency/visibility | Considerable data structures, sorting, storage and implementation scope; unnecessary for the present order-independent ink style. |
| Switch to a general 3D engine | Established cameras, mesh processing, profiling and effect integrations | Still requires a quality solution for thin transparent wires; migration alone provides no performance guarantee. |

## Why conventional game motion blur is a risky replacement

Epic documents that Unreal's usual motion blur stores one velocity per pixel and
can exhibit artifacts when fast objects cross. Its higher-quality movie path
instead renders multiple shutter-time samples. That distinction is especially
relevant when many visible wires cross while moving in different directions.
[Epic: Setting Up Motion Blur](https://dev.epicgames.com/documentation/en-us/unreal-engine/setting-up-motion-blur)

McGuire et al.'s filter reconstructs blur from a conventional color image plus
depth and velocity, with neighborhood velocity information guiding sampling.
It explicitly assumes linear motion and distinguishes this plausible
reconstruction from sampling the scene throughout exposure. A single endpoint
velocity is therefore an approximation for strong rotation.
[McGuire et al., 2012](https://casual-effects.com/research/McGuire2012Blur/index.html),
[paper, limitations](https://www.casual-effects.com/research/McGuire2012Blur/McGuire12Blur.pdf)

The later feature-aware filter addresses overlapping motion and fine-scale
detail, but still documents noise and artifacts for extreme rotational motion
or complex deformation at high exposure. Those are unusually relevant stress
cases for Benga. This supports testing a post-process experimentally, rather
than assuming an engine's default blur will preserve the current appearance.
[Guertin et al., technical report](https://casual-effects.com/research/Guertin2013MotionBlurReport/index.html),
[published paper, limitations](https://casual-effects.com/research/Guertin2014MotionBlur/Guertin2014MotionBlur-small.pdf)

The simple depth-reconstruction method in GPU Gems only recovers camera motion;
moving objects require their own velocity calculation. Benga's animated models
would need those motion outputs even with a stationary camera.
[GPU Gems 3, chapter 27](https://developer.nvidia.com/gpugems/gpugems3/part-iv-image-effects/chapter-27-motion-blur-post-processing-effect)

**Inference for Benga:** a single-layer color/velocity buffer has already combined
multiple wires into one pixel before blur. Filtering cannot generally recover
each wire's original color, depth and trajectory from that combined value.
Layered buffers or special wire processing could improve the approximation but
add complexity and bandwidth. A post-process is a plausible optional low-cost
mode, with explicit visual comparison, rather than the default quality path.

## Where GPU-side changes might help

**Inference:** retaining model-space positions/edge indices on the GPU and
uploading a small pose history can reduce generated-geometry traffic. Vertex
expansion can handle fixed output shapes; compute is more flexible for
projection, culling and variable geometry generation. Neither approach reduces
the number of fragments blended when the same swept ribbons are ultimately
drawn. CPU layout and point-of-interest anchors also require a plan; avoid a
synchronous per-frame GPU readback.

**Inference:** use projected length, motion, curvature and visibility to control
detail. Skip imperceptible sweeps, merge eligible collinear features, and spend
extra motion segments only where projected trajectories visibly curve. Preserve
brightness and fade thresholds continuously to avoid popping. Fewer source
edges help CPU work, uploads and triangle count, but covered area and overlap
also determine GPU cost.

Hardware can lose efficiency on many narrow or tiny triangles: Arm documents
fragment shading in 2-by-2 pixel quads and the overhead from partial coverage.
This is a reason to compare primitive cost with pixel cost, not proof that
Benga's renderer is bottlenecked in either stage on Apple or other GPUs.
[Arm: Tiling and effective triangulation](https://support.arm.com/documentation/101897/0304/Tessellation--geometry-shading--and-tiling/Tiling-and-effective-triangulation?lang=en)

**Inference:** a lower-resolution target for broad blur, composited with crisp
full-resolution wires, may reduce fragment and blending cost. Validate thin and
short sweeps, color/alpha normalization, multiplicative themes and final
compositing carefully. An extra offscreen target and composition can outweigh
the savings. Half width and half height provide one quarter as many target
pixels, not a guaranteed fourfold overall speedup.

## Temporal accumulation and more complete analytical rendering

TAA accumulates reprojected history and must validate that history. The NVIDIA
survey discusses unresolved reconstruction problems, including thin geometry
and history-induced blur. For Benga, rapidly changing wire visibility and
different motions at crossings are reasons to test ghosting and lost detail.
This is separate from deliberately integrating samples over one shutter window.
[Yang et al.: A Survey of Temporal Antialiasing Techniques](https://research.nvidia.com/labs/rtr/publication/yang2020survey/),
[author-hosted paper](https://behindthepixels.io/assets/files/TemporalAA.pdf)

Rønnow et al. demonstrate a GPU analytical method for transparent triangles,
using moving triangle prisms, per-pixel intervals and sorting. Their paper also
documents linear-motion approximation, bandwidth/storage costs and increased
cost with longer exposures. It establishes that richer GPU analytical rendering
is possible; it does not establish that adopting it is cheaper for Benga's
specialized line art.
[Fast Analytical Motion Blur with Transparency, 2021](https://www.sciencedirect.com/science/article/pii/S0097849321000066),
[author-hosted paper](https://mfratarcangeli.github.io/pdf/fast_analytical_motion_blur.pdf)

## Evaluation needed before choosing

Use the existing GPU timestamp benchmark as a starting point. Timestamp queries
measure GPU pass execution, whereas CPU submission timing does not. Basic
render-pass timestamp writes and timestamps inside a pass have different
feature requirements; check adapter support when splitting stages.
[wgpu timestamp query example](https://wgpu.rs/doc/wgpu_examples/timestamp_queries/index.html),
[wgpu features](https://wgpu.rs/doc/wgpu/struct.Features.html)

Compare identical frozen poses at multiple resolutions, edge counts, line
widths and exposures. Separate crisp wires, ribbons and full-screen/UI
composition. Include static, slow, fast rotation, crossing wires, near-plane
motion and model transitions. Compare both timing and images against a dense
shutter-time reference. Avoid drawing conclusions from FPS or aggregate GPU
utilization alone.
