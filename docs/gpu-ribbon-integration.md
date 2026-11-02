# GPU renderer in Benga

GPU projection and model rendering are the desktop default:

```sh
cargo run --release
```

**R** switches between GPU and CPU rendering while audio and animation continue.
Start a CPU comparison explicitly with `cargo run --release -- --cpu-ribbons`;
add `--profile` to either launch for the bounded synthetic workload. The window
title and console show the active backend, model-target size and any CPU fallback.
Normal desktop sessions save settings automatically. Profiling leaves settings
untouched.

Saved shutter and motion preferences remain in effect. Swept motion blur uses
GPU ribbons at shutter **2**, covering two frame intervals and three projected
poses. With motion blur disabled, GPU lines support the complete shutter range.
Other swept shutter settings retain the CPU renderer, as do unsupported GPU
capabilities or model-buffer limits; the fallback reason is visible. Factory
settings already use shutter 2 and swept motion, so a fresh launch selects GPU
ribbons without changing or migrating saved preferences.

`--experimental-gpu-ribbons` remains a compatibility trial alias. It starts with
shutter 2 and keeps that session's settings in memory, matching its earlier
behavior. It is no longer required to enable GPU rendering.

The GPU computes production camera, focus, zoom, shake and moving-slide
projection from cached model-space vertices and one small frame-parameter block.
Edge preparation reads the GPU's three-pose projection history and per-edge ink,
instead of uploading CPU-projected vertices or expanded ribbon triangles.
Animation, audio analysis, per-edge styling, annotation projection and
conservative layout support remain CPU-side. In supported GPU modes, source
projection, motion history, edge expansion and rasterization stay on the GPU;
no CPU-expanded model line/ribbon triangles are uploaded.

Adaptive rendering starts at **1× downsize**, using the full physical output
width and height, and can restore all the way back to 1× when timing allows.
There is no supersampling: the adaptive factor never falls below 1×.
Each fresh timing result updates the target for the **next submitted frame**.
GPU measurements arrive asynchronously and can trail submission by several
frames. Older-quality samples can inform scaling-benefit evidence, but cannot
drive a new control step; another viewport or rendering session is discarded.
There is no additional 120 ms collection window or one-second recovery delay.

Synchronized presentation uses one shared interval for redraw pacing and the
adaptive budget: the slower of the selected FPS limit and the current monitor's
reported refresh rate, with 60 Hz as a fallback. This includes the VSync settings
choice. Arbitrary monitor rates such as 144 Hz retain their exact interval;
they are not rounded down to a listed FPS choice. The normal desktop uses
AutoVsync. An explicit unsynchronized diagnostic run keeps its requested limit.
Using that budget:

- **Lower resolution** as estimated next-frame cost reaches **85% of budget**.
  At 60 FPS, this is **14.17 ms**, leaving margin before the 16.67 ms deadline.
- **Restore detail** on the first frame whose estimate is below **65% of budget**:
  **10.83 ms** at 60 FPS. A learned fixed-cost limit can also allow finer quality immediately
  when the absolute FPS target remains unreachable.
- Aim for **75% of budget** (**12.50 ms** at 60 FPS) when choosing the next
  factor. The estimate multiplies linear downsize by
  `sqrt(projected_cost / target_cost)`, limiting each frame's factor change to
  **0.5×–2×** and retaining the **1× minimum downsize**. Quality normally stays
  unchanged between the restore and drop thresholds.

These examples use controller-equivalent milliseconds. Native GPU feedback
translates the actual ribbon time through the allowance below; the host
frame-time plot is not the controllable model cost.

On devices with timestamp queries, quality uses **GPU ribbon-pass timing** with
a separate measurement of the frame's GPU queue span. Surface acquisition and
presentation waits are outside these timestamp boundaries. The total span can
include CPU submission gaps; it is not an isolated busy-GPU utilization counter.
The controller accounts for work outside the ribbon pass before choosing a
model allowance. Stage intervals can overlap, so derived fixed work is clamped
at zero and is a control estimate, rather than independent GPU busy-time accounting. With frame budget `B`, it estimates
`fixed = max(total_span - ribbon_span, 0)` and sets
`model_allowance = max(0.75 * B - fixed, 0.25 * fixed)`.
Ribbon cost is normalized against that allowance for the 85%/65% thresholds
above, capped by the observed whole-frame span so an otherwise safe 6.8 ms
frame cannot become 8.6 ms merely through normalization. The fixed-cost allowance
prevents a cheap model being reduced to one
pixel merely because full-resolution composition or delivery cannot meet a
requested high FPS. It still permits useful cuts when ribbon work dominates.

Scaling benefit is checked independently against actual ribbon-pass time. After
2× linear coarsening (four times fewer pixels), the pass must become at least
30% cheaper before further coarsening is credited as useful. Otherwise the
renderer restores the preceding useful quality and caps further cuts there.
This is a measured limit, not a fixed maximum factor: real pixel-cost gains can
advance the reference to any needed scale. A substantially heavier scene
reopens the check; finer quality and timing-session changes reset its evidence.

Four timestamp values (32 bytes per sampled frame) are read asynchronously
through six reusable slots. Busy slots skip telemetry rather than waiting.
Metal counter resolution is submitted only after an asynchronous completion
callback; the playback path never waits for a query or map. Geometry and image
data remain on the GPU.

If timestamp queries are unavailable, fallback active redraw cost excludes the
surface-acquisition wait: obtaining a
Metal drawable can block for presentation independently of model rendering.
The FPS and frame-time HUD still records the complete host intervals, including
that wait. Presentation cadence contributes pressure only when both the
**current interval** and its **adjacent-frame mean** exceed 112% of budget, and
uses the paired mean as that pressure's cost. This catches persistent delivery
backpressure without treating alternating early/late samples as resolution
improvements. A current healthy interval can restore quality immediately after
an older slow frame. These fallback host timings are not isolated GPU execution measurements.

The next-frame estimate also responds to rising scene pressure immediately. For
GPU feedback the rise is measured in actual ribbon time, so changing fixed work
or budget normalization cannot masquerade as growing model work. If raw cost
increases while the rendered target is unchanged or coarser, it adds that
one-frame increase again: `projected_cost = cost + max(cost - previous_cost, 0)`.
For example, a rise from 10 ms to 13 ms forecasts 16 ms and can lower resolution
before the current frame itself reaches the 14.17 ms margin. A cost increase
following an intentional finer target is not extrapolated as scene growth.
This uses the preceding observation without another sampling window or cooldown.
Safety rollback and fixed-cost learning continue to use observed cost, rather
than treating the forecast as an execution measurement.

Quality changes remain reversible. A finer trial that consumes the safety margin
and materially increases cost is rolled back; a subsequent trial can use a smaller
step between the safe and rejected factors. Repeated cuts with little observed
benefit restore the preceding quality and learn a fixed-cost limit, rather than
continuing to shrink the image. Three cuts are judged against their sequence's
starting model cost, and unchanged-quality frames retain that evidence. A lower
jitter sample cannot repeatedly restart an unhelpful sequence. That limit is reconsidered when cost changes or
remaining deadline headroom is consumed, and expires when rendering becomes
substantially cheaper. There is no timed hold before a useful quality change.
A resize, R switch, occlusion timing restart or frame-budget change resets quality
to 1×. There is no fixed maximum downsize factor. Sudden workload changes can
still miss a deadline, and CPU/fixed presentation costs can limit attainable FPS.

