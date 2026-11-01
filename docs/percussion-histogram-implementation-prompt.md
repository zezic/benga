# Replace percussion detectors with adaptive onset histograms

Implement a complete replacement for Benga's kick, snare, and hi-hat detectors.
Use fast causal regional onset detection, measure each candidate's sharpness and
energy, and classify candidates against recent per-instrument distributions.
Deliver working detectors, live tuning and diagnostics, and reproducible tests
and music replays. This is a replacement of the existing percussion classifier,
not an additional gate around it.

Repository: `/Users/ukolov.sergey8/Work/Benga/benga`.
Music archive: `/Users/ukolov.sergey8/Music/Spotify Dump`.
The archive is mostly electronic music, without annotated instrument events.
Primary replay gain is −6 dB, approximating desktop capture attenuation. Also
test unattenuated and quieter signals. Read audio in place; keep large traces
and decoded audio outside the repository.

## Scope and integration

Inspect the current code and working-tree instructions before editing. Preserve
unrelated work, including existing untracked files. Relevant seams are:

- `src/visualizer/spectrum.rs`: FFT, stereo power, regional measurements and bin gains.
- `src/visualizer/transients.rs`: the percussion decision path to replace.
- `src/visualizer/detection.rs`: settings, migration, `OnsetBank`, events and diagnostics.
- `src/visualizer.rs`: sample consumption, parameter mapping and action integration.
- `src/visualizer/presentation/analysis.rs` and `ui.rs`: live tuning and diagnostics.
- `examples/detector_probe.rs` and `tools/evaluate_detectors.py`: production streaming replays.
- `docs/detector-evaluation.md`: replay instructions and historical measurements.

Preserve gap/drop behavior, visualizer actions, motion settings, themes and
capture continuity. The existing FFT also supplies the arrangement detector:
shrinking it globally changes gap/drop spectral shape and energy measurements.
Use an independent fast percussion branch, or prove equivalent arrangement
inputs and timing. Keeping `arrangement.rs` unchanged alone is not sufficient.
Keep accepted-hit strength bounded and normalized for the existing visualizer;
raw attack energy and percentile rank are separate diagnostic quantities.

Remove the old active percussion decision rules: EMA/deviation score thresholds,
the 28 ms kick-body peak rule, fixed two-hop instrument confirmation, regional
instrument-dominance vetoes and old choke logic. Causal envelope/noise followers
may support candidate extraction; the final classifier must use the new event
features and percentile eligibility. Do not retain an old classifier fallback.
Do not reintroduce temporal spectrum templates.

## First establish a measurable feature contract

Before integrating a new classifier, build a small causal fixture runner and
write down the exact candidate boundaries, equations, units, windows, smoothing,
normalization and decision availability. Run the feature tests below. Adjust the
feature definitions from that evidence before tuning percentile thresholds.
Proceed through implementation without requiring a separate design approval.

The equations and numerical windows below are starting hypotheses, not validated
defaults. Keep a clear rationale for any replacement definition.

### Frequency regions and analysis resolution

Start with these instrument masks, configurable in physical Hz:

| Instrument | Main region | Additional weighting |
| --- | --- | --- |
| Kick | 60–200 Hz | Zero outside the configured region |
| Snare | 2,000–5,000 Hz | Fall off smoothly to zero at 10,000 Hz |
| Hat | 5,000–16,000 Hz | Zero outside the configured region |

For an explicit initial snare **power** mask, use unit weight from 2–5 kHz and
`0.5 * (1 + cos(pi * (f - 5000) / 5000))` from 5–10 kHz. Distinguish a power
weight from an amplitude response throughout the implementation and UI. Allow
independent simultaneous instrument triggers; overlapping regions are expected.
At low sample rates, truncate unavailable frequencies at Nyquist instead of
remapping a high-frequency region into a lower one. If a mask has no usable
support, mark the channel unavailable and produce neither observations nor
triggers; age any old reference normally. Cover this explicitly at 8/16 kHz.

