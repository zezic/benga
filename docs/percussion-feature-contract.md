# Adaptive percussion feature contract

This contract belongs to the histogram percussion revision. The arrangement
transform retains its existing samples, normalization, startup fill, and cadence.
The independent percussion branch consumes the same stereo samples, after the
known preamp, on its own sample clock. Instrument ranks describe recent regional
attacks; they are not instrument probabilities.

## Causal front end and units

Each percussion hop reports stereo regional mean-square power in full-scale².
Left and right now use separate FFT buffers and transforms. Each side retains
its own calibrated power through envelope growth and background subtraction.
Only positive movements are combined; channel waveforms and complex spectra are
never mixed. This preserves anti-phase material and prevents a falling left
channel from canceling a fresh right-channel attack (or the reverse).
For a Hann window, each side’s positive-frequency power bin is divided by
`N * sum(window²)` and doubled except at DC and Nyquist. An attenuation fader is
an amplitude gain and enters regional power as `gain²`.

Kick selects 60–200 Hz. Snare has unit **power** weight over 2–5 kHz and a raised
cosine taper to zero at 10 kHz. Hat selects 5–16 kHz. The mask is analytically
integrated over each bin's physical support; unavailable frequencies are clipped
at Nyquist. A mask with no support reports unavailable, emits no observations or
hits, and still ages its previous reference. Fractional bin weights do not add
frequency resolution or make finite-window leakage disappear.

The fast branch compares 256/512 samples at 48 kHz and defaults to 512. The
1 ms hop is rounded to an integer sample count. Transform sizes scale to the
nearest power of two at other rates. The kick uses an independent longer window
because 256/512 samples cannot reliably select 60–200 Hz. The phase experiment
measured a 60 Hz regional power spread of 0.900 / 0.338 / 0.0011 / 0.0067 for
256/512/1024/2048 samples across 32 phases. Actual bin width and window duration
are exposed in the inspector, including their sample-rate-dependent values.

No result is available before that channel's transform is filled. The first
available frame establishes its baseline, so starting capture inside a held
sound does not manufacture an attack. Nonfinite samples make every affected
window unavailable until the bad sample has left it. No filter or FFT delay is
subtracted from the sample clock.

## Candidate boundaries and state

Each side has an envelope `A_c`, an exponential smoothing of `sqrt(P_c)` with a
1 ms time constant. Growth is rectified separately,
`d_c = max(A_c[n] - A_c[n-1], 0)`, then combined as
`d = sqrt((d_L² + d_R²) / 2)`. The mixed display level remains
`sqrt((A_L² + A_R²) / 2)` and never substitutes for this movement measurement.
All smoothing uses actual hop duration. A candidate begins at the
start of the first hop whose positive envelope growth exceeds the larger of
`1e-6`, 0.1% of the preceding envelope, and 1.5 times the causal side-movement RMS
noise follower, with a -80 dBFS (0.0001 amplitude) numerical selected-region
entry floor. That floor remains roughly 15 times below typical admitted regional
amplitudes. It rejects tiny finite-window leakage from an unrelated sustained
440 Hz bed which otherwise repeatedly starts numerical candidates and can block
its first kick. The follower and regional power background use an 80 ms time
constant. These are extraction helpers, never final score thresholds.

The boundary is established before admission energy, output RMS, physical
sharpness, or percentile gates. One preceding envelope/power sample supplies
bounded pre-roll. The causal background power immediately preceding that hop
is frozen separately for left and right for the entire candidate. The state progresses through idle,
collecting, finalizing, and rearming. A settling envelope rearms after 12 ms;
settling uses actual envelope growth rather than its adaptive noise floor, so
learning a long ramp as background cannot restart it when it becomes loud.
Continuing downward tails can rearm above silence, allowing new attacks over a
sustained bed.

Lobes closer than the 6 ms merge interval remain one candidate. A fresh rise
after a local trough can start another bounded pending event. If two feature
windows overlap, both records explicitly carry `overlapping feature windows`
and are excluded from learning and output. This conservative policy preserves
the second candidate in diagnostics, without claiming either fixed window is
isolated source energy. It can miss very rapid rolls; changing this policy would
require evidence for a source-excess estimator. Up to eight events per channel
can be pending, with fixed sample/segment storage and no hop-time allocation.

