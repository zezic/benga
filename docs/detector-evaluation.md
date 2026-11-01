# Streaming detector evaluation

`examples/detector_probe.rs` reads interleaved stereo `f32le` PCM from stdin and
runs the same `Visualizer::consume_buf` pipeline used by desktop capture. It does
not load a model, open an audio device, or render frames. Analysis advances only
as samples arrive, so the probe cannot use future samples. Each run starts with
fresh detector history and default settings. Learned histories are never saved.
The exact [feature contract](percussion-feature-contract.md) defines units,
state transitions, overlap policy, and the distinction between measured regional
onsets and instrument recognition.

Build and analyze one track:

```sh
cargo build --release --example detector_probe
ffmpeg -nostdin -v error -i '/path/to/track.ogg' -f f32le -ac 2 -ar 48000 pipe:1 \
  | target/release/examples/detector_probe --gain-db -6 --label track \
      --trace /tmp/benga-track.jsonl > /tmp/benga-track-summary.json
```

The probe applies gain inside the production pipeline. `-6` dB approximates the
desktop capture attenuation described for this task. `--block-frames` exercises
different capture callback sizes; percussion events use immutable sample-clock decision records at every callback
size. Drop timestamps retain callback precision. Candidate traces include start,
completion, emission, energy, concentration, admission, original cutoffs, tie
ranks, warmup, rejection reason, and saved pre-insertion histograms. `--sample-rate`
must match the decoder output.

Evaluate a reproducible library sample, complete tracks, at the capture gain:

```sh
python3 tools/evaluate_detectors.py \
  --library '/Users/ukolov.sergey8/Music/Spotify Dump' \
  --count 24 --seed 20261031 --gains -6 \
  --output /tmp/benga-detectors-24.json
```

Exercise levels and sample rates:

```sh
python3 tools/evaluate_detectors.py --count 24 \
  --gains 0 -6 -18 -30 --sample-rates 44100 48000 \
  --output /tmp/benga-detectors-matrix.json
```

`--match` selects filename substrings instead of random sampling; it can be
repeated. `--count 2000` selects the entire supplied library. `--seconds 90`
limits decoder duration for quick iteration. `--traces /tmp/benga-traces` saves
per-hop diagnostic JSON Lines. Decoder failures are recorded separately and
cause a nonzero exit status. Individual-track runs exclude failed or empty files
from detector results.

Compare the independent fast percussion FFTs on the same stream (the original
`--fft-sizes` flag changes arrangement analysis only):

```sh
python3 tools/evaluate_detectors.py --count 12 --percussion-fft-sizes 256 512 \
  --output /tmp/benga-detectors-fft.json
```

Each invocation snapshots the probe executable and records its SHA-256 digest,
so a concurrent build cannot change algorithms midway through a corpus replay.

Check a continuous playlist with no detector reset at file boundaries:

```sh
python3 tools/evaluate_detectors.py --count 25 --continuous --workers 1 \
  --output /tmp/benga-detectors-playlist.json
```

This decodes each selected file to the same sample format and feeds one
long-lived production pipeline. The report records sample-derived segment
boundaries and per-segment event counts. It adds no silence, crossfade, or
lookahead between files. Continuous mode records decoder failures and returns a
nonzero status after completing the remaining files. Empty failed files add no
samples. If decoding fails after producing samples, that segment is marked
invalid and the report flags partial failed input: samples already consumed by a
causal detector cannot be removed retrospectively.

The JSON report contains event timestamps, counts, rates, inter-event interval
quantiles, interval variation, short-interval counts, event RMS minima, shape and
trend quantiles, energy-gate fractions, analysis-hop count, and elapsed analysis
time. The baseline and replacement can be compared on
the exact same filenames and gain. Audio stays in the user's music directory;
reports contain metadata and analysis values only.

## Active histogram detector (revision 3)