The model always renders into its own `Rgba16Float` layer. At 1× that layer has
the native output dimensions; it remains separate so compositing and GPU model
pass timing retain the same scope at every quality level. When downsized,
**nearest-neighbour** reconstruction preserves hard reduced-pixel boundaries;
at 1× the reconstruction is one source texel per output pixel. Background,
diagnostics, foreground and controls keep their physical pixel resolution.
Projected history stays in physical screen coordinates across quality changes;
a physical viewport resize reseeds it. The title and downsize graph report the
actual model-target dimensions.

Run the bounded synthetic audio workload through the real desktop loop with:

```sh
cargo run --release -- --profile
```

R also works during profiling. Close the standalone prototype viewer before
measuring. Compare the same viewport, animation moments, and settings; the
prototype's performance results do not establish full-application speedups.

For a manual trial, exercise live music, camera/focus changes, slide motion,
selectors, model exchanges, dark and light themes, resizing, and R switching.
Check the active-backend status when changing shutter or disabling motion blur.
The GPU crossing shading remains an approximation and can change local
brightness. The former standalone prototype and CPU comparison renderer have
since been retired; see the [current renderer notes](scene-transition-performance.md)
for the supported path and the scope of these earlier measurements.

The performance overlay keeps its backend status, **FPS**, **frame time** and
**downsize** graphs visible when controls fade. All three plots share eight seconds
of history across
**R** switches, with CPU/GPU colors and switch markers for comparison. FPS follows
successful application presentation calls, including frame limiting, VSync and
backpressure. Frame time shows the interval between those calls in milliseconds;
its second line shows host wall time through UI construction, surface acquisition,
rasterization and submission. That line includes acquisition waiting and is not a
GPU execution measurement. The readouts include recent FPS and frame-time p95.
Downsize is a stepped trace with a log2 axis: moving upward means a coarser model
target. Its readout shows the rendered linear factor and actual target dimensions,
with 1×/full resolution for CPU frames. A 2× factor halves each dimension; 4×
therefore uses one quarter as many model pixels as 2×. The quality sample is
captured alongside its successful presentation timing, before feedback chooses
the next frame's resolution, so quality changes align with the FPS/frame-time
history. Brief cuts survive per-column min/max reduction and the current quality
label refreshes immediately when its factor or dimensions change.
HUD previews with illustrative quality changes are in
`target/gpu-ribbon-integration/performance-hud/{dark,light,narrow,hidpi}.png`.
Minimizing, occlusion or unavailable surfaces break timing continuity instead of
turning an idle period into a slow-frame sample. Timing remains in memory and
requires no GPU readback. The same graphs are visible during `--profile`; their
rendering cost is included in the live profile. Deterministic offscreen comparisons
keep the experimental overlay disabled so both backends use the same UI workload.

Device limits, indirect draw/storage support and float blending
are checked before CPU frame construction; models exceeding the device's working
buffer capacity also fall back. Disabling model motion blur uses GPU lines.
Switching resets temporal geometry and the adaptive quality baseline while
playback and the audio timeline keep advancing.

Static edge topology and model-space positions are cached for each prepared
model/crease revision. Only the
active model and outgoing model own GPU preparation buffers and three projected
poses. Projection transfers a 96-byte parameter block and 16 bytes of ring
metadata per model. Edge styles transfer 16 bytes of real RGB/opacity per source
edge; ribbon frame uniforms are uploaded separately. Initial history seeding is
computed on the GPU. No per-frame CPU ribbon triangles are built in GPU mode.
The only native adaptive readback is the bounded asynchronous timestamp sample;
model vertices, prepared geometry and image pixels are not read back.

The model publishes a conservative union of three projected bounds, padded
for stroke coverage, for graph placement. CPU projection remains available
for POI labels and diagnosis. Full composition keeps background, model,
diagnostics, foreground and controls in their existing order, accumulates in
`Rgba16Float`, then converts once to the surface format.

Actual pose timestamps determine each interval's exposure share. The artistic
profile peaks at the recorded middle pose. Geometric wing and crossing shading
still approximate exposure; unequal-duration weighting does not turn it into a
physical temporal integrator. Longer/fractional shutters remain follow-up work.
GPU swept motion currently accepts shutter 2; GPU still lines accept any saved
shutter value. Unsupported swept shutter preferences remain unchanged and use
the visible CPU fallback.

Video export also requests the supported GPU model paths, at a fixed **1×**
model target. It preserves the requested output dimensions and audio/frame
timeline rather than adapting quality to export throughput. Saved settings are
not changed; the same capability and shutter fallbacks apply.

## Check native-resolution playback

The 2026-11-02 Apple M4 / Metal check used the ordinary `--profile` launch,
with no renderer-selection flag and no fixed-quality override. At the actual
5120 × 2818 backing extent, the 60 FPS run presented 600 measured frames at
**60.1 FPS** and ended at **1.00×**. Of 587 completed GPU timing samples,
**482 were native 1×**; measured samples ranged from 1× to 1.81×. The complete
startup log reached 2.18× briefly. Thus native quality is reachable and restored
automatically, while heavier moments can still reduce resolution.

The isolated ribbon-pass p95 was 6.428 ms and frame queue-span p95 was
13.656 ms; those timestamp spans retain the scope qualifications above. Raw
logs and samples are in `target/gpu-ribbon-default/native/default-60.log` and
`default-60-trace.json`.

Validation passed 658 library tests, 38 desktop/CLI tests, the all-targets and
all-features check, GPU projection/composition/timing/HUD trials, and the
fractional-FPS video-export integration test. The production UI trial includes
GPU blur-off rendering at shutter 16 and returns to swept shutter 2.

The normal launch and profile need no renderer flag. To verify a native model
target under the live adaptive controller, watch the downsize plot and actual
model dimensions. A controlled full-resolution profile can hold quality at 1×:

```sh
BENGA_PROFILE_FPS=60 BENGA_PROFILE_EXTENT=5120x2818 \
BENGA_PROFILE_RIBBON_DOWNSIZE=1 \
cargo run --release -- --profile
```

Omit `BENGA_PROFILE_RIBBON_DOWNSIZE` to test automatic restoration to 1×. The
profiler validates the actual backing extent and records GPU model/queue spans
separately from host timing and presentation cadence. A successful 1× allocation
is not itself a guarantee of a requested FPS under every model or zoom workload.

To reproduce a deterministic full-presentation comparison, run each command
separately with the standalone viewer closed:

