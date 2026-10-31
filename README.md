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

Models live in [`assets/models/`](assets/models/). The loader welds shared
vertices and extracts boundary and crease edges. Model metadata and POI authoring
are described in the [model specification](assets/models-spec/weapon_visualizer_spec.md).

Licensed under [GPL-2.0-only](LICENSE).