## Observed concentration and fixed-window energy

The feature spike began with a 24 ms horizon and a 5 ms concentration interval.
The final kick horizon is **44 ms**; snare and hat retain 24 ms. Concentration
is 5 ms on all three. This change follows a measured failure of the initial
low-frequency prefix: the 2,048-point Hann kick front end with a 24 ms horizon
assigned higher concentration to smooth 30 ms ramps than fast attacks. A 40 ms
horizon still had slight overlap at 44.1 kHz. At 44 ms, the front-end matrix
separates the tested fast and smooth attacks while meeting the emission target.
Each hop contributes the RMS-combined positive left/right growth `d` uniformly
over its actual time interval. Sharpness is the largest growth integral in any 5 ms interval,
divided by total positive growth in the full feature window. Maxima are evaluated
at segment edges and edges shifted by the concentration interval. Fractional
intervals are integrated, so changing hop rounding does not silently change the
feature definition. Negligible total growth is assigned zero sharpness and
cannot enter the reference.

This is observed concentration in the fixed window, not a full rise-time
estimate. An ongoing rise at feature completion is explicitly marked censored.
Different slow attacks with identical early prefixes can have identical
concentration. Display peak holds and envelope downsampling never feed features.

Absolute energy integrates `(P_L + P_R) / 2` over the fixed horizon. Attack
energy integrates `(max(P_L - B_L, 0) + max(P_R - B_R, 0)) / 2`. The frozen
backgrounds `B_L` and `B_R` belong to their own sides, so opposing decay cannot
subtract the new side’s attack energy. Power is linearly interpolated over each
hop and positive-excess integration treats each background crossing exactly.
The last hop is clipped to the fixed horizon. Summing hop integrals once avoids
double counting overlapping FFT windows. Values are linear full-scale² seconds.
The displayed attack level is `10*log10(attack_energy / feature_seconds)`, an
early-attack dBFS-equivalent level, not waveform peak amplitude.

The selected estimate is frozen **regional, per-side** positive excess. It is
causal and bounded, and a gain-scaled copy produces gain² energy. A dying
overlapping tone can still offset a fresh attack within the same side and region,
but decay on the opposite stereo side no longer understates it.
Per-bin positive excess would reduce cancellation but could overcount pitch
motion as new energy; it was not silently substituted. Neither estimator is
source-separated instrument energy.

Admission requires meaningful absolute attack energy, actual positive envelope
growth, and local prominence. For every hop, calculate positive side excursions
`e_c = max(A_c - sqrt(B_c), 0)`. The admitted growth is the smaller of total
positive stereo growth, `max_t sqrt((e_L² + e_R²) / 2)`, and the physical mixed
peak. Prominence divides this bounded growth by the mixed peak. This prevents a
tiny crackle from borrowing a loud bed's stale background difference. Smooth
notes are allowed to learn. Physical concentration, final output prominence,
absolute attack level,
fixed-horizon mix RMS, and output interval are separate output gates; an
upper-ranked smooth or quiet population does not bypass them.

The admission prominence floor is 0.08; the separate final output prominence
floor is 0.55 for kick and 0.16 for snare/hat. A stationary broadband-noise
fixture initially produced four false hits in three seconds: kick prominence
0.43/0.50 and snare/hat prominence 0.13/0.10. The final contrast floors reject
those stochastic fluctuations while still learning them. They do not compare
one instrument against another. A weaker kick over a strong sustained signal
inside the same selected mask can be missed by this conservative contrast gate;
it is exposed for tuning rather than hidden as classifier confidence. A loud
440 Hz bed does not prevent the tested 120 Hz kicks from emitting.

## Paired references and decisions

Each instrument has one deque of paired `(sharpness, attack_energy)` records,
two sorted feature arrays, and two histograms. The exact arrays make percentile
decisions; the optional cloud floors use supported marginal bins. All collections
are allocated once. At each feature completion, records first expire using the
actual completion audio time. The candidate is checked for admission, queried
against the preceding reference, decided, then inserted exactly once if
admitted. Percentile, RMS, physical-sharpness, and output-interval rejection do
not stop learning.