```sh
BENGA_PIPELINE_BENCH_DIR=target/gpu-ribbon-integration/cpu \
BENGA_PIPELINE_EXTENTS=1920x1080,3840x2160 \
BENGA_PIPELINE_FRAMES=64 BENGA_PIPELINE_WARMUP=32 \
cargo test --release --lib measures_full_presentation_pipeline -- --ignored --nocapture --test-threads=1

BENGA_PIPELINE_GPU_RIBBONS=1 \
BENGA_PIPELINE_BENCH_DIR=target/gpu-ribbon-integration/gpu \
BENGA_PIPELINE_EXTENTS=1920x1080,3840x2160 \
BENGA_PIPELINE_FRAMES=64 BENGA_PIPELINE_WARMUP=32 \
cargo test --release --lib measures_full_presentation_pipeline -- --ignored --nocapture --test-threads=1
```

Use `BENGA_PIPELINE_WORKLOAD=poi_flight` for real showcase focus/camera motion;
`BENGA_PIPELINE_PREVIEW_FRAMES=0,31,63` saves identical animation moments. Results
record per-frame payload sizes and assert that GPU mode produced zero CPU model
lines/ribbons. Full-completion time includes audio, UI construction, rasterization,
GPU preparation/composition/resolve and completion waiting. The narrower GPU pass
timestamp excludes preparation and final resolve in GPU mode; compare full
completion to assess application cost. These bounded offscreen runs measure
single-frame completion and do not establish live playback FPS.

Offscreen runs retain full-resolution ribbons by default, so the commands above
remain suitable for comparing projection implementations. To compare fixed
quality levels on the same fast-zoom replay:

```sh
BENGA_PIPELINE_GPU_RIBBONS=1 BENGA_PIPELINE_RIBBON_DOWNSIZE=2 \
BENGA_PIPELINE_WORKLOAD=zoom_stress BENGA_PIPELINE_MODEL=mach_m249para \
BENGA_PIPELINE_EXTENTS=5120x2880 BENGA_PIPELINE_WARMUP=16 \
BENGA_PIPELINE_FRAMES=64 BENGA_PIPELINE_PREVIEW_FRAMES=0,31,63 \
BENGA_PIPELINE_BENCH_DIR=target/gpu-ribbon-adaptive/nearest-fixed-2x \
cargo test --release --lib measures_full_presentation_pipeline -- --ignored --nocapture --test-threads=1
```

Use a different output directory and factor for each comparison. Replace the
fixed-factor option with `BENGA_PIPELINE_ADAPTIVE_RIBBONS=1` to exercise the
controller against measured isolated frame completion, with
`BENGA_PIPELINE_FPS` as its target. Adaptive and fixed factors are mutually
exclusive. JSON records the actual per-frame model extent, linear downsize,
projection-parameter upload and CPU-projected upload (zero in production GPU
mode). This replay includes completion waiting and has no display pacing; its
controller behavior is useful for diagnosis, but it is not a native FPS result.

## Profiling tiny ribbon targets

The measurements in this and the following historical sections predate default
GPU promotion and the 1× adaptive floor. Their **2× adaptive minimum** and exact
settings remain recorded as measured. Fixed-resolution controls still describe
the selected factors; do not silently overwrite these archives with a new
adaptive run.

The 2026-11-02 Apple M4 / Metal investigation found that shrinking the model
target all the way to 1 × 1 could increase its draw cost. The initial measurements
use nearest-neighbour reconstruction and precede the motion-classification
correction described below. They are preserved as the reproduction of the slow
path, followed by measurements after the correction.

At a 5120 × 2880 physical output, the `mach_m249para` 3 Hz zoom script retains
all 69,225 source edges and shutter 2. A fixed 5120× linear downsize produces
a verified 1 × 1 ribbon attachment while foreground/UI and composition remain
at 5K. After 32 warmup frames, 96 measured frames gave:

| Component policy at 1 × 1 | Full completion median / p95 | Model/composition GPU median |
| --- | ---: | ---: |
| Full pipeline | 37.54 / 42.30 ms | 34.48 ms |
| Retain ribbons, omit GPU projection/preparation after three seed frames | 39.46 / 41.12 ms | 36.88 ms |
| Omit model | 4.72 / 5.55 ms | 2.12 ms |
| Omit foreground | 36.43 / 39.15 ms | 34.22 ms |
| Omit model and foreground | 2.34 / 4.33 ms | 0.86 ms |

All policies still build the complete CPU audio/UI/model scene. The retained
ribbon control freezes geometry and ink after three GPU frames, so its image
differs from the animated controls. Its unchanged high cost, the cheap omitted
model and the expensive omitted-foreground control locate the dominant cost in
model drawing for this reproduction. This does not measure an isolated GPU
projection cost by subtracting the two totals. The model query covers ribbon
rasterization and float composition, excludes projection/preparation and final
resolve, and can include dependencies on preceding work. Raw data is in
`target/gpu-ribbon-profile-1px/components/results.json`.

A separate fixed-resolution sweep used the same 5K model/camera workload,
32 warmup and 64 measured frames at 60 Hz animation times:

| Linear downsize | Ribbon target | Full completion median / p95 |
| --- | --- | ---: |
| 1× | 5120 × 2880 | 37.85 / 87.42 ms |
| 2× | 2560 × 1440 | 15.21 / 29.92 ms |
| 4× | 1280 × 720 | 10.01 / 13.91 ms |
| 8× | 640 × 360 | 9.27 / 10.31 ms |
| 16× | 320 × 180 | 9.53 / 10.62 ms |
| 32× | 160 × 90 | 9.49 / 11.91 ms |
| 64× | 80 × 45 | 10.02 / 14.96 ms |
| 128× | 40 × 23 | 9.92 / 17.48 ms |
| 320× | 16 × 9 | 11.30 / 18.10 ms |
| 5120× | 1 × 1 | 36.94 / 39.03 ms |

The cost is non-monotonic: 1 × 1 is much slower than 640 × 360 in this version.
Raw samples are in
`target/gpu-ribbon-profile-1px/resolution-sweep/results.json`. Reduced pixel
count alone therefore cannot be treated as evidence that a resolution change
helped. The shader still processes source primitives, and very coarse targets
concentrate their blending into a small number of pixels. This sweep alone does
not establish a general one-pixel hardware limit; the classification error below
was present in these samples.

A GPU regression fixture identified a specific error in the coarse path:
20 physical pixels of ordinary sideways edge motion could be classified as
parallel motion at a 1 × 1 target. The enlarged antialiasing footprint had been
used as the motion-classification threshold. Classification now uses the
physical stroke half-width, while coverage retains its larger coarse-pixel
footprint. The regression first failed with two parallel intervals at 1 × 1 and
passes with that correction.

The sweep repeated after this correction used the same configuration, camera
poses, source-edge counts, audio/UI scene inputs, factors and frame counts.
These input fields match exactly for all ten factors; verification is recorded
in `target/gpu-ribbon-profile-1px/classification-sweep-matched-inputs.json`.
Selected full-completion results are:

| Ribbon target | Before median / p95 | After median / p95 |
| --- | ---: | ---: |
| 5120 × 2880, full resolution | 37.85 / 87.42 ms | 41.05 / 96.94 ms |
| 640 × 360, 8× downsize | 9.27 / 10.31 ms | 9.49 / 12.04 ms |
| 320 × 180, 16× downsize | 9.53 / 10.62 ms | 9.08 / 11.34 ms |
| 16 × 9, 320× downsize | 11.30 / 18.10 ms | 6.38 / 7.84 ms |
| 1 × 1, 5120× downsize | 36.94 / 39.03 ms | 7.07 / 8.87 ms |