Compare 256- and 512-point percussion FFTs, with a hop short enough to resolve
the proposed temporal features. At 48 kHz, 128/256/512 samples span
2.67/5.33/10.67 ms, with bin spacings 375/187.5/93.75 Hz. A 60 Hz period is
16.67 ms. Very short full-rate FFTs cannot provide trustworthy 60–200 Hz
selectivity just because fractional weights or zero padding are available.

Require phase, leakage, sweep and sustained-tone tests before accepting a kick
front end. If necessary, use a dedicated causal kick filter/envelope or a
separate low-frequency analysis resolution while keeping snare/hat fast. A
decimated low-frequency FFT can reduce computation, but its physical window and
filter delay still count. Report both computation cost and actual latency.

Use stereo power, preserving anti-phase material. Calibrate selected regional
power in mean-square units. For an unnormalized stereo DFT and time window `h`:

```text
P_region[n] = sum_k c[k] * W_region[k] * g[k]^2
                   * (abs(XL[n,k])^2 + abs(XR[n,k])^2) / 2
              / (N * sum_j h[j]^2)

c[k] = 2 for interior positive-frequency bins; 1 at DC and Nyquist
```

This is window-weighted mean-square power; changing window duration still
changes transient blur. Test stationary power and known energy integrals across
resolutions, not identical transient features from different physical windows.
Integrate mask weights over bin support where appropriate. A user amplitude gain
`g` contributes `g²` to power. Use hop duration when integrating overlapping FFT
frames; summing whole-window energies repeatedly double counts time. Validate
normalization against known signals and sample-rate/window changes.

### What counts as an onset

An onset is one bounded new attack in a selected frequency region. A positive
spectral-flux frame, an amplitude threshold crossing, and a final instrument
trigger are different things.

Use a permissive candidate extractor based on fresh spectral/energy growth,
local prominence and a causal regional envelope. It should discover credible
smooth attacks as well as sharp ones, so the histograms contain a comparison
population. Candidate discovery must not depend on the final percentile settings.

Define and test an event state machine: idle, collecting, finalizing and
rearming, with a bounded pre-roll and any bounded pending-event queue needed.
Establish attack start before an absolute admission gate opens; otherwise a slow
note can cross that gate late and appear artificially sharp. Freeze a pre-onset
background estimate at that start. Establish explicit rules for merging lobes,
splitting a second genuine attack and rearming over a sustained bed.

Start by evaluating a fixed 20–30 ms feature horizon, with a 24 ms initial
experiment. Specify the prospective portion, pre-roll, filter/window support and
actual availability of every feature. If a second attack overlaps this horizon,
define and test the measurement policy: do not silently lose the second event,
double-insert the first, or treat a neighboring attack as isolated source energy.
Keep onset time, feature completion and emission time separately.

### Sharpness measures concentration of growth

Use a lightly smoothed regional RMS/amplitude envelope `A`, with smoothing and
any background subtraction explicitly defined. An initial sharpness feature is:

```text
d[i] = max(0, A[i] - A[i-1])
G = sum of d[i] over the fixed feature horizon T
S = maximum sum of d[i] in any delta-ms interval inside T / G
```

Try `delta = 4–5 ms` and `T = 20–30 ms`. The hop must resolve `delta`; use actual
time intervals/interpolation rather than assuming a fixed number of frames.
Exclude negligible-growth events instead of turning epsilon-level noise into
`S = 1`. High `S` means growth was concentrated in a short interval; a broad ramp
distributes growth. Above admission floors, scaling the same waveform should
leave `S` approximately unchanged.

This is **observed attack concentration**, not spectral brightness, raw maximum
derivative, loudness, or an unknowable full rise time. Do not claim a 10–90% rise
duration for a note still rising when the observation window ends. A completed
rise-time measurement can be a secondary diagnostic, with censoring explicit.
Display peak holds must never feed feature extraction.

### Energy measures the early attack

Measure both absolute regional energy and estimated new-attack energy over an
explicit fixed window:

