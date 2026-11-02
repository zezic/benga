![Benga Logo](/assets/benga-logo.svg)

# Benga

An audio visualizer that animates 3D wireframe models to music playing on your
desktop, with motion blur, color themes, and animated infographics.

## Run

Requires [Rust](https://rustup.rs/) and a GPU with compute support for the
Vello/wgpu renderer. Run from the project root so Benga can find `assets/models/`:

```sh
cargo run --release
```

Desktop playback uses GPU model projection, ribbon preparation and rasterization.
Adaptive quality starts at **1× native resolution**, lowers the model
layer when needed and restores detail toward 1× as headroom returns. Background,
infographics and controls retain native resolution.

Model motion blur uses a fixed two-frame exposure and can be turned off.
Benga requires a compatible GPU with compute support.
See the [GPU integration notes](docs/gpu-ribbon-integration.md) for quality logic,
profiling controls and earlier measurements.

Audio capture:

- **macOS 14.6+:** allow system audio recording when prompted.
- **Windows:** captures the default audio output. Restart Benga after changing
  the output device or its sample rate.
- **Linux:** captures the default recording source. Route Benga to your desktop
  output's monitor in your audio settings (for example, the Recording tab in
  `pavucontrol`). PulseAudio and PipeWire's PulseAudio service are supported,
  with ALSA as a fallback.

## Render an audio file to video

Render the presentation without opening a window or capturing desktop audio:

```sh
cargo run --release -- --render "track.flac" --output "video.mp4" \
  --fps 60 --width 1920 --height 1080 --gain -6
```