In the matched 1 × 1 sweep case, the model/composition GPU median fell from
**33.62 ms to 3.78 ms**. At the last captured pose, animation frame 95/time
1.6 seconds, active parallel intervals fell from **134,184 to 404**; active
triangles changed from 4,260 to 136,957. All 69,225 source edges are retained.
The result therefore changes which prepared shader path handles the motion,
without source-edge filtering. Low-resolution shading remains approximate and
changes with the corrected classification. The full-resolution classification
math is unchanged, and these sequential runs show no uniform performance gain
at full resolution or 8× downsize.

Full after-correction samples are in
`target/gpu-ribbon-profile-1px/resolution-sweep-after-classification/results.json`.
The CPU coverage diagnostic's coarse-pixel padding was corrected during this
investigation too. The old coarse-support area totals underreport coverage;
use the primitive/classification counts for this before/after comparison, rather
than treating old and new area totals as equivalent measurements.

### Native timing check

The native profiling options select FPS, requested physical extent, fixed ribbon
downsize and performance-overlay visibility. An initial request for 5120 × 2880
was clamped by the window system to **5120 × 2818**. The app printed timings for
that actual extent and deliberately returned a nonzero result for the mismatch.
Later comparisons requested the accepted extent explicitly.

Before the classification correction, the 60 FPS native run with a 1 × 1 ribbon
target and performance overlay enabled presented at 56.6 FPS. Host redraw-work
p95 was 21.409 ms and surface-acquisition p95 was 19.895 ms. With the overlay
disabled, a separate run presented at 53.6 FPS. These sequential samples vary
and do not establish an overlay speedup or slowdown. At fixed 8× downsize
(640 × 353 ribbons) with the overlay enabled, another run presented at 60.0 FPS;
redraw-work p95 was 5.451 ms and acquisition p95 was 0.113 ms.

After correcting the motion-classification threshold, the native 1 × 1 run with
the same accepted 5120 × 2818 extent and overlay enabled presented **600 frames
at 60.0 FPS**. Redraw-work p95 was **5.455 ms** and acquisition p95 was
**0.110 ms**, compared with 21.409 ms and 19.895 ms before correction. The
corrected tiny target therefore reached the requested cadence in this bounded
synthetic workload. Its log is
`target/gpu-ribbon-profile-1px/native/fixed-1px-60-after-classification.log`.

The following adaptive captures used the preceding windowed controller, before
the per-frame response described above. The fixed-quality shader comparisons
remain independent of that controller change.

An adaptive 60 FPS run presented 600 frames at **60.0 FPS** in the same
5120 × 2818 window. It briefly tried 4× downsize and returned to the **2×
baseline**. Measured redraw-work p95 was 5.610 ms and acquisition p95 was
0.100 ms; cadence p95 was 18.036 ms, so average 60 FPS does not imply every
present-call interval is exactly 16.667 ms. The log is
`target/gpu-ribbon-profile-1px/native/adaptive-60-final.log`.

With the downsize graph added, a later run in the same window again presented
600 frames at **60.0 FPS**, maintaining the 2× baseline. Redraw-work p95 was
5.330 ms; presentation-cadence p95 was 18.263 ms. The three-graph run is saved in
`target/gpu-ribbon-profile-1px/native/adaptive-60-downsize-hud.log`. Its update
passed 617 library tests, the all-targets/all-features check, and GPU preview
rendering for dark, light, narrow-window and HiDPI layouts.

At that revision, validation passed 615 library tests, 11 separately enabled
GPU tests, and the all-targets/all-features build check. Regression tests cover nearest texel
blocks in both blend modes, physical motion classification across target sizes,
recovery from tiny held targets while the requested FPS remains unreachable,
and balanced presentation jitter with inexpensive redraw work.

These are host wall-clock intervals. Acquisition can wait for VSync or GPU
backpressure, and its duration is not a GPU execution timestamp. Successful
presentation cadence also includes scheduling and display pacing; it is not
monitor scanout latency. The raw native logs are under
`target/gpu-ribbon-profile-1px/native/`.

### Reproduce the profiling protocol

Use a fresh directory for each revision to preserve the before-correction data:

```sh
BENGA_PIPELINE_GPU_RIBBONS=1 BENGA_PIPELINE_RIBBON_DOWNSIZE=5120 \
BENGA_PIPELINE_COMPONENTS=full,no_gpu_prepare,no_model,no_foreground,no_model_no_foreground \
BENGA_PIPELINE_WORKLOAD=zoom_stress BENGA_PIPELINE_MODEL=mach_m249para \
BENGA_PIPELINE_EXTENTS=5120x2880 BENGA_PIPELINE_WARMUP=32 \
BENGA_PIPELINE_FRAMES=96 \
BENGA_PIPELINE_BENCH_DIR=target/gpu-ribbon-profile-1px/components-after-classification \
cargo test --release --lib measures_full_presentation_pipeline -- --ignored --nocapture --test-threads=1

BENGA_PIPELINE_GPU_RIBBONS=1 \
BENGA_PIPELINE_RIBBON_DOWNSIZES=1,2,4,8,16,32,64,128,320,5120 \
BENGA_PIPELINE_WORKLOAD=zoom_stress BENGA_PIPELINE_MODEL=mach_m249para \
BENGA_PIPELINE_EXTENTS=5120x2880 BENGA_PIPELINE_WARMUP=32 \
BENGA_PIPELINE_FRAMES=64 \
BENGA_PIPELINE_BENCH_DIR=target/gpu-ribbon-profile-1px/resolution-sweep-after-classification \
cargo test --release --lib measures_full_presentation_pipeline -- --ignored --nocapture --test-threads=1

BENGA_PROFILE_FPS=60 BENGA_PROFILE_EXTENT=5120x2818 \
BENGA_PROFILE_RIBBON_DOWNSIZE=5120 BENGA_PROFILE_PERFORMANCE_OVERLAY=1 \
cargo run --release -- --experimental-gpu-ribbons --profile
```

The plural `BENGA_PIPELINE_RIBBON_DOWNSIZES` runs fresh matching workloads per
factor and excludes the singular fixed-factor and adaptive options. Sweep
previews include their factor in each filename. `no_gpu_prepare` requires GPU
ribbons, fixed resolution and at least three GPU warmup frames. For the native
check, change `BENGA_PROFILE_RIBBON_DOWNSIZE` to `8` for the larger ribbon target
or `BENGA_PROFILE_PERFORMANCE_OVERLAY` to `0` to omit the performance HUD. The
profile remains bounded to one second of warmup and ten seconds of measurement;
it reports actual backing size and checks any requested extent afterward.

## High FPS pacing and resolution feedback

The 2026-11-02 Apple M4 / Metal high-FPS investigation reproduced the quality
collapse at a requested **240 FPS** in the actual **5120 × 2818** native window.
Pacing metadata reported a **240 Hz** monitor, an effective **4.167 ms** adaptive
budget, desired maximum frame latency 2, and supported present modes
`Fifo`/`Immediate`. These measurements do **not** show a hard 120 Hz display cap.

