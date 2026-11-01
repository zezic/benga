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

Audio capture:

- **macOS 14.6+:** allow system audio recording when prompted.
- **Windows:** captures the default audio output. Restart Benga after changing
  the output device or its sample rate.
- **Linux:** captures the default recording source. Route Benga to your desktop
  output's monitor in your audio settings (for example, the Recording tab in
  `pavucontrol`). PulseAudio and PipeWire's PulseAudio service are supported,
  with ALSA as a fallback.

## Controls

Benga starts in presentation mode. Move the mouse to reveal the controls; hover
any control for a hint. They fade after 5 seconds of inactivity.

- Browse models and themes, lock the current model, or enable automatic colors.
- Adjust motion blur, shutter exposure, line width, zoom, and the FPS limit.
- Adjust analyzer boost and text opacity, or manage disliked themes.
- Double-click the background to toggle fullscreen; `Esc` leaves fullscreen.
- Use **Save as Default** to set the startup theme.

Music drives the model's motion, part highlights, and close-ups of points of
interest. Energy drops change the model unless Model Lock is enabled.

Presentation mode includes two live audio instruments in the side columns.
The left instrument shows the level-normalized spectrum against its slow
reference, spectral trend changes, mix energy, bass attacks, and the armed gap
state. The right instrument shows regional attack candidates,
a joint sharpness–energy heatmap and the envelope used for each decision.
The spectral shape graph also shows **Aggression**: LF is the maximum of the
selected low bands, MF is the median of the selected mid bands, and
`aggression = (LF - MF + 1) / 2` on the graph's −1 to +1 scale.
Whenever the fast LF envelope exceeds the slow one, their positive separation
provides a boost: `max((fast - slow) / fast, 0)`, using the LF measurement floor.
Its strength is multiplied by `clamp(LF - MF + 1, 0, 1)`: no boost when LF is
1.0 or more below MF, half strength at −0.5, and full strength when LF equals
or exceeds MF. This compares the actual envelopes, independently of the
**Bass attack** offset or gap/drop gates. **LF boost** shows the weighted
addition. **LF boost gain** beside
**Warm sound relaxation** scales that addition from 0× (off) through 1×
(the default) to 4×. Spectral attenuation still applies at every gain, and
the gain saves automatically. The displayed value and spring
stiffness follow an envelope with immediate rise and a slow exponential fall
(3-second time constant), so brief bass gaps preserve aggression. It advances
with the audio and resets when capture restarts. Shape aggression, amplified
LF boost and the envelope stay unclamped; the percentage readouts can exceed
100% or fall below 0%. Above 100%, spring stiffness follows aggression directly:
150% means 1.5× and 200% means 2×, with a ceiling of 2× the configured stiffness.
The meter spans 0–200%, marking the 100% baseline and the 2× ceiling.
Larger raw peaks hold this ceiling for longer as the envelope falls.
Drag the colored LF/MF boundary handles to select whole bands. LF defaults to
the lowest four (32–151 Hz), and MF to four bands centered near 590, 870,
1,280 and 1,890 Hz (485 Hz–2.3 kHz). **Relaxation** controls how much warm sound
softens the model flip and POI entry, look-at point, orbit and FOV springs.
At the default 75%, fully warm sound uses 25% of the configured stiffness;
100% aggression uses the configured stiffness. Zero disables the warm-sound
reduction; overdrive above 100% remains active. The strongest relaxation keeps
at least 5% stiffness so springs continue moving. Damping stays at its configured
value during relaxation and scales by the square root of the stiffness multiplier
during overdrive, preserving its damping ratio. The ranges, gain and relaxation
amount save automatically.
The energy-loss graph compares the smoothed mix with its learned reference:
positive dB means quieter, and the solid loss trace must rise above the dashed
requirement. **Mix loss hold** can arm the gap from this loss alone, without a
shape change, when the requirement stays met continuously for its own duration
(default 1.2 seconds). Falling back to or below the required loss resets that
timer. The loss hold is independent of the shape-path and RMS holds, and its
readout sits below the graph. Each historical threshold stays as measured when
the Mix loss slider changes.
The independent **RMS gap** path arms the same flag whenever the analysis-window
RMS stays below **RMS gap below** continuously for **RMS gap hold**. Its defaults
are −40 dBFS and 500 ms. It needs no spectral change, relative energy loss,
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
Drop confirmation uses adjustable **Fast envelope** and **Slow envelope** times
for the weighted low-frequency spectrum, defaulting to 20 ms and 250 ms.
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
Both default to 25%. An attack passes the cloud filter when **energy OR sharpness**
clears its floor, including values above the cloud. The shade shows this union;
energy and sharpness use matching dashed threshold guides across the full plot
width and height, with a numeric sharpness label on the map.
**Cloud bucket support** sets the minimum count per bucket (default 3), pooling
the other axis. Bounds grow and shrink as observations enter or leave the
reference. Dynamic mode starts off. Each axis uses its saved fixed floor until
supported bounds exist; those fallbacks also combine with OR in cloud mode.
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
checkbox; switching it off preserves its chosen value. Controls save automatically.
Revision 3 resets old percussion masks
and input curves while retaining gap/drop, motion, and theme preferences.

For developer controls, hold any modifier key and click **Developer: exit
presentation**. Common shortcuts:

| Key | Action |
| --- | --- |
| `F` | Return to presentation mode |
| `D` | Toggle the audio analyzer and parameter inspector |
| `G` | Toggle the onset/drop timeline |
| `A` / `S` | Previous / next theme |
| `\` | Next model |
| `T` | Toggle model motion blur |
| `C` / `V` | Decrease / increase shutter exposure |
| `Q` / `W` | Previous / next FPS limit |

The developer HUD shows the remaining shortcuts. Click a band or property in
the analyzer to inspect and tune it.

## Settings

Settings save automatically. Browsing themes does not change the saved startup
theme. Config locations:

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
seconds at a 240 FPS limit, then exits. Keep its window visible. It reports FPS,
CPU usage, and frame-stage wall-clock timings; these include display waits and
are not GPU execution timings. Profiling leaves saved settings untouched.

For a headless wireframe and motion-blur preview:

```sh
BENGA_RENDER_PREVIEW_PATH=/tmp/benga-preview.png \
  cargo test renders_wireframe_lines_and_swept_ribbons -- --ignored
```

GPU preview tests require a compute-capable GPU. Other presentation previews
are ignored tests in [`src/visualizer/ui.rs`](src/visualizer/ui.rs).

The audio detectors use independent causal stereo percussion analysis with paired
onset histograms, plus spectrum-shape tracking for gap/drop events. Left and right
stay separate through attack movement and background subtraction; their positive
changes combine afterward, preserving attacks hidden by opposite-side decay. The [detector evaluation guide](docs/detector-evaluation.md) documents
the streaming music-library probe, resolution/gain comparisons, and validation.

Models live in [`assets/models/`](assets/models/). The loader welds shared
vertices and extracts boundary and crease edges. Model metadata and POI authoring
are described in the [model specification](assets/models-spec/weapon_visualizer_spec.md).

Licensed under [GPL-2.0-only](LICENSE).