```text
E_absolute = sum_i P_region[i] * dt[i]
E_attack   = sum_i max(P_region[i] - frozen_background_power, 0) * dt[i]
```

Start with `E_attack` as the histogram feature and use absolute energy plus
prominence for sanity checks. Store linear energy in normalized full-scale²
seconds. A display value such as `10*log10(E_attack / T)` is a dBFS-equivalent
early attack level, not the waveform's peak amplitude.

Avoid whole-note/tail integration, a variable duration masquerading as comparable
energy, or selecting the loudest sample after seeing the future. Test overlapping
sounds: a dying note can offset a new attack's net regional increase. If this
breaks regional background subtraction, compare a frozen per-bin positive-excess
estimate and document its bias and pitch-motion safeguards. Neither estimate is
source-separated instrument energy. Settle this choice during the feature spike.

Validate that amplitude scaling multiplies energy by gain squared. Equal-energy
fast/slow fixtures must equalize measured energy over the same feature horizon
and front end, not just whole-note energy. Separate these dimensions instead of
letting a louder copy score as sharper by construction. Attacks outlasting the
horizon can saturate or have different prefix concentration: test curved ramps
and mark incomplete rises. Do not promise strict full-attack-duration ordering
from identical early prefixes.

## Learn six distributions and classify events

Maintain a paired record of sharpness and energy for each admitted event.
There are three instrument populations and **two histograms per instrument**:
sharpness and energy. The UI can show one selected pair at a time.

Use this order for each completed candidate:

1. Expire reference observations using consumed audio time.
2. Apply event-quality and histogram-admission gates.
3. Query both ranks/cutoffs against the preceding admitted population.
4. Decide eligibility and any output/refractory rule.
5. Insert an admitted observation once, including percentile-rejected candidates.
6. Publish an immutable decision record and, if eligible, an instrument event.

Use causal feature-completion audio time for reference aging and deterministic
`(completion_time, candidate_id)` ordering within each instrument. Completion
means the moment features are actually available, not a backdated support
timestamp. Run idle expiry against current audio time as well.

The current candidate cannot raise its own cutoff. Both feature histograms must
describe the same paired events and expire observations together. Learn valid
candidates even when they fail percentile, output RMS or output refractory
checks; otherwise the classifier selectively trains on its own decisions.

### Admission and eligibility are separate

Reject numerical noise, invalid samples, startup artifacts, duplicate lobes,
negligible selected-region energy and negligible actual new attack growth before
learning. A loud sustained bed must not validate a tiny new crackle. Assess
cumulative selected-area energy and prominence, with spectral coherence or
meaningful bandwidth if evidence supports it. Avoid arbitrary minimum FFT-bin
counts that change their physical meaning with FFT resolution.

Do not make histogram admission a disguised sharp-percussion classifier:
credible smooth notes need to contribute low-sharpness observations. Keep any
final minimum physical sharpness and final mix-energy gate distinct and visible.
A population consisting only of smooth notes or quiet noise must not become
drums merely because it has an upper percentile. Retain and tune absolute
sanity/output floors, especially for material around −30 dBFS RMS.

Use separate sharpness-percentile and energy-percentile controls; initially
require both. Higher percentile settings must unambiguously be stricter.
Do not assume 80th percentile on both features means 80% confidence: independent
marginals would pass roughly 4% jointly, and these features will be correlated.
Ranks express position in recent regional onsets, not probability of instrument
identity. Sharp bass plucks and struck chords can still resemble drums.

### Ties and warmup must work

For sorted reference values `x[0..N)` and `p in (0,1]`, use the empirical quantile
`Qp = x[ceil(p*N)-1]`; accept `feature >= Qp - tolerance`. Tolerances must be
feature-specific and justified by measurement resolution. At `p = 0`, disable
that percentile condition after absolute gates. Test 0/100%, N=1/2 and monotonic
strictness. Report the tie interval
`[count(reference < value)/N, count(reference <= value)/N]` where useful.
Expose tolerance-based acceptance in the decision reason.