Percussion runs on its own sample clock and never changes the arrangement FFT's
window, startup, inputs, or cadence. Mean-square stereo power preserves
anti-phase signals. Kick uses 60–200 Hz; snare uses a unit power mask at 2–5 kHz
and a cosine power taper to zero at 10 kHz; hat uses 5–16 kHz. Unavailable support
above Nyquist is truncated; a region with no support cannot learn or trigger.
Faders apply amplitude gain squared to selected power. The transform's real
frequency resolution and physical time support appear beside the weighted bins.

A causal state machine starts attacks before admission floors open, freezes the
background, and measures positive amplitude-growth concentration and fixed-window
absolute/background-excess energy. Overlapping feature horizons are explicitly
marked ambiguous and excluded from learning/output; their immutable candidate
records remain visible. This conservative regional estimate can miss a new
attack offset by a dying sound. It is not source separation.

Each instrument has a time-aged paired population and exact sorted values for
decisions. Completed candidates query the preceding population, then learn once
if quality admission passes, including candidates rejected by percentile, output
RMS, or refractory checks. Both empirical cutoffs must pass. Zero percent disables
a condition; ties accept matching values with separately documented feature
resolution tolerances. Warmup ranks remain undefined and passing absolute gates
can emit provisional hits. Silence and unavailable channels still age references.

The selected instrument's UI shows saved pre-insertion histograms, cutoff lines,
candidate markers, tie intervals, measured attack envelope, frozen background,
start/completion/emission times, and decision reasons. Previous/next inspect an
immutable historical snapshot; **Live** switches to the newer distribution.
All controls appear together in pipeline order, with hover help and individual
input/trigger filter switches. Interactive
controls render in the normal-alpha top scene below toolbar/popovers.

Masks, gain curves, analysis timing, or feature-definition edits cancel affected
pending events and reset only affected references. Percentile/display edits keep
observations. Known preamp edits start a fresh percussion epoch without resetting
the arrangement stream or consumed-audio clock. Version 3 validates legacy data
before resetting incompatible percussion masks/gains; motion, arrangement, and
theme preferences survive. The migration is logged and the saved revision is
explicit. Learned song histories are not persisted.

## Reproduce the controlled and split replays

```sh
cargo test
cargo build --release --examples
target/release/examples/percussion_fixtures > /tmp/benga-percussion-fixtures.json
python3 tools/evaluate_detectors.py --count 12 --seed 20261031 \
  --split calibration --gains 0 -6 -18 -30 \
  --output /tmp/benga-histogram-calibration.json
python3 tools/evaluate_detectors.py --count 12 --seed 20261031 \
  --split heldout --gains 0 -6 -18 -30 \
  --output /tmp/benga-histogram-heldout.json
python3 tools/evaluate_detectors.py --count 12 --seed 20261031 \
  --continuous --workers 1 --gains 0 -6 -18 -30 \
  --output /tmp/benga-histogram-playlist.json
BENGA_ANALYSIS_PREVIEW_DIR=/tmp/benga-histogram-ui \
  cargo test renders_audio_analysis_presentation -- --ignored
```

The seeded selection is sorted and split into alternating calibration/held-out
tracks **before** coefficient tuning. Every report includes both partitions,
settings, executable SHA-256, decoder failures, candidate/admission/eligibility
counts, actual emission times, and process CPU cost. Native preview tests need
Metal/GPU access. The fixture runner uses deterministic anti-phase inputs,
137-frame callbacks, 44.1/48/96 kHz, eight phases, both fast FFT sizes, gain and
attack-duration variations. Its signal-to-emission delays use known source
onsets; music's onset-to-emission field begins at detected onset and cannot
measure unknown physical source latency.

## Interpreting the results

There are no annotated kick, snare, hat, breakdown, or drop labels in this
library. Counts and timing distributions diagnose chatter, missing activity,
level sensitivity, and regressions; they do **not** measure classification
precision, recall, or musical correctness. A regular snare interval can support
inspection, but syncopation and half-time arrangements make it unsuitable as a
hard BPM constraint. Synthetic causal tests cover known onset times, smooth
attacks, quiet artifacts, startup/reset, and arrangement transitions; library
replays complement those tests with real mixtures.