Requires [FFmpeg](https://ffmpeg.org/) on `PATH`, with the `libx264` and AAC
encoders, and a GPU with compute support. Any audio format FFmpeg can decode
can be used, including WAV, FLAC, MP3, and Ogg. The first audio track is used.
The result is an H.264 MP4 with the source audio encoded as AAC.

- `--fps`: 10–240, including decimal values such as `29.97`; defaults to 60.
- `--width` / `--height`: positive even pixel dimensions; default to 1920×1080.
- `--gain`: analyzer gain in decibels, from −96 to +96. Overrides the saved
  analyzer gain for this render; the soundtrack volume stays unchanged.
- `--config`: use a particular settings `.ron` file; otherwise use saved
  desktop settings, including theme, detector tuning, zoom, and motion blur.
- `--overwrite`: replace an existing output after the render succeeds.

`--output` is required and must end in `.mp4`. Rendering follows the audio
timeline at the requested FPS and runs as fast as the GPU and encoder allow.
It streams audio and frames rather than loading the whole track into memory.
The presentation includes models, motion blur, and infographics, with controls
and startup screens hidden. Models use GPU rendering at fixed 1×
output resolution. Saved
settings remain unchanged. The last frame
can extend past the audio by less than one frame interval. Visual choices
remain randomized between runs, as in the desktop app.

## Controls

Benga runs in presentation mode. Move the mouse to reveal the controls; hover
any control for a hint. They fade after 5 seconds of inactivity.

- Browse models and themes, lock the current model, or enable automatic colors.
- Adjust motion blur, line width, zoom, and the FPS limit.
- Adjust analyzer boost and text opacity, or manage disliked themes.
- Double-click the background to toggle fullscreen; `Esc` leaves fullscreen.
- Use **Save as Default** to set the startup theme.

Music drives the model's motion, part highlights, and close-ups of points of
interest. Energy drops change the model unless Model Lock is enabled. Model
changes are instant; theme colors still fade smoothly.

Renderer statistics start hidden. Hold a modifier key and click
**Developer: show renderer stats** above the toolbar to display them.
Their visibility is independent of the audio analyzers.

Music also introduces temporary graph compositions into the presentation. These
include live spectral shape, aggression, energy, bass envelopes, onset timelines,
and percussion heatmaps. Attack envelopes update with each newly registered
attack for their instrument. Heatmap groups choose one to three distinct
instruments (kick, snare, hat), with independently varied square, wide, or tall
maps and sizes.
Graph headings use thin text with space above each plot, retaining the original
axis legends. Switching between normal view and POI close-ups can add an initial
composition, or no new graphs. Later musical triggers may add
more graphs. Selector changes, POI target switches, and model changes alone do not
choose a new initial composition. Compositions retain their musical retirement
rules. Up to two compositions can coexist; each has an 18–28-second protected hold,
then waits for another 48–96 onset
events before leaving. Quiet passages have a 75–105-second lifetime limit.
Static plots flicker on and off like neon lamps, staying steady between their
entrance and exit. Static heatmaps keep their cell-based assembly and dissolve.
Static formations survive POI switches when their space stays clear. In POI
close-ups, additional graphs follow model vertices alongside the floating
cards, using the same spring motion, anchor markers, and leader lines. On
defocus, floating cards coast and fade for 0.5 seconds; vertex graphs retain
their 1.1-second coasting fade. Attached graphs fade as a whole while moving
freely over the scene. Static formations make room for cards and controls. Maps,
envelopes, timelines, and other plots can assemble into compact formations whose rectangles follow
shared rows, columns, and gapped grid spans, including asymmetric layouts. Some compositions
pair two tight formations with a wider gap between them. Heatmaps retain their
square, wide, or tall proportions.
Aggression uses a slim horizontal meter in a wide, shallow panel.

The detailed analyzers start hidden on every launch. Hold any modifier key to
reveal **Developer: show analyzers** above the toolbar; click it to show the two
live audio instruments and their tuning controls. The same button hides them.
Scene graph compositions pause while the detailed analyzers are open.
The left instrument shows the level-normalized spectrum against its slow
reference, spectral trend changes, mix energy, bass attacks, and the armed gap
state. The right instrument shows regional attack candidates,
a joint sharpness–energy heatmap and the envelope used for each decision.
The spectral shape graph also shows **Aggression**: LF is the maximum of the
selected low bands, MF is the median of the selected mid bands, and
`aggression = (LF - MF + 1) / 2` on the graph's −1 to +1 scale.
**Spectrum amount** scales this contribution from 0–100% before LF boost is
added; it defaults to 66%. At 0%, only the LF envelope boost contributes.
Whenever the fast LF envelope exceeds the slow one, their positive separation
provides a boost: `max((fast - slow) / fast, 0)`, using the LF measurement floor.
Its strength is multiplied by `clamp(LF - MF + 1, 0, 1)`: no boost when LF is
1.0 or more below MF, half strength at −0.5, and full strength when LF equals
or exceeds MF. This compares the actual envelopes, independently of the
**Bass attack** offset or gap/drop gates. **LF boost** shows the weighted
addition. **LF boost gain** scales that addition from 0× (off) to 4×
(the default). Spectral attenuation still uses the measured LF/MF levels,
independently of Spectrum amount. Both controls save automatically. The
displayed value and spring stiffness follow an envelope with immediate rise
and an adjustable exponential fall. **Aggression falloff** sets its time
constant from 0–10 seconds, defaulting to 0.5 seconds. Larger values bridge
longer bass gaps; zero makes falling aggression immediate. Live adjustments
preserve the current envelope value. It advances with the audio and resets
when capture restarts. Shape aggression, amplified
LF boost and the envelope stay unclamped; the percentage readouts can exceed
100% or fall below 0%. Above 100%, POI camera spring stiffness follows aggression
directly: 150% means 1.5× and 200% means 2×, with a ceiling of 2× the configured stiffness.
The 180° model flip spring stays at its configured stiffness above 100%.
The meter spans 0–200%, marking the 100% baseline and the 2× ceiling.
Larger raw peaks hold this ceiling for longer as the envelope falls.
Drag the colored LF/MF boundary handles to select whole bands. LF defaults to
the lowest two (32–70 Hz), and MF to six bands spanning 151 Hz–1.56 kHz.
**Relaxation** controls how much warm sound softens the model flip and POI entry,
look-at point, orbit and FOV springs.
At the default 100%, fully warm sound uses the 5% minimum stiffness;
100% aggression uses the configured stiffness. Zero disables the warm-sound
reduction; POI camera overdrive above 100% remains active. The strongest relaxation
keeps at least 5% stiffness so springs continue moving. Damping stays at its configured
value for the model flip and during camera relaxation. Camera overdrive scales
damping by the square root of the stiffness multiplier, preserving its damping
ratio. The ranges, gain and relaxation amount save automatically.
The energy-loss graph compares the smoothed mix with its learned reference:
positive dB means quieter, and the solid loss trace must rise above the dashed
requirement. **Mix loss hold** can arm the gap from this loss alone, without a
shape change, when the requirement stays met continuously for its own duration
(default about 0.91 seconds). Falling back to or below the required loss resets
that timer. The loss hold is independent of the shape-path and RMS holds, and its
readout sits below the graph. Each historical threshold stays as measured when
the Mix loss slider changes.
The independent **RMS gap** path arms the same flag whenever the analysis-window
RMS stays below **RMS gap below** continuously for **RMS gap hold**. Its defaults
are −24 dBFS and 500 ms. It needs no spectral change, relative energy loss,
reference warmup, or relative hold; reaching or exceeding the limit resets its
own timer. The level graph shows RMS solid, the captured gap limit dashed, and
the learned reference dotted, with the RMS hold shown underneath. Any of the
three paths—shape overlap, loss hold, or RMS hold—can arm the gap; **Drop interval**
still limits the subsequent drop trigger.
The shape/trend/area graph uses multiples of each recorded requirement: 1× is
the threshold and peaks are capped at 4×. Its time axis aligns with energy loss
and the relative-condition timeline. Filled rows show when each relative check
passes; **Overlap** includes that path's initial learning period and silence
handling. **Rel hold** rises only while those conditions stay true and resets
when they fail, independently of the loss and RMS holds above. A confirmed gap stays armed until
a drop consumes it; elapsed time and level recovery do not disarm it.
This makes shape changes that precede energy loss visible. Gap controls are
grouped beside their reference, shape, energy,
timing, and bass-return graphs.
Snares detected while the gap is armed update detector readouts and hit counters
immediately, but their model action waits 50 ms on the audio clock. A drop within
that window cancels the held snare action. Otherwise the action fires once after
50 ms, even if the gap remains armed. Snares detected without an armed gap act immediately.
Drop confirmation uses adjustable **Fast envelope** and **Slow envelope** times
for the weighted low-frequency spectrum, defaulting to about 50 ms and 501 ms.
**Bass attack** sets how much of the fast envelope must
rise above the slow envelope: `(fast - slow) / fast`. The LF graph shows the fast
envelope as a solid line, the slow envelope dotted, and the required fast level
dashed, all in dB. A confirmed gap, that envelope rise, the configured **Drop RMS**
floor, and **Drop interval** are the drop checks. This path does not use the
percussion kick detector's classification or spectral-flux needle scores.
A fresh LF rise must begin after the gap confirms. It stays active while the
fast envelope clears its requirement, allowing the RMS gate to pass as the
return gets louder. Both envelopes share the graph's −80 dB measurement floor
so tiny numerical LF leakage cannot create a full-strength relative onset.
Heatmap intensity shows the current paired reference population. Recent additions
are overlaid as markers: filled means an original trigger, hollow means learned
without triggering; older additions outside the reference are dimmed. The marker
count defaults to 32 and can be changed with the heatmap’s −/+ buttons. Cutoff
lines and the shaded feature-pass region explain the sharpness/energy checks;
output prominence, mix RMS and spacing still apply separately.
The dashed **cloud** outline tracks supported energy and sharpness bounds,
ignoring sparse tails. Each cloud percentage has its own checkbox: enable
**Cloud energy top area**, **Cloud sharpness top area**, or both. Energy top X%
selects the upper part of the cloud's height
on the dB map; **Cloud sharpness top area** selects its sharpest Y% by width.
The default energy/sharpness areas are 20.6%/13.1% for kick, 13%/9.8% for
snare, and 25%/25% for hat. An attack passes the cloud filter when
**energy OR sharpness** clears its floor, including values above the cloud. The shade shows this union;
energy and sharpness use matching dashed threshold guides across the full plot
width and height, with a numeric sharpness label on the map.
**Cloud bucket support** sets the minimum count per bucket (default 3), pooling
the other axis. Bounds grow and shrink as observations enter or leave the
reference. Both cloud branches start enabled. Each axis uses its saved fixed
floor until supported bounds exist; those fallbacks also combine with OR in cloud mode.
The cloud checkboxes work directly, independently of the fixed **Trigger energy
floor** and **Sharpness floor** switches. With both cloud checkboxes off, those
fixed filters apply as before. Other enabled filters, including the independent
percentiles, can still reject an attack.
Click its timeline to switch kick, snare, and hat. Previous/next event buttons
inspect saved decisions with their original reference; **Live** shows the newer
population. Ranks describe recent regional onsets, not confidence in instrument
identity. See the [feature contract](docs/percussion-feature-contract.md).

Click the weighted spectrum to inspect actual FFT bins. Each bin has an
attenuation fader; arrows page through bins and **Reset** restores unity for the
selected channel. The instruments keep fixed viewport anchors; infographics can
overlap them. Gap/drop sliders sit to the right of its graphs; percussion
controls sit to the left. All controls are visible in pipeline order: choose
frequencies, find and measure attacks, filter inputs, compare recent history,
and filter triggers. Hover any
control for a plain-language explanation. Each input/trigger filter has an on/off
checkbox; switching it off preserves its chosen value. Factory defaults enable
input energy, attack level, and output spacing on all channels, plus mix RMS on
kick and snare. The other optional filters start off. Controls save automatically.
Revision 3 resets old percussion masks
and input curves while retaining gap/drop, motion, and theme preferences.

Press `F1` to open help. The presentation toolbar provides the visual controls;
the former developer mode and its band/property inspector are removed.

## Settings

Settings save automatically. Factory settings start with Aura, automatic colors,
and alternating dark/light contrast, with Gruvbox Dark and Nord Light remembered
as the manual choices. The bundled theme exclusions and audio/visual tuning come
from the [seed configuration](tests/fixtures/factory-defaults.ron); first-run
onboarding remains enabled. Existing saved
settings keep priority. Browsing themes does not change the saved startup theme.
Config locations:

- macOS: `~/Library/Application Support/com.zezic.benga/benga-config.ron`
- Linux: `$XDG_CONFIG_HOME/benga/benga-config.ron`, normally
  `~/.config/benga/benga-config.ron`
- Windows: `%APPDATA%\zezic\benga\config\benga-config.ron`

## Development

```sh
cargo test
cargo run --release -- --profile
```

The profiler uses synthetic audio, warms up for one second, measures for ten
seconds at a 240 FPS limit, then exits. Keep its window visible. It uses
the GPU renderer and reports
FPS, CPU usage, host frame-stage timings and asynchronous GPU model/queue spans.
Statistics start hidden. Set `BENGA_PROFILE_PERFORMANCE_OVERLAY=1` to include
the performance graphs during a profiling run.
Host timings include display waits; GPU queue spans can include CPU submission
gaps and are not busy-GPU utilization counters. Profiling leaves saved settings
untouched. The [integration notes](docs/gpu-ribbon-integration.md) describe target
FPS, native dimensions, fixed downsize and trace controls.

For scene-change stutters, the [transition performance notes](docs/scene-transition-performance.md)
explain model/GPU buffer lifetimes and the bounded colors-only, model-only and
combined transition replay.

For a headless wireframe and motion-blur preview:

```sh
BENGA_RENDER_PREVIEW_PATH=/tmp/benga-preview.png \
  cargo test renders_gpu_wireframe_and_swept_motion -- --ignored
```

GPU preview tests require a compute-capable GPU. Other presentation previews
are ignored tests in [`src/visualizer/ui.rs`](src/visualizer/ui.rs).

The audio detectors use independent causal stereo percussion analysis with paired
onset histograms, plus spectrum-shape tracking for gap/drop events. Left and right
stay separate through attack movement and background subtraction; their positive
changes combine afterward, preserving attacks hidden by opposite-side decay. The [detector evaluation guide](docs/detector-evaluation.md) documents
the streaming music-library probe, resolution/gain comparisons, and validation.

Models live in [`assets/models/`](assets/models/). Before the first animation
update, the loader welds vertices, extracts boundary and crease edges, and prepares
every model's wireframe, slide mask, POI bindings, and magazine preview.
Switching reuses this shared data, two animation-state slots, reserved edge-color
storage and GPU working buffers. Projection and motion history stay on the GPU;
playback allocates no CPU model geometry or pose-history arrays. Changing the crease threshold
refreshes all models immediately; animated projection, randomized captions, and
viewport-dependent text layout still run during playback. Model metadata and POI
authoring are described in the [model specification](assets/models-spec/weapon_visualizer_spec.md).

Licensed under [GPL-2.0-only](LICENSE).