When the reference consists of identical admitted drum hits, matching hits must
pass the percentile conditions, assuming their absolute gates and spacing pass.
Midrank alone gives equal events rank 50%, rejecting a regular train under an
80% gate. Display flat or tied populations clearly, not as “100% confidence.”
Histogram plotting bins must
not create artificial ties. Also test small real variations: high percentiles
can thin genuine drum trains, so defaults need evidence rather than a forced
target acceptance rate.

Empty or insufficient references produce **undefined ranks**. Bootstrap using
explicit absolute quality/energy/sharpness rules until a minimum sample count
is available; label those triggers provisional. Do not mute the opening of music
until enough snares arrive, and do not invent percentiles for an empty reference.
Silent or event-free time must age references into the documented warmup state.
Bootstrap bypasses only unavailable percentile comparisons; admission and
absolute output-quality gates remain the same before and after warmup.

### Reference aging

Begin with a bounded rolling deque of paired observations and a configurable
audio-time window; evaluate 10–20 seconds as initial memory. Keep exact feature
values for reference decisions and binned counts for plotting. Sorted feature
arrays or another bounded order-statistics structure can provide exact cutoffs
without sorting unbounded history on each frame.

Time-based expiry is primary: a last-N-events buffer adapts much faster for dense
hats than sparse snares. Bound capacity for supported event rates and memory
settings; expose effective retained duration if capacity truncates the window.
Run expiry even when no new candidate arrives. If exponential weighting proves
better, define half-life, finite maximum age, total mass and effective sample
count and use weighted CDFs/quantiles consistently. A large effective count alone
does not make an old reference fresh.

Changing percentile cutoffs or display histogram bins must not erase observations.
Changing masks, bin gains, FFT/hop, feature windows or feature definitions creates
a new feature-population epoch: cancel affected pending events and reset/rebuild
only affected references. Handle known preamp changes explicitly; do not mix
incompatible energy scales or manufacture an attack from a setting change.
Persist controls, not song-specific learned histories, unless explicitly justified.

## Replace the tuning and observation UI

Expose the parameters that actually drive this implementation, with units and
effects: frequency masks/taper, analysis timing, candidate merge/rearm, envelope
and feature windows, admission energy/prominence gates, final sanity gates, both
percentile cutoffs, reference lifetime/count and output interval. Group primary
and advanced controls so “all controls” does not become an unusable slider wall.
Remove obsolete active EMA/deviation “Sensitivity” controls and their explanation
graphs. Do not silently relabel an old knob with new percentile semantics.

For each selected instrument, show:

- Its weighted spectrum and actual available frequency resolution.
- The regional envelope, background, attack start and feature window, making
  sharpness and energy inspectable at the event level.
- Sharpness and energy histograms with physical units, reference count/age,
  percentile cutoff lines, candidate markers and warmup state.
- A candidate timeline distinguishing admitted/rejected candidates and eligible
  triggers. Show the latest decision's pre-insertion ranks and rejection reason.

Add a compact paired-feature scatter if it materially clarifies the two-cutoff
decision. Record one immutable event snapshot per candidate so display sampling
cannot miss it or recompute old decisions from a newer reference. Bounded event
records must carry sample-time evidence into the slower display-rate UI.
Inspecting a historical event must use its saved pre-insertion reference and
cutoffs. Store bounded binned snapshots/axis metadata or a reconstructible
reference version; clearly distinguish this from the newer live distribution.

Preserve established presentation behavior: fixed viewport anchors, one channel
selected by clicking the percussion timeline, gap/drop controls on the right of
their graphs and percussion controls on the left. Infographics may overlap
graphs. Interactive controls/bin faders belong to the **normal-alpha top UI
scene**, below toolbar/popovers; the main presentation's additive/multiplicative
composition can wash them out. Preserve opacity-zero input cancellation, cursor,
drag/release/focus handling, paging, reset and autosaving. No numbered heading
prefixes or rapidly changing A/S number tables.