The percussion gates apply to the short analysis-window RMS. A track with a
quiet overall average can still contain a sufficiently loud individual attack.
An attenuation test at `-30` dB gain is consequently different from setting a
track's average RMS to `-30` dBFS. Reports preserve event RMS and whole-stream
level quantiles to make that distinction inspectable. Runtime detection never
uses a file's future loudness or duration.

## Historical pre-histogram calibration snapshot

These measurements describe the evaluated detector revision **before** gap
starts were changed to use relative energy loss without an absolute RMS gate.
They remain a reproducible calibration snapshot identified by executable digest;
the more permissive gap-start rule needs a new replay for current event counts.

The original temporal-template pipeline was saved as an executable before the
replacement was built. At a capture gain of `-6` dB, the complete archive replay
contained 1,028 decodable tracks and 58.32 hours of audio. Another 38 of the
1,066 `.ogg` files failed decoding and were excluded from detector results.
The original 4,096-sample analysis window spanned 85.3 ms at 48 kHz.

| Pipeline | Kick events | Snare events | Hat events | Drop events |
| --- | ---: | ---: | ---: | ---: |
| Original temporal templates | 252,689 | 245,366 | 657,507 | 18,006 |
| Replacement, before relative-only gap starts | 476,835 | 121,698 | 459,419 | 1,972 |

The median track's drop rate changed from 2.96 to 0.44 per minute; the highest
rate changed from 64.28 to 3.72 per minute. Total drop triggers fell by 89.0%.
Repeated baseline intervals were consistent with the gap gate rearming between
ordinary musical hits. This reduction measures trigger activity, **not** an
89% improvement in accuracy. The replacement left 278 tracks without a confirmed
drop; absence of labels prevents separating correct restraint from missed drops.

All 1,028 replacement runs satisfied the emission energy floors. The lowest
observed trigger-hop RMS was −27.000 dBFS for kick, −23.990 for snare, −26.998
for hat, and −25.995 for drop. In particular, kick validation now checks current
mix RMS when publishing its confirmed peak, as well as the earlier attack's
energy evidence.

The development sample contained 23 valid full tracks and 86.86 minutes of
audio. A matrix repeated those tracks at four gains and three sample rates:
276 valid runs; the same one corrupt file failed in each of its 12 combinations.
At 48 kHz:

| Applied gain | Kick | Snare | Hat | Drop |
| --- | ---: | ---: | ---: | ---: |
| 0 dB | 13,369 | 5,438 | 14,337 | 36 |
| −6 dB | 12,696 | 4,377 | 13,174 | 36 |
| −18 dB | 4,875 | 292 | 3,270 | 20 |
| −30 dB | 0 | 0 | 0 | 0 |

At −6 dB, 44.1/48/96 kHz all produced 36 drop events. The 48 and 96 kHz runs
produced identical kick totals, with only one snare and three hat events
differing across the complete sample. All callback partitions tested on a
90-second excerpt—137, 256, 4,096, and 8,192 frames—produced exactly 16,872
analysis hops and the same 553/48/147/0 kick/snare/hat/drop counts.

The FFT comparison used the same default thresholds at every size; its counts
show sensitivity to resolution, rather than an independently tuned quality
ranking:

| FFT samples at 48 kHz | Window | Kick | Snare | Hat | Drop |
| --- | ---: | ---: | ---: | ---: | ---: |
| 512 | 10.67 ms | 26,365 | 3,000 | 10,258 | 37 |
| 1,024, chosen default | 21.33 ms | 12,696 | 4,377 | 13,174 | 36 |
| 2,048 | 42.67 ms | 4,445 | 3,380 | 10,448 | 33 |

A separate continuous replay retained detector history across 24 valid files,
89.20 minutes, and every file boundary. It produced 12,828/4,428/13,622/60
kick/snare/hat/drop events, including transitions between files. One undecodable
file produced no samples; no partially failed input entered this replay.