Three bounded native runs with the performance overlay enabled gave:

| Before feedback correction | Actual ribbon policy | Presented FPS | Acquisition p50 / p95 |
| --- | --- | ---: | ---: |
| AutoVsync, adaptive | 2× eventually collapsed to 1 × 1 | 130.0 | 6.372 / 8.779 ms |
| AutoVsync, fixed 2× | 2560 × 1409 throughout | 130.7 | 6.384 / 6.893 ms |
| Immediate, fixed 2× | 2560 × 1409 throughout | 128.1 | 5.812 / 9.483 ms |

Keeping the much finer 2× image gave essentially the same presentation rate as
the collapsed adaptive target. Turning synchronization off did not raise the
rate in this sample either. The remaining delivery/acquisition ceiling therefore
cannot be assigned solely to VSync or timer phase arithmetic from these data.
Acquisition can include GPU or compositor backpressure; these host measurements
do not identify the internal source of that wait.

The quality controller had a separate, reproducible feedback error: it counted
acquisition waiting as render work and used individual late intervals to assess
resolution changes. At a 4.167 ms requested budget, several milliseconds of
waiting look like overload even when active rendering is inexpensive. Jitter
could also appear to improve after each cut and keep resetting the fixed-cost
rollback. A deterministic controller test with approximately 3 ms of active
work and 120 Hz delivery reproduced collapse to a 1 × 1 target without any
resolution-dependent render cost.

Host-only averaging and acquisition subtraction were insufficient on the live
machine. The final path uses asynchronous GPU timestamps for both the ribbon
pass and the surrounding queue span, with fixed-work budget allocation and a
measured scaling-benefit checkpoint. Changes in the fixed allowance are not
credited as model-rendering improvements, and holding a resolution between cuts
does not erase evidence of unhelpful cuts. Old-quality samples can refine scaling
evidence, while only matching-quality samples may drive a new control step.

Redraw pacing and the adaptive budget also share `presentation_interval`. That
corrects a cross-machine mismatch where a requested 180/240 FPS timer could
enqueue faster than a 120/144 Hz display-derived quality budget. It does not
remove this run's throughput ceiling: its reported 240 Hz mode already matches
the requested rate.

Final bounded runs with all three HUD plots enabled gave:

| Requested FPS | Presented FPS | Logged downsize range | Final downsize |
| --- | ---: | ---: | ---: |
| 120 | 120.0 | 2.00–6.75× | 2.72× |
| 180 | 132.2 | 2.00–3.45× | 2.10× |
| 240 | 130.2 | 2.00–3.22× | 2.00× |

The factor ranges include startup; FPS follows the ten-second measured phase.
The image no longer collapses to a one-pixel target or remains stuck there.
The 5K throughput ceiling remains. A separate fixed-2× 1920 × 1080 run on the
same reported 240 Hz monitor reached **237.5 FPS**, which rules out a general
120 FPS timer cap. The resolution dependence points to the full-resolution
pipeline/presentation path, rather than a benefit from further model reduction.
These are sequential local observations, not controlled GPU-utilization results.

The 180/240 results are in `180-adaptive-final.log` and
`240-adaptive-final-counter-validation.log`; the 120 result is
`120-adaptive-scaling-guard.log`. The latest counter decoder validates each
interval independently because Metal stage intervals can overlap; it does not
require the model interval to be contained in the surrounding queue interval.
Corresponding JSON GPU/host traces are available alongside the logs. Invalid or
incomplete timestamp samples are skipped without blocking, so their count can
be lower than the presented frame count. The lower-extent control is
`240-fixed2-1080-before.log`. All paths are under
`target/gpu-ribbon-high-fps/native/`.

The profile-only `BENGA_PROFILE_PRESENT_MODE` option accepts `auto_vsync`, `fifo`
and `immediate`, validates explicit modes against adapter support, and reports
the configured mode, refresh, effective budget and surface extent. The default
playback mode remains AutoVsync. The native profiler reports both total host
redraw work and active redraw work excluding acquisition; neither is a GPU
execution timestamp. For a controlled repeat, keep the extent and overlay
policy fixed and preserve logs from each revision:

```sh
BENGA_PROFILE_FPS=240 BENGA_PROFILE_EXTENT=5120x2818 \
BENGA_PROFILE_RIBBON_DOWNSIZE=2 BENGA_PROFILE_PRESENT_MODE=auto_vsync \
cargo run --release -- --experimental-gpu-ribbons --profile
```

Omit the fixed-downsize variable for the adaptive case, or select `immediate`
for the synchronization control. Before-correction samples are
`target/gpu-ribbon-high-fps/native/240-adaptive-before.log`,
`240-fixed2-fifo-before.log` and `240-fixed2-immediate-before.log`.
The final update passed 654 library tests and the all-targets/all-features check.
GPU checks cover the timing ring, frame-tag validity, model projection,
composition and the HUD. No throughput improvement is claimed for the 5K path;
the correction preserves useful image detail when the requested FPS is beyond it.
Set `BENGA_PROFILE_TRACE_OUTPUT=path.json` to save bounded host/GPU samples after
measurement for a replay. Normal profiling performs no trace-file I/O.

## Immediate per-frame controller check

The per-frame policy passed 626 library tests and the all-targets/all-features
build check. Its 20 direct controller tests cover the first near-deadline cut,
the first cheap-frame restoration, one-frame prediction of rising cost, safe
retry after an oversized quality increase, and presentation-jitter protection.

A native 5120 × 2818 run with the three performance graphs enabled presented
600 measured frames at 60.1 FPS for a 60 FPS target. It restored an initial
4× reduction through 2.79× to the 2× baseline. Measured redraw-work p95 was
5.137 ms; cadence p95 was 18.429 ms, which still includes scheduling and display
pacing. This is a bounded synthetic playback check, not a scanout guarantee.
The log is `target/gpu-ribbon-immediate/native/adaptive-60-final.log`.

The 5K M249 zoom stress replay used 32 warmup and 120 measured frames with
immediate feedback and the cost-trend forecast. Isolated completion was
11.27 ms median / 18.51 ms p95 / 24.19 ms peak, with 20 frames over 16.67 ms.
The factor ranged from 2× to 4.12× and changed on 81 adjacent measured frames.
This demonstrates frame-by-frame adaptation; sudden zoom growth can still miss
the deadline. Its samples are in `target/gpu-ribbon-immediate/zoom-trend/`.

## Historical bilinear adaptive-resolution replay

These measurements predate nearest-neighbour reconstruction, periodic
finer-quality probes and the current per-frame controller. They used the earlier recovery controller, which could
remain at a coarse target while fixed presentation costs prevented reaching its
FPS budget. The results below describe that earlier version; they do not
validate the current recovery behavior. Preserve these archives when collecting
new measurements.