For `p > 0`, the exact empirical cutoff is `sorted[ceil(p*N)-1]`. A zero
percentile disables that comparison. Equality passes. The fixed concentration
tolerance is 0.01; the energy tolerance is 3% (0.128 dB), with a `1e-12` numerical
minimum. The decision explicitly records tolerance-based acceptance. These
tolerances apply to exact feature values, not histogram bins. Ties are displayed
as the interval `[count(<value)/N, count(<=value)/N]`.

Before eight paired observations exist, both ranks/cutoffs are undefined.
Absolute-quality rules can emit provisional hits during this warmup; they are
identical before and after warmup. Silence and unavailable channels expire their
references normally into warmup. The default lifetime is 15 seconds. Capacity is
8,192 observations per instrument; if exceeded, the oldest pair is removed and
the effective retained span and truncation are reported. Timing is `f64` audio
time, so a day-long capture does not quantize short intervals.

Mask, fader, FFT/hop, envelope, candidate-boundary, or feature-window changes
start a new epoch for the affected channels, canceling their pending events and
references. Percentile and output-floor changes preserve observations. Known
preamp changes reset percussion analysis without resetting arrangement cadence
or the absolute sample clock. Learned histories are never persisted.

Every completed candidate stores an immutable decision with its own pre-insertion
histograms, axis metadata, counts, exact cutoffs, ranks, reason, sampled envelope,
background, onset boundary, feature completion, and emission availability.
There are 256 bounded records shared across the three instruments. Historical
inspection uses that saved reference; the live distribution is explicitly
different. Strength sent to the established visualizer is bounded to 0.15–1.0;
linear energy and ranks remain separate diagnostics.
Creation identity (`candidate_id`) is separate from the monotonically increasing
publication cursor (`id`): a later-created 24 ms snare can complete before a
pending 44 ms kick without making a display or trace reader skip that kick.
Preamp epoch changes preserve both identities and saved decisions on the
continuing audio clock. A full stream reset starts a new clock and history.
Pending-queue exhaustion produces an explicit capacity rejection with no invented
feature window or ranks.

## Joint heatmap and the two gate stages

The joint heatmap is a visualization of the paired reference. Horizontal position
is sharpness (0–1), vertical position is early attack level
`10*log10(E_attack / feature_seconds)` in dBFS-equivalent units. A cell counts
admitted observations still retained by the reference. Its row/column sums match
the two marginal histograms. Exact sorted feature values continue to decide the
percentile cutoffs; the heatmap introduces no joint-density or confidence gate.

The cloud tracker pools each energy row across sharpness and each sharpness
column across energy. A bucket can extend its axis only while it holds at least
**Cloud bucket support** retained observations (default 3, shared by both axes).
The lowest supported bucket's lower edge and highest supported bucket's upper
edge define each span independently. Bounds recompute from current counts,
including time and capacity evictions. Sparse observations still learn and stay
visible. One supported bucket retains its full height/width; an unsupported axis
uses its fixed fallback even when the other axis has supported bounds.

Each cloud axis has an independent checkbox beside its percentage control.
**Cloud energy top area** X% sets `max_db - (max_db - min_db) * X / 100`, converted back to attack
energy using the feature horizon. **Cloud sharpness top area** sets
`max_sharpness - (max_sharpness - min_sharpness) * Y / 100`. Both default to 25%.
These are fractions of the displayed logarithmic energy height and linear
sharpness width, not percentiles of observations. Zero requires the highest
edge; 100% admits the full supported span. Equality and values above either
upper edge pass that branch; neither axis imposes an upper limit.