Fast bass plucks and sharply struck chords can still resemble percussion to
these acoustic rules. For example, Caribou's “Odessa” produced 1,898 kick
triggers in 315.6 seconds; that activity warrants listening, not a claim that
all are musically kicks. Its drop count changed from 30 to zero. The requirement
for preceding audible activity in this recorded version favored restraint: a
flat quiet intro with no established active section could miss its first main
return. The current gap-start rule has removed that absolute level requirement;
it still requires spectral/relative-energy evidence rather than quietness alone.

The [compact result file](detector-evaluation-summary.json) contains executable
SHA-256 digests, matched corpus counts, every matrix combination, callback
checks, and selected track comparisons. All recorded replacement reports share
digest `6e71d1085130e362b314194b298753be2ecac686732ac43092c339df9e3ead90`.
Full reports remain in `/tmp/benga-final-v3-*.json`; temporary tuning traces also
stay outside the repository. Audio files were read in place and never copied
into the project.

## Relative-only gap-start follow-up

After removing the absolute gap-start RMS requirement, a bounded replay used
the same seeded development sample at −6 dB and 48 kHz: 23 valid complete tracks,
86.86 minutes, and the same one undecodable file excluded. Relative spectral
movement and at least 2 dB mix energy loss produced **76 gap entries**, compared
with 53 in the preceding snapshot. Confirmed drops increased from 36 to 60.
Kick, snare, and hat counts were identical on every matched track.

The independent drop-confirmation energy gate still passed: the lowest emitted
drop RMS was −25.732 dBFS, above its −26 dBFS floor. These counts show increased
gap-start sensitivity, not labeled accuracy. This bounded follow-up does not
replace the historical 1,028-track corpus results above.

```sh
cargo build --release --example detector_probe
python3 tools/evaluate_detectors.py --count 24 --seed 20261031 \
  --gains -6 --sample-rates 48000 \
  --output /tmp/benga-relative-gap-sample.json
```

The evaluated executable digest was
`d793b00b01cb705a1e8b091ea763590a8d22f788ad7ce520906f01c71dacecfd`.
The full follow-up report is `/tmp/benga-relative-gap-sample.json`.

## Adaptive histogram replacement (2026-10-31)

The active percussion path now uses regional candidates, fixed-window attack
concentration and energy, and six paired empirical distributions. Its equations,
units, candidate boundaries, reference semantics and controls are specified in
[the feature contract](percussion-feature-contract.md). Earlier measurements in
this document describe historical classifiers. The
[current compact results](percussion-histogram-results.json) record the evaluated
settings, track partition, counters, rejection reasons and executable identities.
Full reports remain in `/tmp/benga-histogram-final/`; decoded audio was not saved
in the repository.

The frozen evaluated probe has SHA-256
`78c725ccc706b0ddf5a5278d96c4527090372ef40c5ec6d8f6ff166a28bce83d`.
The frozen preceding probe is
`d793b00b01cb705a1e8b091ea763590a8d22f788ad7ce520906f01c71dacecfd`.
Subsequent UI text/precision, test cleanup, and the capacity-truncation
diagnostic fix produced delivery probe
`01c040d5103e97fb18b047c47c7cd169ba47d6463a8f3e4add345d7300b5dece`;
admission, event features, ranks and triggers are unchanged. Its production
settings, percussion/drop telemetry, arrangement hop count and
source duration exactly matched the evaluated probe on a 20-second held-out
excerpt of James Shinra's “Gritti.” The music matrices below identify the frozen
evaluated snapshot.

### Complete-track replays and restraint

Seed `20261031`, count `12`, established six calibration tracks and six held-out
tracks before tuning. “Burial - Etched Headplate.ogg” failed decoding and was
excluded, leaving six calibration and five held-out complete tracks: 40.14 unique
minutes. Four gain runs cover 160.57 minutes. This is a bounded replay, not a new
complete-archive evaluation. Each table entry is **candidates / admitted /
eligible**; rejected percentile or output decisions still learn when admitted.

