# Scene transition performance

Model changes cut directly to the requested model. Theme colors continue their
0.2-second interpolation independently. The first rendered frame after a cut
contains the complete new model, with fresh motion history and no outgoing-model
contribution.

Rendering uses the GPU path throughout. Motion blur uses a fixed two-frame
shutter or can be disabled; there is no CPU-renderer switch. Renderer statistics
start hidden in each session. Hold a modifier key and use **Developer: show
renderer stats** beside the analyzer button to reveal them; the same button hides
them again. Analyzer and renderer-stat visibility are independent.

## Model swapping and uploads

- The model catalog is parsed and prepared before animation. Prepared wireframes
  and point-of-interest data are shared. The CPU reuses two reserved animation
  slots: one current model and one spare for the next cut.
- The first GPU use of a prepared model uploads its source vertices and edge
  indices in bulk. Static topology remains cached for that prepared revision.
- The current model uploads one bulk edge-color array (16 bytes per source edge),
  112 bytes of projection/ring parameters and 160 bytes of ribbon parameters
  each frame. GPU compute projects the vertices and prepares/compacts ribbons.
  Four indirect draws render the classified ribbon geometry. Production GPU mode
  uploads no CPU-projected vertices or expanded ribbon triangles.
- A cut replaces the topology on the next frame; it does not reveal geometry
  through incremental uploads. The GPU projection ring reseeds for the new
  model, so motion blur cannot connect the old and new wireframes.

## Retained backgrounds and GPU working buffers

The original color fade invalidated the full-resolution retained background on
every interpolated frame: 12 rebuilds at 60 Hz. Each rebuild rasterized a solid
rectangle through Vello, registered its texture, then rasterized the cached image
into the composition background.

Presentation frames now carry `Frame::Solid` with the exact interpolated color.
Desktop, export and the pipeline benchmark clear a dedicated GPU texture when
the color or extent changes. Settled colors reuse it. Vector background caches
remain separate, including across transitions between solid and vector frames.

GPU model working buffers were also destroyed when their model left the scene.
Unlike immutable source geometry, these buffers are large: roughly
`672.25 * edges + 48 * vertices` bytes per model. Working sets now reuse their
buffers across model identities. Capacity grows when a larger model first needs
it; this is not a per-model scratch cache. The renderer retains at most two
working sets and can reuse a single set across instantaneous cuts.

The allocator regression also exercises two simultaneous models as a renderer
stress case, using catalog models with 69,225, 63,331 and 60,277 edges. It verifies
that the third model can reuse the first working set. Pixel comparisons cover
rebinding buffers to smaller vertex/edge counts, history resets and empty frames.
Normal scene cuts publish only the current model.

## Earlier measurements with the edge dissolve

The following measurements were captured **before model transitions changed to
instantaneous cuts**. They document the background optimization under the former
two-model edge dissolve; they are not timings or guarantees for the current cut.

A matched 5120 × 2880 replay with the same executable compared the former vector
background method with the solid clear. Median render-plus-readback times during
the first 12 dissolve frames were:

| Change | Vector background | Solid clear |
| --- | ---: | ---: |
| Colors only | 35.79 ms | 24.68 ms |
| Colors and dissolving model | 45.90 ms | 23.91 ms |

The combined dissolve case retained identical model/style payloads: 70,466 source
edges and 1,127,456 style bytes per two-model frame. Background rebuilds fell from
12 to zero. Results are saved in `target/scene-transition-vector.json` and
`target/scene-transition-solid.json`. These timings include synchronous output
readback, and their peaks remain variable; they do not establish native
presentation cadence or eliminate every possible frame spike.

Validation of that earlier implementation on Apple M4 / Metal passed 697 ordinary
tests, targeted GPU pixel/history/allocation tests and the production audio/focus/
resize integration trial. Its separate 1920 × 1080 native `--profile` smoke achieved
60.0 FPS over 600 measured frames, with 1.65 ms median and 3.69 ms maximum host
redraw work. That bounded synthetic smoke is historical evidence, not a current
5K model-cut benchmark. The commands below exercise the current behavior.

## Reproduction

GPU-only validation passes all 695 ordinary tests and the all-target build check.
Eleven GPU renderer/history checks, the production audio/focus/resize trial,
video export and solid-background pixel checks pass. Developer-overlay previews
cover hidden/shown statistics at desktop, slim and HiDPI sizes. A native
1920 × 1080 smoke achieved 60.0 FPS over 600 frames with statistics hidden by
default; its log is saved in `target/gpu-only-native-profile.log`.

The earlier instant-cut pipeline replay submitted exactly one model on every
measured frame, selected the new model on all 24 post-cut frames, and requested
zero background rebuilds. Its per-frame output is saved in
`target/scene-transition-instant.json`.

The ordinary regressions use actual model navigation and theme interpolation,
verify that only the complete new model is published while colors continue
changing, and reject vector background rebuilds:

```sh
cargo test --release --lib instant_model_swaps_publish_only_the_new_complete_model
cargo test --release --lib color_interpolation_with_instant_model_swaps_avoids_background_rasterization
```

GPU checks require a compute-capable adapter:

```sh
cargo test --release --lib offscreen_solid_backgrounds_match_vector_backdrops_and_preserve_retained_layers -- --ignored
cargo test --release --lib gpu_model_swaps_reuse_two_high_water_working_sets -- --ignored --nocapture
BENGA_TRANSITION_EXTENT=5120x2880 BENGA_TRANSITION_MAX_BACKGROUND_REBUILDS=0 \
  cargo test --release --lib measures_scene_transition_stages -- --ignored --nocapture --test-threads=1
```

The bounded full-pipeline replay compares settled, colors-only, model-only and
combined changes. It writes per-frame timings and source/style-upload counts to
`target/scene-transition-profile.json`. `BENGA_TRANSITION_OUTPUT` selects another
output path. `BENGA_TRANSITION_VECTOR_BACKGROUND=1` restores the old background
method as a comparison control; `BENGA_TRANSITION_AUDIO=1` adds analyzer-driven
motion while keeping the discrete model/theme switch schedule fixed.

The former CPU-generated frozen-geometry benchmarks were retired with the CPU
renderer. The standalone GPU ribbon prototype was also retired after integration.
Model-cut checks and the bounded full-pipeline replay use the production GPU path.