In cloud mode the trigger floors combine with **OR**: energy at/above its floor
or sharpness at/above its floor is sufficient for this gate. The cloud energy
and cloud sharpness checkboxes choose the participating branches directly;
neither requires the legacy fixed-floor checkbox. A disabled cloud branch does
not automatically satisfy OR.
Before an axis has support its fixed floor participates as a fallback, still
using OR. Both cloud checkboxes default to off; when both are off, the original
AND of enabled fixed floors applies. Their legacy switches affect this fixed
mode only. Input quality, output prominence, mix level, independent percentiles,
and spacing remain separate requirements. Support is independent of percentile
warmup. Mode, percentage and support edits preserve history and pending attacks.

The map outlines both supported spans, shades their OR union intersected with
independent input-energy and percentile bounds. Energy retains its original
full-width dashed threshold guide. Sharpness uses a matching full-height dashed
vertical guide and a numeric label. Both use the same neutral ink, line width,
opacity and dash pattern.
Resolved fallback cutoffs remain visible before supported bounds exist.
Each decision captures its pre-insertion cloud spans, floors and Boolean mode,
so historical inspection preserves the original area as live data changes.

Learning and output use different thresholds:

| Stage | Required checks | Percentile cutoff? |
| --- | --- | --- |
| Input filtering | Valid bounded attack plus the enabled overlap, energy, rise and contrast checks | None |
| Trigger filtering | Input filtering plus enabled sharpness, contrast, energy, mix level, percentile and spacing checks | Both enabled marginal cutoffs, against earlier accepted inputs |

For every completed candidate the detector first expires old references, checks
admission, queries the **preceding** population and decides output, then inserts
one sharpness/energy pair if admission passed. A percentile-rejected, quiet, or
refractory candidate still learns when it passes admission. The candidate cannot
raise its own threshold. Each optional input and trigger filter has a persisted
on/off switch. A disabled filter contributes no cutoff; its configured value is
retained for later re-enabling. Valid finite data and meaningful nonzero growth
remain prerequisites for measuring an attack. The default switches preserve the
previous detector behavior. During warmup (default eight observations), unavailable
percentiles are bypassed and an otherwise valid hit is provisional. Setting a
percentile to zero disables only that comparison.

Solid cutoff guides mark nominal empirical percentiles; physical output floors
and the separate admission-energy floor have distinct guide styles. The shaded
region uses the effective output feature boundaries, including the small tie
tolerances and the fixed-floor intersection or cloud-floor union. Being in this
region establishes the sharpness/energy part only: admission, prominence, mix
RMS and spacing still matter. The selected candidate's saved decision reason
reports the actual result.

The overlay shows the last N **admitted additions** (default 32, selectable 4–64)
from a separate bounded per-instrument history, rather than the shared candidate
ring. Triggered and learned-only additions have different marker styles; their
outcomes are saved and never recomputed from newer cutoffs. Additions that have
expired from the reference remain visible as dimmed recent markers and contribute
nothing to density or cutoffs. Fewer markers appear until enough additions exist
in the current feature epoch. A feature epoch reset clears incompatible history;
changing N is a persisted display preference and does not reset learning.

Historical inspection saves the joint counts, recent markers, physical floors,
feature horizon and pre-insertion cutoffs. **Live** uses the current reference and
current controls; marker fill still describes the original outcome. Accordingly,
a historical trigger can sit outside the current live feature-pass region.

## Deterministic evidence

Run the causal feature fixture suite without decoding music:

```sh
cargo test --lib visualizer::transients::tests -- --nocapture
```

All 21 classifier tests pass. A 2 ms linear rise measures concentration
1.0; a 24 ms linear rise measures 5/24. After equalizing their **measured prefix
energies**, these concentrations remain distinct. Doubling amplitude multiplies
energy by four without changing concentration. Hops of 0.5/0.7/1.0/1.3/2.0 ms
produce the same linear-ramp concentration after interval interpolation. A
stationary power of 1 integrated for 24 ms measures 0.024 FS²s absolute energy;
against background power 0.25 it measures 0.018 FS²s attack energy. A power hop
crossing a frozen 0.5 background from zero to one over 1 ms measures 0.000125
FS²s positive excess.