Measured on 2026-11-02, Apple M4 / Metal, at a **5120 × 2880 physical output**:
`mach_m249para`, all 69,225 source edges, shutter 2, the default 3 Hz
`zoom_stress` camera script, 16 warmup frames and 64 measured frames at
deterministic 60 Hz animation times. All three runs use GPU projection, float
accumulation, full-resolution foreground/UI and final surface conversion.
Render settings, active-edge counts, camera inputs and preview animation
moments match across the runs; only ribbon resolution policy differs.

| Ribbon resolution | Full completion median / p95 / peak | Frames over 16.67 ms |
| --- | ---: | ---: |
| Full resolution, 5120 × 2880 | 34.00 / 98.65 / 101.45 ms | 47 / 64 |
| Fixed 2× downsize, 2560 × 1440 | 21.79 / 43.63 / 50.28 ms | 43 / 64 |
| Adaptive, 60 Hz completion budget | 12.54 / 18.62 / 23.16 ms | 16 / 64 |

Adaptive starts at 2× during warmup. During the measured frames its linear
downsize ranges from **4.038× to 5.683×**, ending at 5.683×; the model target
changes from 1269 × 714 to 901 × 507. The controller uses repeated slow-frame
costs so inexpensive poses cannot hide recurring expensive sweeps. It reacts
after observing them, and this replay still exceeds the 16.67 ms budget on
16 frames. These results show lower isolated completion cost at reduced image
quality; they do not establish steady 60 FPS in native playback.

A separate convergence check keeps the same 5K model/camera setup and 60 Hz
completion budget, with **120 warmup and 120 measured frames**. It records
10.48 ms median, 16.43 ms p95 and 19.08 ms peak; 5 of 120 frames exceed
16.67 ms. Measured linear downsize ranges from 2.815× to 3.232× and ends at
3.232× (1585 × 892 model target). This longer adaptive run covers later
animation moments and has no matching full-resolution control; it tests
controller behavior rather than a paired speedup. Its p95 is within budget,
while individual frames still miss it. This does not guarantee every-frame
deadlines or native display scanout. Raw results are in
`target/gpu-ribbon-adaptive/adaptive-steady-final/`.

Each frame reports zero CPU-projected pose upload, zero CPU-expanded model
geometry, a 96-byte projection parameter block and 1,107,600 bytes of edge ink.
The selected-payload report excludes the additional 16-byte projection-ring
metadata, ribbon frame uniforms and initial immutable source upload.

This replay submits one frame through completion before the next, with no
display pacing. Adaptive feedback receives isolated full-frame completion as
both interval and work; native feedback instead uses presentation cadence and
redraw wall time, including surface backpressure. Short, unpaced replay
measurements cannot predict native controller settling or latency.
Raw samples and matched PNG moments 0, 31 and 63 are in
`target/gpu-ribbon-adaptive/{full-resolution,fixed-2x,adaptive-final}/`.
`target/gpu-ribbon-adaptive/matched-settings.json` records the scene/settings
comparison.

The full-resolution and fixed-2× archives retain their original camera inputs
and timing samples. Their untimed `projected_bounds`/`visible_vertices` fields
predate the lazy-projection diagnostic correction and are empty in GPU mode.
The final adaptive archives contain populated bounds and 27,859–31,168 visible
source vertices. The comparison therefore checks camera inputs separately from
these corrected diagnostic fields. The current snapshot projects source vertices
on the CPU only during the untimed diagnostic scan; it does not add CPU
projection or readback to playback.

A bounded native `--experimental-gpu-ribbons --profile` smoke of that earlier
version also completed with GPU projection/ribbons active on Apple M4 / Metal
at 1920 × 1080. At the
profile's 240 FPS target, resolution cuts reached a fixed-cost limit; the guard
rolled back unproductive probes and held approximately 9.69× downsize
(199 × 112 model target). This checks native backend integration and rollback,
without establishing a controlled native speedup or an achievable 240 FPS
budget. That smoke predates nearest-neighbour reconstruction and periodic
finer-quality probes; the newer fixed-resolution native check is recorded in
the tiny-target profiling section above. Adaptive sessions start at 2× downsize.

## Earlier CPU-projection integration measurements

Local integration measurements (2026-11-02), Apple M4 / Metal, 69,225 source
edges, 64 measured frames after 32 GPU warmup frames, 60 Hz deterministic poses.
POI flight additionally advances 120 CPU frames before GPU warmup. These are
full-resolution production presentation passes, including foreground/UI and
completion waiting, with identical animation times and settings per pair.
CPU uses its existing `Bgra8Unorm` composition; the experimental path uses
`Rgba16Float` and a final surface conversion. Neither changes saved preferences.

| Workload | Physical pixels | CPU median / p95 | GPU bridge median / p95 |
| --- | --- | ---: | ---: |
| Controlled rotation | 1920 × 1080 | 9.32 / 11.35 ms | 6.38 / 8.14 ms |
| Controlled rotation | 3840 × 2160 | 10.78 / 12.92 ms | 7.52 / 9.30 ms |
| POI camera flight | 1920 × 1080 | 10.74 / 15.15 ms | 6.27 / 8.13 ms |
| POI camera flight | 3840 × 2160 | 12.86 / 18.08 ms | 7.66 / 10.17 ms |

At 4K, median `Ui.draw` fell from 3.10 to 0.40 ms in controlled rotation and
from 4.21 to 0.44 ms in POI flight. The narrow model/composition GPU pass itself
was slower in GPU mode; the full-frame gain comes from removing CPU expansion
and its transfer/encoding costs. The GPU pose payload was 498,688 bytes and style
payload 1,107,600 bytes each frame, versus median CPU-expanded geometry of
26.61 MB (rotation) and 28.78 MB (flight). Every measured GPU frame had zero CPU
model line/ribbon payload.

The preceding standalone viewer was absent during measurement. A bounded native
`--experimental-gpu-ribbons --profile` run also completed successfully with the
GPU backend active. Native pacing results are not used for the table. Automated
CPU bridge, GPU image and composition tests cover style/depth equivalence,
independent exchange histories, actual middle-pose reversals, timestamp weights,
resets, deterministic blend order, faint ink and foreground/control ordering.

Raw samples, settings, scope metadata and matched PNGs are in the ignored
`target/gpu-ribbon-integration/{cpu-normal,gpu-normal,cpu-poi,gpu-poi}/` directories.
These two workloads improved in this local run. This does not remove the known
standalone along-edge/ordinary-motion regressions or the crossing-brightness
approximation, and is insufficient evidence for making this renderer default.

The ignored `production_ui_gpu_trial_with_audio_focus_transitions_resize_and_switches`
test also passed on Metal: 80 frames through the real `Ui` and production offscreen
composition, with analyzer audio, actual selector/focus/slide callbacks, model
exchanges, both theme composites, diagnosis, resizing, zoom, R-equivalent
switches and shutter fallback/recovery. Ten visual captures are retained in
`target/gpu-ribbon-integration/trial/`. Readback is confined to tests/benchmark
capture. The later native controller reads timestamp words asynchronously; it
does not read those trial images or geometry during playback. Paired comparison metadata confirms
identical render settings, active-edge counts and POI/camera snapshots.

## Fast zoom at 5120 × 2880

A deterministic zoom replay reproduces the large spikes in both renderers. This
is a scripted stress case through production projection, with the largest source
model, full resolution, all source edges and shutter 2. It does not claim to
reconstruct an unrecorded live music session exactly.