| Applied gain | Kick C / A / E | Snare C / A / E | Hat C / A / E | Drop |
| --- | ---: | ---: | ---: | ---: |
| 0 dB | 17,533 / 6,802 / 735 | 14,742 / 8,310 / 2,233 | 15,058 / 8,735 / 2,153 | 29 |
| −6 dB | 17,501 / 6,651 / 724 | 14,724 / 8,162 / 2,050 | 15,003 / 8,574 / 1,912 | 29 |
| −18 dB | 17,316 / 5,842 / 521 | 14,772 / 6,744 / 404 | 15,084 / 7,316 / 316 | 27 |
| −30 dB | 17,101 / 4,600 / 0 | 15,036 / 2,623 / 0 | 15,285 / 3,017 / 0 | 0 |

At the primary −6 dB gain, the pre-established partitions measured:

| Partition | Full tracks | Kick C / A / E | Snare C / A / E | Hat C / A / E |
| --- | ---: | ---: | ---: | ---: |
| Calibration | 6 | 9,193 / 3,304 / 168 | 7,211 / 3,987 / 1,038 | 7,412 / 4,000 / 907 |
| Held-out | 5 | 8,308 / 3,347 / 556 | 7,513 / 4,175 / 1,012 | 7,591 / 4,574 / 1,005 |

The preceding classifier emitted 6,467 / 2,520 / 7,128 kick/snare/hat events on
these same −6 dB inputs, versus 724 / 2,050 / 1,912 now. Kick activity is
particularly conservative: “Kercha - Tigers” produced three kicks and “vowl. -
creep” four over their complete tracks. At −6 dB, 7,819 kick candidates were
rejected for overlapping feature windows and 3,575 for insufficient final
prominence; 1,372 admitted candidates failed the sharpness percentile. These
counts expose possible missed drums and starvation. They do not establish that
the preceding additional events were false, or that restraint improved accuracy.
At −30 dB, candidate discovery and learning remain active while absolute output
floors prevent triggers. Quiet-reference percentiles therefore do not promote
that material into percussion.

Drop counts match the preceding executable on **all 44 matched track/gain
runs**, with 85 events in each pipeline. Direct tests also verify arrangement
spectrum fields and cadence. This equivalence applies to matched sample inputs;
changing sample rate or retaining history across songs can change arrangement
results.

The process consumed 162.83 CPU seconds for 9,633.93 audio seconds, or **16.90 ms
CPU per audio second**, on arm64 with Rust 1.98.1. This includes production
analysis and probe telemetry/JSON, and excludes ffmpeg decoding, capture buffers
and UI rendering. The frozen baseline did not record process CPU, so its wall
time is not a comparable CPU measurement.

### Continuous playlists and physical resolution

Four playlists retained state across all eleven decodable songs. The corrupt
file contributed no samples; no partially failed decode entered any playlist.
All playlist durations were 2,408.48 seconds, with per-song boundaries and counts
saved in the compact results.

| Gain | Kick | Snare | Hat | Drop |
| --- | ---: | ---: | ---: | ---: |
| 0 dB | 714 | 2,213 | 2,117 | 38 |
| −6 dB | 710 | 2,057 | 1,905 | 39 |
| −18 dB | 524 | 413 | 324 | 35 |
| −30 dB | 0 | 0 | 0 | 0 |

The −6 dB playlist produced 39 drops versus 29 when every track began with fresh
history. Its percussion counts also differ, as expected when references and
pending arrangement evidence cross song boundaries. Playlist process CPU was
12.25 ms per audio second in this separate run; this is the same telemetry scope,
not an isolated detector benchmark.

Held-out complete tracks at −6 dB compared fast FFTs without retuning cutoffs:

| Sample rate | Fast FFT at 48 kHz | Kick | Snare | Hat | Drop | CPU ms / audio second |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 44.1 kHz | 256 | 551 | 254 | 291 | 15 | 15.90 |
| 44.1 kHz | 512 | 551 | 973 | 1,012 | 15 | 16.76 |
| 48 kHz | 256 | 555 | 235 | 281 | 17 | 15.35 |
| 48 kHz | 512 | 556 | 1,012 | 1,005 | 17 | 17.16 |
| 96 kHz | 256 | 555 | 235 | 281 | 17 | 32.46 |
| 96 kHz | 512 | 556 | 1,012 | 1,005 | 17 | 35.21 |