The same suite checks one observation per repeated attack, matching 100th
percentile ties, empty and one/two-element references, monotonic cutoffs,
self-inclusion prevention, rejected-candidate learning, RMS/refractory learning,
tolerance reporting, immutable histograms, idle expiry, capacity bounds,
channel-local epochs, a slow attack crossing gates late, explicit close-hit
overlap rejection, quiet crackle over a loud bed, smooth-only populations, and
day-long absolute clocks. Front-end and production integration tests separately
cover normalization, phases/leakage, sample rates, stereo, callback partitions,
and bitwise arrangement input/cadence equivalence.

The front-end matrix uses four frequencies (60/80/120/180 Hz), four phases,
44.1/48/96 kHz, amplitudes 0.2/0.4/0.8, two exponential decays (60/150 ms), and
four linear attack durations (1/30/60/120 ms). With the selected kick 44 ms
horizon, all 288 fast attacks pass and all 864 smooth attacks fail the absolute
concentration rule. Observed fast concentration spans 0.2328–0.2840; smooth
concentration spans 0.1540–0.1890. The final physical kick floor is 0.225. Maximum
actual fast emission delay is 47.66 ms. Rejected smooth notes complete as late as
50.66/51.66/52.65 ms for 30/60/120 ms rises because their numerical entry floor
is crossed later; these are not emitted percussion. The 1 ms-hop timestamp
includes transform fill and feature completion;
these figures exclude capture buffering and the next visual frame.

```sh
cargo test --release --lib production_kick_longer_horizon_experiment -- --ignored --nocapture
```

Production nuisance fixtures also check sustained tones across phases, a pitch
sweep, vibrato, slow tremolo, loud/quiet steady broadband noise, narrow isolated
clicks, and quiet crackle alone and over a loud unrelated bed. The paired-bin
bias fixture explicitly demonstrates a dying neighbor canceling regional energy,
and the alternative per-bin excess falsely measuring energy when a held tone
moves between bins. This evidence supports the conservative regional estimate
and documents its overlap limitation rather than claiming source separation.
Another 96 snare/hat production fixtures cover phases, levels, stereo polarity,
44.1/48/96 kHz, and 256/512-point fast FFTs. Measured signal-to-emission delay is
24.000 ms at the median and 24.717 ms at the 95th percentile and maximum.

## Controls follow the pipeline

The full control surface is visible together, in processing order:
choose frequencies, find attacks, measure attacks, filter inputs, configure recent
history, and filter triggers. The recent history supplies the comparison
population; the current candidate is still inserted only after its output
decision. There are no hidden primary/advanced pages. Every scalar, filter
switch, and navigation control has a short hover explanation. Filter switches
keep their numeric thresholds and are saved independently of the values.

Input filtering can independently disable minimum energy, minimum rise, minimum
contrast, or rejection of overlapping attacks. Trigger filtering can independently
disable the minimum sharpness, minimum contrast, minimum attack energy, mix level,
each percentile comparison, or output spacing. Disabling overlap rejection
permits mixed-window measurements to learn and trigger, without claiming that
they isolate either source. The heatmap hides disabled guides and recalculates
the shaded feature-pass region from the enabled conditions saved in that view.

## Stereo movement validation

A production fixture first reproduced the missing event: one side introduces a
new in-band attack while the other decays, leaving regional mean power nearly
constant. The previous mixed-before-growth path learned no event. Independent
side growth and side-positive excess now retain it. Candidate boundaries and
paired learning remain shared, so a stereo hit still produces one observation
and one instrument trigger.

The same mean-square normalization applies to both sides. Duplicated mono,
anti-phase, left/right swaps and fixed-pan equal-energy copies retain equivalent
levels and feature scale; there is no automatic stereo-width multiplier. Noise
tracking uses RMS signed side motion before its causal follower, preventing
opposing fluctuations from hiding the estimated noise motion.

Rapid constant-power panning of a held sound can resemble a spatial attack under
these measurements; it is not guaranteed to identify a new musical source. Slow
constant-power panning and independent steady left/right broadband noise are
explicit nuisance fixtures. Same-side overlapping sounds remain mixtures.
The inspector saves the independent side envelopes/backgrounds and combined
positive growth so a flat mixed level does not conceal the feature evidence.