Reuse per-bin input attenuation where it remains honest at the new resolution.
The new percussion revision should start with the requested masks and neutral
input curves; make any detector-specific reset/migration visible. Preserve
unrelated gap/drop, motion and theme preferences. Use strict versioned settings
migration, validating malformed legacy input before conversion. Missing new
fields need safe defaults. Remove the old active classifier/state/control path;
retain legacy parsing only where needed to load existing configurations.

## Validation and completion

Before replacement, freeze the current executable/configuration for comparison.
Reuse the production streaming probe; extend its event telemetry for candidate
boundaries, features, admission, pre-insertion ranks/cutoffs, reference population,
provisional/eligible state and reason. Measure **emission time**, not just FFT
length or a backdated onset timestamp. Keep everything causal and sample-clock
driven, with `f64` absolute timing, no per-hop allocation or dependence on FPS.
Include FFT startup-fill and filter delays in the sample-time accounting. Set an
initial signal-to-emission target of at most 50 ms for canonical audible in-band
percussion across tested phases/sample rates; report distributions and justified
exceptions rather than hiding delay in backdated timestamps. Capture buffering
and the next visual frame are separate latency components.

Required controlled tests include:

- Independent attack-duration, amplitude, frequency, decay, phase and background
  variations; equal-energy fast/slow attacks and gain-scaled copies.
- Sustained 60/80/120/180 Hz tones at varied phases, pitch sweeps, vibrato,
  tremolo, piano/plucked bass, narrow clicks and steady broadband noise.
- One observation per attack, no tail duplicates, repeated equal hits, closely
  spaced hits, overlapping attacks and meaningful warmup behavior.
- A slow attack initially rejected by admission must not restart as a fresh,
  artificially sharp candidate when it later becomes loud.
- Self-inclusion prevention, rejected-candidate learning, ties, empty/flat
  references, time expiry during silence, memory/capacity bounds and cutoffs.
- Quiet junk both alone and over a loud bed; a smooth-only population must not
  self-promote into percussion merely by percentile ranking.
- Stereo/anti-phase behavior, 44.1/48/96 kHz, callback partitions, stream resets,
  long sessions, settings changes and independent simultaneous instruments.
- Exact agreement between UI event snapshots/histograms and the reference used
  for each decision, plus persistence, input interactions and native GPU previews.
- Gap/drop input/cadence equivalence when adding the fast percussion branch.

Replay diverse complete archive tracks at 0/−6/−18/−30 dB, plus continuous
playlists. Split calibration and held-out tracks before coefficient tuning. Show
candidate, admission and eligibility counts separately; inspect false chatter,
starvation, phase sensitivity, warmup and reaction to section/loudness changes.
Use deterministic fixtures for accuracy/latency assertions. Unannotated library
counts and visually plausible BPM rates are not precision/recall or instrument
ground truth. Do not tune solely to match a desired trigger rate.

Completion requires the replacement pipeline to be active, obsolete active
controls/code removed, definitions/units and reference semantics documented,
controlled tests and required project checks passing, native UI previews reviewed,
and reproducible calibration/held-out results with real emission delays and CPU
cost. Report concrete remaining ambiguities and the chosen tradeoffs; do not
claim human-level instrument recognition from percentile ranks.

## Primary references

These establish useful distinctions and candidate-extraction techniques, not
validation of the proposed histogram classifier or its starting feature formula:

- [AudioLabs onset detection](https://www.audiolabs-erlangen.de/resources/MIR/FMP/C6/C6S1_OnsetDetection.html): distinguishes an onset instant from its attack/transient and outlines novelty plus peak picking.
- [AudioLabs spectral novelty](https://www.audiolabs-erlangen.de/resources/MIR/FMP/C6/C6S1_NoveltySpectral.html): spectral change can reveal attacks in mixtures; compression can also emphasize noise.
- [Böck and Widmer, Maximum Filter Vibrato Suppression for Onset Detection](https://www.dafx.de/paper-archive/2013/papers/09.dafx2013_submission_12.pdf): a causal neighboring-frequency maximum filter can suppress pitch-motion false onsets. Specify its width in Hz when comparing FFT resolutions.