```sh
BENGA_PIPELINE_GPU_RIBBONS=1 BENGA_PIPELINE_WORKLOAD=zoom_stress \
BENGA_PIPELINE_EXTENTS=5120x2880 BENGA_PIPELINE_WARMUP=8 \
BENGA_PIPELINE_FRAMES=32 BENGA_PIPELINE_MAX_FRAME_MS=80 \
BENGA_PIPELINE_BENCH_DIR=target/gpu-ribbon-zoom/repro \
cargo test --release --lib measures_full_presentation_pipeline -- --ignored --nocapture --test-threads=1
```

`BENGA_PIPELINE_MAX_FRAME_MS` deliberately returns a failing verdict when the
peak exceeds its budget, after saving every sample and the peak's camera and
projected bounds. Use `BENGA_PIPELINE_MIN_PEAK_MS=80` instead to assert that a run
reproduces the slow case. Set the GPU flag to `0` for the same CPU pose sequence.
Zoom controls are `BENGA_PIPELINE_ZOOM_NEAR/FAR/MIN/MAX/HZ`, with defaults
`0.7/4.2/0.7/3/3`. These control camera distance, camera magnification and cycles
per second; the production projector and near-plane rules remain unchanged.

Apple M4 / Metal diagnostic runs, 32 measured frames after 8 warmup frames:

| Control | Median | p95 | Peak |
| --- | ---: | ---: | ---: |
| GPU zoom, 5120 × 2880, 60 Hz poses | 40.27 ms | 95.69 ms | 97.55 ms |
| CPU zoom, same poses | 53.37 ms | 151.71 ms | 169.79 ms |
| GPU zoom, foreground omitted | 36.78 ms | 91.80 ms | 99.03 ms |
| GPU float composition, model omitted | 4.12 ms | 4.54 ms | 4.55 ms |
| GPU same close-up held still | 7.65 ms | 9.73 ms | 9.78 ms |
| GPU zoom, 2560 × 1440 diagnostic extent | 14.93 ms | 29.91 ms | 31.11 ms |
| GPU zoom, 5120 × 2880, 15 Hz poses | 156.97 ms | 180.85 ms | 183.46 ms |

The smaller extent is an attribution experiment; playback resolution is not
reduced. The empty-model control retains the selected renderer's composition
format and final resolve, and still builds the CPU UI scene. At the first
97.55 ms GPU peak, `Ui.draw` took 0.42 ms and the model/composition GPU pass took
93.69 ms. Removing the foreground barely changes the peak. Stationary geometry
and the empty float target are much cheaper. Together with resolution scaling,
this identifies motion-ribbon pixel processing/overlap as the dominant cost in
this reproduction, rather than CPU projection or fixed float-target conversion.
Larger displayed-pose steps can make the swept support much more expensive;
CPU-to-GPU projection migration alone cannot remove that cost.

A conservative parallel-strip support experiment is available only with
`BENGA_GPU_RIBBON_PARALLEL_BOUNDS=tight`. It preserves byte-identical float pixels
in the Metal regression fixtures. It is **disabled by default**: alternating
original/tight/original 64-frame runs at 5K gave medians 41.64/41.07/33.67 ms and
p95 119.34/121.20/113.09 ms, showing no consistent improvement for fast zoom.
The original support already uses oriented rectangles. At the repeated slow
pose, the CPU source-coverage diagnostic counted 59,221 ordinary/body edges and
only 115 parallel edges, so tightening parallel support targets very little of
this workload. Area diagnostics count continuous geometric bounds, not fragments.

The large zoom spike remains unresolved. A larger optimization must target the
many overlapping swept-triangle contributions and their accumulation; dropping
source edges, shrinking the render target or shortening the shutter was not used.
The current bounds experiment does not establish that a different accumulation
architecture will be faster; that needs its own full-resolution prototype and
image/performance comparison, including multiplicative themes.

Raw samples and comparisons are under `target/gpu-ribbon-zoom/`. CPU-only source
coverage accounting runs with `cargo test --release --lib reports_zoom_source_coverage -- --ignored --nocapture`
and writes `target/gpu-zoom-repro/coverage/results.json`.

To attribute a live occurrence without GPU readback or changing saved settings:

```sh
BENGA_TRACE_SLOW_FRAMES=1 cargo run --release -- --experimental-gpu-ribbons
```

The trace prints the actual physical backing extent on startup/resize. Frames
at least 50 ms report the model/backend, cadence, redraw wall time, time outside
the redraw callback, UI construction, surface acquisition, rasterization and
encode/submission. Slow bursts are aggregated once per second. Acquisition and
submission can wait on GPU backpressure; their host wall time is not a GPU
execution timestamp. First-use setup stalls and sustained zoom overdraw can
therefore be distinguished without mislabelling every CPU-wall spike as CPU
geometry work.

## Rejected small-edge filtering trial

The screen-relative edge-length cutoff experiment was removed after visual
review: useful speedups required losing too much mesh detail and did not reliably
remove the worst zoom spikes. Playback again retains all source edges in both
renderers, with no cutoff launch flag, hotkeys or shader branch. The R renderer
toggle, full-resolution float accumulation, performance graphs and zoom/slow-frame
diagnostics remain. Ignored measurements and captures under
`target/gpu-ribbon-edge-cutoff/` are retained as the record of the rejected trial.

## Full-screen ribbon capacity and asset budget

Measured on 2026-11-02, **Apple M4 / Metal, AC power**, using the current prepared
GPU ribbon shaders, full-resolution `Rgba16Float` accumulation and final
`Bgra8Unorm` resolve. No geometry filtering, resolution reduction or shutter
shortening is used. The normal app is unchanged; the new hooks are benchmark-only.

One regular full-screen ribbon means one full-height source edge whose actual
three poses move left → center → right across the viewport. Its two temporal
wings plus middle body cover exactly one viewport, as verified from the prepared
GPU records. A left → right → left reversal covers it twice. The crossing fixture
covers approximately half a viewport (measured 0.502404 at 5K), so logical ribbon
counts across fixtures are not directly interchangeable.

**Budget overlapping rasterized area, not edge count or visible-pixel union.**
One thousand ribbons each covering 2% of the screen represent 20 full-screen
equivalents. Add current and outgoing models together. Faint contributions still
cost rasterization/blending work unless their ink is exactly zero and culled.

| Physical target | Additional regular screen layer | GPU-only 60 Hz ceiling | Suggested starting asset budget |
| --- | ---: | ---: | ---: |
| 5120 × 2880 | about 0.53 ms | about 30 screen-equivalents | 15–20 combined screen-equivalents |
| 3840 × 2160 | about 0.30 ms | about 54 screen-equivalents | 30–35 combined screen-equivalents |

The slope corresponds to roughly 28 billion full-screen-equivalent pixel
contributions/second for this ideal large-primitive workload. It is not the
chip's theoretical peak. Dark/additive and light/multiplicative modes were close
in this test, and sweep/reversal/crossing costs tracked their measured area.