These are resolution/feature sensitivity measurements, not labeled quality
scores. The default fast FFT is 512. Kick retains its separate 2,048-point
window at 48 kHz; fast-window mix RMS can still affect its final sanity gate.
The actual window scales with sample rate and its physical resolution is shown
in the UI. Shortening the fast transform materially changes observed high-band
attack concentration, even with calibrated stationary power.

### Known-onset latency and checks

The production fixture runner evaluated 164 cases, including 144 canonical
attacks: three sample rates, two fast FFTs and eight phases per instrument. All
144 canonical candidates were admitted and eligible. Source-to-emission timing
includes the causal front end and feature collection; it excludes capture
buffering and the next visual frame.

| Instrument | Canonical cases | Median emission | Maximum emission |
| --- | ---: | ---: | ---: |
| Kick | 48 | 46.00 ms | 47.665 ms |
| Snare | 48 | 24.00 ms | 24.717 ms |
| Hat | 48 | 24.00 ms | 24.717 ms |

A separate 1,152-case production test spans kick frequency, phase, sample rate,
amplitude, rise and decay independently: all 288 fast attacks emitted and all
864 smooth attacks remained ineligible. Fast observed concentration ranged
0.2328–0.2840, versus 0.1540–0.1890 for these smooth fixtures. The longest rejected
smooth candidate completed 52.65 ms after known source onset; its quiet prefix
crossed the numerical discovery floor later and generated no instrument event.
This is a documented completion-time exception, not a backdated trigger.
Curved ramps are checked, and incomplete rises remain explicitly censored.

The settled defaults use a 44 ms kick horizon with minimum concentration 0.225,
and 24 ms snare/hat horizons with minimum concentration 0.46. All use a 5 ms
concentration interval, 60th/40th sharpness/energy cutoffs, eight-observation
warmup, 15-second references and a −27 dBFS output mix floor. Final prominence
floors are 0.55 / 0.16 / 0.16, distinct from the 0.08 admission floor. The feature
contract explains why the initial 24 ms kick horizon was rejected.

Probe input blocks of 137, 256, 4,096, 8,192 and 480,000 frames produced exactly
the same percussion event times, decision reasons, feature-latency summaries and
counts on the held-out excerpt: kick 58 / 43 / 27, snare 87 / 67 / 27, hat
100 / 81 / 5. Telemetry drains at most every 480 frames, so large input blocks
cannot overflow bounded event records. Drop timestamps/RMS remain callback-polled
diagnostics; their counts agree, but these diagnostic timestamps are not used
for percussion latency assertions.

Project checks passed: 404 library and 26 binary tests, with 26 and one ignored
respectively; the explicit release matrix and native GPU previews were run
separately. Ten UI previews cover normal/light/tall layouts, controls, bins,
advanced/quality pages and historical/selector states. After final UI text
cleanup, 25 focused percussion and six UI tests, release build, formatting and
diff checks also passed. A capacity diagnostic regression verifies that shortening
reference lifetime updates its truncation flag without clearing paired values.
Clippy remains blocked by three existing approximate
constant errors in `showcase.rs` and existing warnings.

Fast bass plucks and struck chords can still resemble percussion. The tests
intentionally show a narrow, sharply plucked bass tone passing the kick rule;
percentile ranks cannot determine its musical source. Overlapping feature
windows conservatively reject both candidates, which can miss rapid rolls or
weaker attacks on sustained content. Unannotated music counts and native
previews do not supply instrument precision/recall.

Reproduce the complete-track and physical-resolution measurements with the
frozen evaluated probe while that snapshot is available:

```sh
python3 tools/evaluate_detectors.py --probe /tmp/benga-histogram-final/detector_probe \
  --count 12 --seed 20261031 --gains 0 -6 -18 -30 \
  --output /tmp/benga-histogram-replay.json
python3 tools/evaluate_detectors.py --probe /tmp/benga-histogram-final/detector_probe \
  --count 12 --seed 20261031 --split heldout --gains -6 \
  --sample-rates 44100 48000 96000 --percussion-fft-sizes 256 512 \
  --output /tmp/benga-histogram-resolution.json
python3 tools/evaluate_detectors.py --probe /tmp/benga-histogram-final/detector_probe \
  --count 12 --seed 20261031 --gains 0 -6 -18 -30 --continuous \
  --output /tmp/benga-histogram-playlist.json
cargo run --release --example percussion_fixtures > /tmp/benga-histogram-fixtures.json
cargo test --release --lib production_kick_longer_horizon_experiment -- --ignored --nocapture
```

The expected corrupt file makes the library commands return nonzero after
writing their valid-run results. Rebuilding a probe produces a new executable
identity; retain its digest with any new report.

## Joint heatmap and visible pipeline controls

The percussion panel now plots a joint 24×24 sharpness/early-energy heatmap.
Density contains the current paired reference; the latest 32 learned additions
(default, adjustable 4–64) retain their original trigger outcomes as markers.
Older additions remain visible after reference expiry with a faint style and
never contribute to current counts or cutoffs. Coincident-marker badges show
repeated additions without shifting their measured positions.

All controls are visible in processing order, with plain hover explanations and
11 independent input/trigger filter switches. Switching a check off retains its
value, removes its displayed cutoff, and skips only that rule. Defaults keep
every existing check enabled. Historical heatmaps save their own thresholds,
filter switches, joint counts and recent additions.

[The heatmap validation record](percussion-heatmap-validation.json) records 449
passing project tests, 11 focused UI tests, hover-help coverage, native Metal
previews, and exact default-behavior agreement on nine sample-rate/gain profiles
from a held-out excerpt. The comparison checks trigger times, candidate/learned
counts, event levels, completion delays and renamed decision reasons. The joint
display does not introduce a joint-density classifier: enabled sharpness and
energy percentile checks remain separate and both must pass before a trigger.

## Separate stereo movement

Percussion now retains independent L/R FFT power, smoothed envelopes and frozen
backgrounds through feature extraction. Positive amplitude growth is rectified
on each side before RMS combination. Attack energy averages each side's
positive background excess. A declining left channel can therefore no longer
cancel a fresh right-channel attack, or the reverse. There is one shared
candidate and output decision per instrument, so a stereo hit does not double
its observations or triggers. Global level and absolute energy retain calibrated
mean stereo power; no fixed width bonus is added.

A production regression reproduced the old failure with opposing-channel
movement and nearly constant total energy. It now learns and emits once with
unchanged defaults. All 96 centered, panned, anti-phase, wide, swapped, quadrature
and delayed-side snare/hat fixtures emit within 24.717 ms. Equal-power pan
fixtures retain energy and concentration; slow panning and independent steady
L/R noise remain quiet. Abrupt spatial panning can still resemble an attack.
The inspector shows saved L solid / R dashed envelopes, faint combined level,
and dotted independent backgrounds.

[The stereo validation record](percussion-stereo-validation.json) contains
executable hashes, checks and matched complete-track replays at −6 dB. The
11 valid tracks total 40.14 minutes; the same corrupt file is excluded.

| Pipeline | Kick | Snare | Hat | Drop | CPU ms / audio second |
| --- | ---: | ---: | ---: | ---: | ---: |
| Before separate movement | 724 | 2,050 | 1,912 | 29 | 15.39 |
| Separate stereo movement | 835 | 1,472 | 1,589 | 29 | 22.05 |

All matched arrangement hop counts and drop counts agree. Percussion activity
changes because the movement features and their learned distributions changed;
these unlabeled counts do not establish improved accuracy. The new path runs
four FFTs per percussion hop across the fast and kick windows, reusing plans,
scratch and buffers. The measured CPU scope includes analysis/probe telemetry
and excludes decoding/capture/rendering. The larger immutable stereo traces use
bounded preallocated storage rather than increasing thread-stack requirements.

Validation: 436 library and 26 binary tests pass, plus the explicit native Metal
stereo preview. Existing ignored GPU/benchmark tests remain separately invoked.
The feature contract documents equations, units and the rapid-panning ambiguity.