Fine runs used 16 warmup and 64 measured frames per count. At 5K, 30 regular
ribbons took 16.38 ms median / 16.44 ms p95 in additive mode; 31 took 16.89 /
17.00 ms. Multiplicative results were 16.37 / 16.45 ms at 30 and 16.87 / 16.98 ms
at 31. At 4K, 54 took 16.42 / 16.65 ms additive and 16.41 / 16.60 ms multiply;
56 exceeded the budget at about 17.2 ms p95. The 4K count 55 was not tested.
These are local p95 render-budget bounds, not guarantees that every frame meets
its deadline.

The rest of the real offscreen presentation pipeline, with model draws omitted
but float composition retained, took 4.17 / 4.55 ms median/p95 at 5K and 3.15 /
3.54 ms at 4K. Its CPU scene/projection work remains included; GPU edge preparation
is absent in that control. The hardware-capacity fixture freezes preparation,
uses only two shared projected vertices and repeats the source edge. It excludes
real model preparation/upload, audio, Vello foreground/UI, surface acquisition,
OS composition and display. Thus the raw ceiling is too high for an asset budget.
The starting budgets above are conservative targets that still need live asset
validation, especially with many small primitives.

A paced offscreen verification submitted 180 frames at 60 Hz for each case:

- At 5K, 20 ribbons completed all 180 with no host-observed completion misses;
  p95 scheduled-to-completion time was13.47 ms. At 30, all 180 completed at 60 FPS
  cadence but with roughly17.76 ms p95 host latency and two frames in flight.
  At 32, completion cadence fell to 57.77 FPS and 5 scheduled frames were skipped
  to bound the queue to three frames.
- At 4K, 36 completed all 180 with no host-observed completion misses and13.37 ms
  p95 host latency. At 54, all 180 completed at 60 FPS cadence with 17.81 ms p95
  latency/two frames in flight. At 56, cadence fell to 59.18 FPS with one skipped
  scheduled frame over this short run.

Callbacks were polled at 0.5 ms intervals; host latency includes polling and OS
scheduling delay. These are headless submission/completion tests, not monitor
scanout measurements. Stable 60 FPS cadence and sub-16.67 ms end-to-end latency are
different criteria. The practical asset budget leaves headroom for both.

A deliberately minimal flat-ink full-screen blending control was also measured,
with the same float target, background and final resolve. It used a resource-free
fragment shader and one triangle per layer, without ribbon coverage, AA or masks.
At 5K, 89 layers took about16.52/16.61 ms additive and16.51/16.59 ms multiply;
90 exceeded16.67 ms p95. An additive88-layer run had a 17.83 ms p95 outlier despite
a 16.32 ms median. This is an optimistic artificial fill reference, not ribbon
capacity: uniform colors can benefit compression/tile fast paths, so the gap
does not establish a threefold ribbon-shader speedup.

### Reproduce the hardware measurement

Close other Benga/prototype viewers, then run:

```sh
BENGA_CAPACITY_EXTENTS=5120x2880,3840x2160 \
BENGA_CAPACITY_FIXTURES=sweep,reversal,crossing \
BENGA_CAPACITY_COUNTS=0,1,8,16,24,32,48,64 \
BENGA_CAPACITY_WARMUP=12 BENGA_CAPACITY_SAMPLES=32 \
BENGA_CAPACITY_OUTPUT=target/ribbon-capacity/matrix \
cargo test --release --lib measures_fullscreen_ribbon_capacity -- --ignored --nocapture --test-threads=1
```

`BENGA_CAPACITY_BLENDS=additive,multiply` is the default. `...BATCH=8` measures
queued batch throughput; individual queued pass spans are not isolated frame
cost. `...PACED_FRAMES=180` additionally checks 60 Hz submissions without query
readback during replay. `...PREVIEWS=1` captures N=1 images and validates actual
prepared support area. `...FLAT_CONTROL=1` with `...FIXTURES=sweep` selects the
separate minimal-fill reference. Default count sweeps stop after the first case
above 33.33 ms unless explicit counts are supplied.

An eight-frame queued 5K check agreed with the isolated boundary: 20/30/32
regular screen layers took 11.16/16.40/17.41 ms p95 per frame using the whole GPU
batch span divided by eight. Individual pass intervals in those queued batches
were much longer because pipeline stages overlap; the batch-span metric is the
throughput result. Raw queued data is in `target/ribbon-capacity/queued-5k/`.

GPU total spans run from float composition begin through final resolve end.
Individual pass intervals can overlap/include dependencies on Metal; do not add
composition and resolve intervals. Timestamp resolution/readback occurs after
host-observed completion and outside frame-completion timing. Raw samples,
JSON scopes and CSV tables are saved under `target/ribbon-capacity/`, including
`matrix`, `fine-5k`, `fine-4k`, `paced-5k`, `paced-4k`, `flat-ceiling`, `flat-fine`,
`validation` and `application-baseline`.

### Measure a specific asset before and after editing

```sh
BENGA_PIPELINE_GPU_RIBBONS=1 BENGA_PIPELINE_WORKLOAD=zoom_stress \
BENGA_PIPELINE_MODEL=mach_m249para BENGA_PIPELINE_EXTENTS=5120x2880 \
BENGA_PIPELINE_WARMUP=16 BENGA_PIPELINE_FRAMES=18 \
BENGA_PIPELINE_COVERAGE=1 BENGA_PIPELINE_BENCH_DIR=target/ribbon-capacity/my-asset \
cargo test --release --lib measures_full_presentation_pipeline -- --ignored --nocapture --test-threads=1
```

The explicit model ID prevents the benchmark silently switching to another
heaviest asset after an edit. The coverage report captures the last rendered
frame, so these counts select animation frame 33/time 0.566667 s. Change frame counts
or the zoom-script controls to inspect other moments. Re-run with lower virtual
pose cadence (`BENGA_PIPELINE_FPS=15`) to test larger history steps too.

`cases[0].raster_coverage` records continuous viewport-clipped prepared triangle
and parallel-rectangle area, counting overlap repeatedly, with totals by shader
class/model. It is not a fragment-invocation query, opacity-weighted area, visible
brightness or union coverage. Source IDs refer to the prepared/welded model.
The top 20 expensive source edges include part IDs/names and normalized model-space
endpoints for inspection. Buffer readback/CPU analysis occurs once after the last
frame and outside timing; it is unavailable in normal playback.

The unfiltered M249 replay at frame33 produced **129.74 screen-equivalents**:
129.71 regular triangles and0.0304 parallel support, with 305,977 active triangles
and 217 parallel intervals. It took 92.79 ms full completion /89.03 ms model GPU
interval in the explicit-asset run. This is about 6.5–8.6 times the suggested 5K
starting asset budget. The real mesh is more expensive than ideal giant ribbons
of the same geometric area, so area is a first-order budget rather than an exact
predictor. The detailed report is `target/ribbon-capacity/asset-selected/results.json`.

Some reported edge pairs have identical source positions in different parts
(for example carry_handle edge 11134 and receiver edge 59240). Inspect whether
those are intentional shared boundaries or duplicated mesh content; the
benchmark does not automatically weld or remove them. No asset was edited.
