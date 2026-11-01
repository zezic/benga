//! Live analyzer plots composed into the presentation itself.
//!
//! Normal/POI mode entries choose initial compositions; detector events may
//! add more and retire them. Their contents keep updating between events;
//! layout and animation seeds stay fixed until the next composition or resize.

use vello::{
    kurbo::{Affine, Line, Point, Rect, Stroke},
    Scene,
};

use crate::text::Fonts;

use super::super::{showcase::ShowcaseMode, Rng, TriggerCounts, Visualizer};
use super::{
    color::ScenePalette,
    plots::{
        plot_bounds, PlotKind, ScenePlot, AGGRESSION_MIN_RATIO, PLOT_GUTTER_HEIGHT,
        PLOT_GUTTER_WIDTH,
    },
    stage::Stage,
};

const CARD_GAP: f64 = 5.0;
const FORMATION_AIR: f64 = 36.0;
const GROUP_STAGGER: f32 = 0.085;
const MIN_CHART_WIDTH: f64 = 180.0;
const INITIAL_COOLDOWN: f32 = 0.35;
const MODE_ENTRY_SPAWN_CHANCE: f32 = 0.75;
const MAX_GROUPS: usize = 2;
const MAX_PLOTS: usize = 8;
const MIN_HOLD_SECONDS: f32 = 18.0;
const HOLD_SECONDS_SPAN: f32 = 10.0;
const MIN_RETIREMENT_EVENTS: u32 = 48;
const RETIREMENT_EVENTS_SPAN: usize = 49;
const MIN_TIMEOUT_SECONDS: f32 = 75.0;
const TIMEOUT_SECONDS_SPAN: f32 = 30.0;

pub(super) struct GraphCompositions {
    groups: Vec<GraphGroup>,
    cooldown: f32,
    last_triggers: Option<TriggerCounts>,
    last_audio_time: Option<f64>,
    last_mode: Option<ShowcaseMode>,
    mode_entry_pending: bool,
    context: Option<Context>,
    viewport: Option<(f64, f64)>,
    previous_family: Option<Family>,
}

impl Default for GraphCompositions {
    fn default() -> Self {
        Self {
            groups: Vec::new(),
            cooldown: INITIAL_COOLDOWN,
            last_triggers: None,
            last_audio_time: None,
            last_mode: None,
            mode_entry_pending: false,
            context: None,
            viewport: None,
            previous_family: None,
        }
    }
}

struct GraphGroup {
    cards: Vec<GraphCard>,
    life: Lifetime,
    data_frozen: bool,
}

struct GraphCard {
    plot: ScenePlot,
    spec: PlotSpec,
    rect: Rect,
    seed: u64,
    pulse: f32,
}

struct Context {
    model: Option<String>,
    percussion_epochs: [u64; 3],
}

impl Context {
    fn matches(&self, visualizer: &Visualizer) -> bool {
        self.model.as_deref() == visualizer.model_id()
            && self.percussion_epochs == Self::percussion_epochs(visualizer)
    }

    fn capture(visualizer: &Visualizer) -> Self {
        Self {
            model: visualizer.model_id().map(str::to_owned),
            percussion_epochs: Self::percussion_epochs(visualizer),
        }
    }

    fn percussion_epochs(visualizer: &Visualizer) -> [u64; 3] {
        std::array::from_fn(|channel| visualizer.percussion_channel_status(channel).epoch)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Events {
    pub(super) kick: bool,
    pub(super) snare: bool,
    pub(super) hat: bool,
    pub(super) drop: bool,
    pub(super) event_count: u32,
}

impl Events {
    pub(super) fn between(previous: Option<TriggerCounts>, current: TriggerCounts) -> Self {
        previous.map_or_else(Self::default, |last| Self {
            kick: current.kick > last.kick,
            snare: current.snare > last.snare,
            hat: current.hat > last.hat,
            drop: current.drop > last.drop,
            event_count: current
                .kick
                .saturating_sub(last.kick)
                .saturating_add(current.snare.saturating_sub(last.snare))
                .saturating_add(current.hat.saturating_sub(last.hat))
                .saturating_add(current.drop.saturating_sub(last.drop)),
        })
    }

    pub(super) fn any(self) -> bool {
        self.kick || self.snare || self.hat || self.drop
    }

    pub(super) fn count(self) -> u32 {
        self.event_count.max(
            u32::from(self.kick)
                + u32::from(self.snare)
                + u32::from(self.hat)
                + u32::from(self.drop),
        )
    }

    pub(super) fn spawn_chance(self) -> f32 {
        let per_event = if self.drop {
            1.0
        } else if self.kick || self.snare {
            0.82
        } else if self.hat {
            0.38
        } else {
            0.0
        };
        // Preserve opportunities when several audio onsets arrive between
        // display frames, rather than reducing a batch to one coin flip.
        1.0 - (1.0f32 - per_event).powi(self.count().min(32) as i32)
    }
}

#[derive(Clone, Copy)]
pub(super) struct Lifetime {
    pub(super) age: f32,
    pub(super) entrance: f32,
    pub(super) exit: f32,
    pub(super) dwell_until: f32,
    pub(super) maximum: f32,
    pub(super) exiting_at: Option<f32>,
    pub(super) count: usize,
    event_budget: u32,
    events_seen: u32,
}

impl Lifetime {
    pub(super) fn new(count: usize, rng: &mut Rng) -> Self {
        let entrance = 0.55 + rng.unit() * 0.3;
        let dwell_until = entrance
            + GROUP_STAGGER * count.saturating_sub(1) as f32
            + MIN_HOLD_SECONDS
            + rng.unit() * HOLD_SECONDS_SPAN;
        Self {
            age: 0.0,
            entrance,
            exit: 0.42 + rng.unit() * 0.23,
            dwell_until,
            maximum: MIN_TIMEOUT_SECONDS + rng.unit() * TIMEOUT_SECONDS_SPAN,
            exiting_at: None,
            count,
            event_budget: MIN_RETIREMENT_EVENTS + rng.range(RETIREMENT_EVENTS_SPAN) as u32,
            events_seen: 0,
        }
    }

    pub(super) fn retire(&mut self) {
        if self.exiting_at.is_none() {
            self.exiting_at = Some(self.age);
        }
    }

    pub(super) fn advance(
        &mut self,
        dt: f32,
        events: Events,
        context_changed: bool,
        _rng: &mut Rng,
    ) {
        self.age += dt.max(0.0);
        // Busy percussion during the protected hold must not pre-pay the
        // retirement budget and dismiss a formation on its first later hit.
        if self.age >= self.dwell_until {
            self.events_seen = self.events_seen.saturating_add(events.count());
        }
        if context_changed
            || self.age >= self.maximum
            || (self.age >= self.dwell_until
                && events.any()
                && self.events_seen >= self.event_budget)
        {
            self.retire();
        }
    }

    pub(super) fn exiting(&self) -> bool {
        self.exiting_at.is_some()
    }

    pub(super) fn finished(&self) -> bool {
        self.exiting_at.is_some_and(|start| {
            self.age - start >= self.exit + GROUP_STAGGER * (self.count - 1) as f32
        })
    }

    pub(super) fn animation(&self, index: usize) -> (f32, f32) {
        let entrance_age = self.exiting_at.unwrap_or(self.age);
        let entered = smoothstep((entrance_age - index as f32 * GROUP_STAGGER) / self.entrance);
        let leaving = self.exiting_at.map_or(0.0, |start| {
            let delay = (self.count - index - 1) as f32 * GROUP_STAGGER;
            smoothstep((self.age - start - delay) / self.exit)
        });
        // Freezing the entrance at retirement preserves partial coverage when
        // a POI or model change interrupts a composition's first moments.
        (entered * (1.0 - leaving), 1.0 - leaving)
    }
}

fn graph_animation(kind: PlotKind, life: &Lifetime, index: usize, seed: u64) -> (f32, f32) {
    if matches!(kind, PlotKind::Heatmap(_)) {
        return life.animation(index);
    }

    // Treat each plot as one lamp: full geometry flickers into steady light,
    // rather than assembling or dissolving individual trace segments.
    let entrance_age = life.exiting_at.unwrap_or(life.age) - index as f32 * GROUP_STAGGER;
    let entered = neon_level(
        entrance_age / life.entrance,
        seed,
        &[0.0, 0.68, 0.07, 1.0, 0.14, 0.72, 0.32, 0.92, 0.86, 1.0],
    );
    let shutdown = life.exiting_at.map_or(1.0, |start| {
        let delay = (life.count - index - 1) as f32 * GROUP_STAGGER;
        neon_level(
            (life.age - start - delay) / life.exit,
            seed.rotate_left(17),
            &[1.0, 0.65, 0.12, 0.9, 0.18, 0.55, 0.06, 0.2, 0.0, 0.0],
        )
    });
    // Retirement freezes ignition, so a plot interrupted before lighting
    // cannot strike for the first time during its shutdown.
    let alpha = entered * shutdown;
    (if alpha > 0.0 { 1.0 } else { 0.0 }, alpha)
}

fn neon_level(progress: f32, seed: u64, levels: &[f32; 10]) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    // Vary the strike timing per plot without consuming randomness per frame.
    let skew = (((seed ^ (seed >> 32)) & 255) as f32 / 255.0 - 0.5) * 0.32;
    let phase = progress + skew * progress * (1.0 - progress);
    levels[((phase * levels.len() as f32) as usize).min(levels.len() - 1)]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Heatmaps,
    Attack,
    SpectralShape,
    Aggression,
    Energy,
    Bass,
    Onsets,
}

impl Family {
    fn choose(rng: &mut Rng, previous: Option<Self>) -> Self {
        let mut chosen = Self::Heatmaps;
        for _ in 0..3 {
            chosen = match rng.range(100) {
                0..=47 => Self::Heatmaps,
                48..=61 => Self::Attack,
                62..=72 => Self::SpectralShape,
                73..=80 => Self::Aggression,
                81..=87 => Self::Energy,
                88..=93 => Self::Bass,
                _ => Self::Onsets,
            };
            if Some(chosen) != previous {
                break;
            }
        }
        chosen
    }
}

#[derive(Clone, Copy)]
struct PlotSpec {
    kind: PlotKind,
    /// Ratio of the actual chart, with heading/legend gutters excluded.
    ratio: f64,
    cluster: Option<Cluster>,
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum FormationPreview {
    Mixed,
    Sidecar,
    Stepped,
    Split,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cluster {
    Row,
    Column,
    TwoOverOne,
    TwoBesideOne,
    Stepped,
    Sidecar,
    Compound,
}

fn choose_specs(
    family: Family,
    rng: &mut Rng,
    events: Events,
    available_attacks: [bool; 3],
) -> Vec<PlotSpec> {
    let channel = if events.snare {
        1
    } else if events.hat && !events.kick {
        2
    } else if events.kick {
        0
    } else {
        rng.range(3)
    };
    if family == Family::Heatmaps {
        let mut channels = [0, 1, 2];
        shuffle(&mut channels, rng);
        let count = 1 + rng.range(3);
        let cluster = if count >= 2 && rng.unit() < 0.64 {
            Some(match rng.range(if count == 3 { 4 } else { 2 }) {
                0 => Cluster::Row,
                1 => Cluster::Column,
                2 => Cluster::TwoOverOne,
                _ => Cluster::TwoBesideOne,
            })
        } else {
            None
        };
        let mut specs: Vec<_> = channels[..count]
            .iter()
            .enumerate()
            .map(|(index, channel)| PlotSpec {
                kind: PlotKind::Heatmap(*channel),
                ratio: match cluster {
                    Some(Cluster::TwoOverOne) => [1.0, 1.0, 2.0][index],
                    Some(Cluster::TwoBesideOne) => [1.0, 1.0, 0.5][index],
                    // Mixed coaxial maps give rows a common chart height and
                    // columns a common chart width without distorting axes.
                    Some(Cluster::Row | Cluster::Column) => {
                        if index % 2 == 0 {
                            1.0
                        } else {
                            2.0
                        }
                    }
                    None | Some(Cluster::Stepped | Cluster::Sidecar | Cluster::Compound) => {
                        [1.0, 2.0, 0.5][rng.range(3)]
                    }
                },
                cluster,
            })
            .collect();
        if rng.unit() < 0.48 {
            let channel = channels[0];
            if available_attacks[channel] && specs.len() < 4 {
                specs.push(plot_spec(PlotKind::Attack(channel), rng));
            }
            if specs.len() < 4 {
                specs.push(plot_spec(PlotKind::Onsets, rng));
            }
            let cluster = mixed_cluster(specs.len(), rng);
            apply_cluster(&mut specs, cluster);
        }
        return specs;
    }
    if family == Family::Attack {
        let mut specs = vec![plot_spec(PlotKind::Heatmap(channel), rng)];
        if available_attacks[channel] {
            specs.push(plot_spec(PlotKind::Attack(channel), rng));
        }
        specs.push(plot_spec(PlotKind::Onsets, rng));
        if rng.unit() < 0.35 {
            specs.push(plot_spec(PlotKind::Energy, rng));
        }
        let cluster = mixed_cluster(specs.len(), rng);
        apply_cluster(&mut specs, cluster);
        return specs;
    }
    let kinds = match family {
        Family::SpectralShape => [
            PlotKind::SpectralShape,
            PlotKind::Aggression,
            PlotKind::Energy,
            PlotKind::Bass,
        ],
        Family::Aggression => [
            PlotKind::Aggression,
            PlotKind::Energy,
            PlotKind::SpectralShape,
            PlotKind::Bass,
        ],
        Family::Energy => [
            PlotKind::Energy,
            PlotKind::Bass,
            PlotKind::Aggression,
            PlotKind::SpectralShape,
        ],
        Family::Bass => [
            PlotKind::Bass,
            PlotKind::Onsets,
            PlotKind::Energy,
            PlotKind::Aggression,
        ],
        Family::Onsets => [
            PlotKind::Onsets,
            PlotKind::Heatmap(channel),
            if available_attacks[channel] {
                PlotKind::Attack(channel)
            } else {
                PlotKind::Energy
            },
            PlotKind::Bass,
        ],
        Family::Heatmaps | Family::Attack => unreachable!(),
    };
    let count = 2 + rng.range(3);
    let mut specs: Vec<_> = kinds[..count]
        .iter()
        .map(|kind| plot_spec(*kind, rng))
        .collect();
    let cluster = mixed_cluster(specs.len(), rng);
    apply_cluster(&mut specs, cluster);
    specs
}

fn plot_spec(kind: PlotKind, rng: &mut Rng) -> PlotSpec {
    let ratio = match kind {
        PlotKind::Heatmap(_) => [1.0, 2.0, 0.5][rng.range(3)],
        PlotKind::Attack(_) => 2.2 + f64::from(rng.unit()) * 0.6,
        PlotKind::Onsets => 2.8 + f64::from(rng.unit()) * 0.4,
        PlotKind::Aggression => AGGRESSION_MIN_RATIO + f64::from(rng.unit()) * 1.0,
        PlotKind::SpectralShape | PlotKind::Energy | PlotKind::Bass => {
            2.1 + f64::from(rng.unit()) * 0.8
        }
    };
    PlotSpec {
        kind,
        ratio,
        cluster: None,
    }
}

fn mixed_cluster(count: usize, rng: &mut Rng) -> Cluster {
    match count {
        2 => [Cluster::Row, Cluster::Column, Cluster::Stepped][rng.range(3)],
        3 => [
            Cluster::Sidecar,
            Cluster::Sidecar,
            Cluster::TwoOverOne,
            Cluster::TwoBesideOne,
            Cluster::Stepped,
        ][rng.range(5)],
        _ => [
            Cluster::Compound,
            Cluster::Compound,
            Cluster::Sidecar,
            Cluster::Stepped,
        ][rng.range(4)],
    }
}

fn apply_cluster(specs: &mut [PlotSpec], cluster: Cluster) {
    for spec in specs {
        spec.cluster = Some(cluster);
    }
}

impl GraphCompositions {
    /// Cancels scene content and disarms the trigger baseline. Used when the
    /// interactive analyzer is open, so its hits are not replayed on closing.
    pub(super) fn clear(&mut self) {
        self.groups.clear();
        self.last_triggers = None;
        self.last_audio_time = None;
        self.mode_entry_pending = false;
        self.context = None;
        self.viewport = None;
        self.cooldown = INITIAL_COOLDOWN;
    }

    pub(super) fn update(
        &mut self,
        dt: f32,
        visualizer: &Visualizer,
        fonts: &mut Fonts,
        stage: &Stage,
        rng: &mut Rng,
        occluders: &[Rect],
    ) {
        let dt = dt.max(0.0);
        self.cooldown = (self.cooldown - dt).max(0.0);
        let mode = visualizer.showcase_mode();
        let mode_changed = self.last_mode != Some(mode);
        if mode_changed {
            self.last_mode = Some(mode);
            self.mode_entry_pending = rng.unit() < MODE_ENTRY_SPAWN_CHANCE;
        }
        let counts = visualizer.trigger_counts();
        let audio_time = visualizer.percussion_audio_time();
        let timeline_reset = self.last_audio_time.is_some_and(|last| audio_time < last)
            || self.last_triggers.is_some_and(|last| {
                counts.kick < last.kick
                    || counts.snare < last.snare
                    || counts.hat < last.hat
                    || counts.drop < last.drop
            });
        let epochs_changed = self.context.as_ref().is_some_and(|context| {
            context.percussion_epochs != Context::percussion_epochs(visualizer)
        });
        let events = Events::between(self.last_triggers, counts);
        self.last_triggers = Some(counts);
        self.last_audio_time = Some(audio_time);
        let context_changed = timeline_reset
            || self
                .context
                .as_ref()
                .is_some_and(|context| !context.matches(visualizer));
        if context_changed && !mode_changed {
            self.mode_entry_pending = false;
        }
        if self.context.is_none() || context_changed {
            self.context = Some(Context::capture(visualizer));
        }

        if !self.groups.is_empty() {
            let active_horizontal = self
                .groups
                .iter()
                .flat_map(|group| &group.cards)
                .any(|card| !contains(stage.left, card.rect) && !contains(stage.right, card.rect));
            let model = active_horizontal
                .then(|| model_bounds(visualizer, stage))
                .flatten();
            let resized = self.viewport != Some((stage.width, stage.height));
            if resized {
                let mut occupied = occluders.to_vec();
                let mut resized_groups = Vec::new();
                for mut group in self.groups.drain(..) {
                    let specs: Vec<_> = group.cards.iter().map(|card| card.spec).collect();
                    let lanes = if specs
                        .first()
                        .is_some_and(|spec| spec.cluster == Some(Cluster::Row))
                    {
                        horizontal_lanes(visualizer, stage)
                    } else {
                        Vec::new()
                    };
                    if let Some(rects) = arrange_in(&specs, stage, &occupied, rng, &lanes) {
                        for (card, rect) in group.cards.iter_mut().zip(rects) {
                            card.rect = rect;
                        }
                        occupied.extend(group_footprints(&group).into_iter().map(|bounds| {
                            bounds.inflate(
                                stage.px(FORMATION_AIR - 10.0),
                                stage.px(FORMATION_AIR - 10.0),
                            )
                        }));
                        resized_groups.push(group);
                    }
                }
                // Remove only complete groups that no longer fit, rather
                // than drawing their old coordinates outside the viewport.
                self.groups = resized_groups;
                self.viewport = Some((stage.width, stage.height));
            }
            for group in &mut self.groups {
                // Keep invalidated cards on their original feature scale
                // through the dissolve, even if a later group starts fresh.
                group.data_frozen |= timeline_reset || epochs_changed;
                let collision = group.cards.iter().any(|card| {
                    occluders
                        .iter()
                        .any(|other| overlaps(card.rect, *other, stage.px(6.0)))
                        || (!contains(stage.left, card.rect)
                            && !contains(stage.right, card.rect)
                            && model
                                .is_some_and(|bounds| overlaps(card.rect, bounds, stage.px(6.0))))
                });
                for card in &mut group.cards {
                    if !group.data_frozen {
                        card.plot.update(visualizer);
                    }
                    card.pulse *= (-dt / 0.15).exp();
                    if !group.life.exiting() && matching_event(card.spec.kind, events) {
                        card.pulse = 1.0;
                    }
                }
                group
                    .life
                    .advance(dt, events, context_changed || collision, rng);
            }
            self.groups.retain(|group| !group.life.finished());
        }

        // A mode entry may wait for the brief placement cooldown, but consumes
        // its one attempt even when capacity or occupied space prevents a spawn.
        let mode_entry = self.mode_entry_pending && self.cooldown == 0.0 && !context_changed;
        if mode_entry {
            self.mode_entry_pending = false;
        }
        if context_changed
            || self.groups.len() >= MAX_GROUPS
            || self.plot_count() >= MAX_PLOTS
            || self.cooldown > 0.0
            || (!mode_entry && (!events.any() || rng.unit() >= events.spawn_chance()))
        {
            return;
        }
        let family = Family::choose(rng, self.previous_family);
        let available_attacks = std::array::from_fn(|channel| {
            let epoch = visualizer.percussion_channel_status(channel).epoch;
            visualizer.percussion_decisions().iter().any(|event| {
                event.channel == channel && event.epoch == epoch && event.envelope_len > 1
            })
        });
        let mut specs = choose_specs(family, rng, events, available_attacks);
        let original_count = specs.len();
        specs.truncate(MAX_PLOTS - self.plot_count());
        if specs.len() != original_count {
            for spec in &mut specs {
                spec.cluster = None;
            }
        }
        if self.spawn(specs, visualizer, fonts, stage, rng, occluders) {
            self.previous_family = Some(family);
        } else {
            // A fully occupied scene should not run the placement search on
            // every high-frequency hat event.
            self.cooldown = 0.35;
        }
    }

    fn spawn(
        &mut self,
        specs: Vec<PlotSpec>,
        visualizer: &Visualizer,
        fonts: &mut Fonts,
        stage: &Stage,
        rng: &mut Rng,
        occluders: &[Rect],
    ) -> bool {
        if specs.is_empty()
            || self.groups.len() >= MAX_GROUPS
            || self.plot_count() + specs.len() > MAX_PLOTS
        {
            return false;
        }
        let mut occupied = occluders.to_vec();
        occupied.extend(self.groups.iter().flat_map(group_footprints).map(|bounds| {
            bounds.inflate(
                stage.px(FORMATION_AIR - 10.0),
                stage.px(FORMATION_AIR - 10.0),
            )
        }));
        let lanes = if specs
            .first()
            .is_some_and(|spec| spec.cluster == Some(Cluster::Row))
        {
            horizontal_lanes(visualizer, stage)
        } else {
            Vec::new()
        };
        let Some(rects) = arrange_in(&specs, stage, &occupied, rng, &lanes) else {
            return false;
        };
        let life = Lifetime::new(specs.len(), rng);
        self.viewport = Some((stage.width, stage.height));
        let cards = specs
            .into_iter()
            .zip(rects)
            .map(|(spec, rect)| GraphCard {
                plot: ScenePlot::new(spec.kind, visualizer, fonts),
                spec,
                rect,
                seed: rng.next(),
                pulse: 0.0,
            })
            .collect();
        self.groups.push(GraphGroup {
            cards,
            life,
            data_frozen: false,
        });
        self.cooldown = 0.8 + rng.unit() * 0.8;
        true
    }

    pub(super) fn draw(&self, scene: &mut Scene, stage: &Stage, palette: ScenePalette) {
        for group in &self.groups {
            for (index, card) in group.cards.iter().enumerate() {
                let (reveal, alpha) =
                    graph_animation(card.spec.kind, &group.life, index, card.seed);
                card.plot
                    .draw(scene, card.rect, stage, palette, reveal, alpha, card.seed);
                draw_beat_accent(scene, card, stage, palette, reveal * alpha);
            }
        }
    }

    /// Reserve outgoing bounds through their last animation frame so normal
    /// text does not appear underneath a still-visible dissolving chart.
    pub(super) fn occluders(&self) -> Vec<Rect> {
        self.groups
            .iter()
            .flat_map(|group| &group.cards)
            .map(|card| card.rect)
            .collect()
    }

    fn plot_count(&self) -> usize {
        self.groups.iter().map(|group| group.cards.len()).sum()
    }

    #[cfg(test)]
    pub(super) fn preview_spawn(
        &mut self,
        visualizer: &Visualizer,
        fonts: &mut Fonts,
        stage: &Stage,
        rng: &mut Rng,
        occluders: &[Rect],
    ) {
        self.clear();
        let choice = if stage.aspect > 2.0 {
            4
        } else {
            (rng.range(5) + rng.range(5)) % 5
        };
        let (cluster, ratios) = match choice {
            0 => (None, [1.0, 2.0, 0.5]),
            1 => (Some(Cluster::Column), [1.0, 2.0, 1.0]),
            2 => (Some(Cluster::TwoOverOne), [1.0, 1.0, 2.0]),
            3 => (Some(Cluster::TwoBesideOne), [1.0, 1.0, 0.5]),
            _ => (Some(Cluster::Row), [1.0, 2.0, 1.0]),
        };
        let specs = ratios
            .into_iter()
            .enumerate()
            .map(|(channel, ratio)| PlotSpec {
                kind: PlotKind::Heatmap(channel),
                ratio,
                cluster,
            })
            .collect();
        if self.spawn(specs, visualizer, fonts, stage, rng, occluders) {
            if let Some(group) = self.groups.last_mut() {
                let life = &mut group.life;
                life.age = life.entrance + GROUP_STAGGER * (life.count - 1) as f32;
            }
        }
    }

    #[cfg(test)]
    pub(super) fn preview_formation(
        &mut self,
        visualizer: &Visualizer,
        fonts: &mut Fonts,
        stage: &Stage,
        rng: &mut Rng,
        occluders: &[Rect],
        preview: FormationPreview,
    ) -> usize {
        self.clear();
        let (cluster, kinds): (Cluster, Vec<_>) = match preview {
            FormationPreview::Mixed => (
                Cluster::Column,
                vec![
                    (PlotKind::Heatmap(0), 1.0),
                    (PlotKind::Attack(0), 2.5),
                    (PlotKind::Onsets, 3.0),
                ],
            ),
            FormationPreview::Sidecar => (
                Cluster::Sidecar,
                vec![
                    (PlotKind::Heatmap(0), 1.0),
                    (PlotKind::Aggression, AGGRESSION_MIN_RATIO),
                    (PlotKind::Energy, 2.4),
                ],
            ),
            FormationPreview::Stepped => (
                Cluster::Stepped,
                vec![
                    (PlotKind::SpectralShape, 2.4),
                    (PlotKind::Aggression, AGGRESSION_MIN_RATIO),
                    (PlotKind::Energy, 2.8),
                    (PlotKind::Bass, 2.2),
                ],
            ),
            FormationPreview::Split => (
                Cluster::Compound,
                vec![
                    (PlotKind::Heatmap(0), 1.0),
                    (PlotKind::Attack(0), 2.5),
                    (PlotKind::SpectralShape, 2.4),
                    (PlotKind::Energy, 2.8),
                ],
            ),
        };
        let specs = kinds
            .into_iter()
            .map(|(kind, ratio)| PlotSpec {
                kind,
                ratio,
                cluster: Some(cluster),
            })
            .collect();
        if self.spawn(specs, visualizer, fonts, stage, rng, occluders) {
            for group in &mut self.groups {
                group.life.age =
                    group.life.entrance + GROUP_STAGGER * (group.cards.len() - 1) as f32;
            }
        }
        self.plot_count()
    }
}

/// Search a small set of aligned side-column compositions. Width and position
/// are randomized once; later frames only update graph data and animations.
#[cfg(test)]
fn arrange(
    specs: &[PlotSpec],
    stage: &Stage,
    occluders: &[Rect],
    rng: &mut Rng,
) -> Option<Vec<Rect>> {
    arrange_in(specs, stage, occluders, rng, &[])
}

fn arrange_in(
    specs: &[PlotSpec],
    stage: &Stage,
    occluders: &[Rect],
    rng: &mut Rng,
    horizontal_lanes: &[Rect],
) -> Option<Vec<Rect>> {
    if specs.is_empty() {
        return Some(Vec::new());
    }
    let preferred = specs[0].cluster.unwrap_or(Cluster::Column);
    let mut alternatives = vec![
        preferred,
        Cluster::Column,
        Cluster::Row,
        Cluster::Stepped,
        Cluster::Sidecar,
        Cluster::TwoOverOne,
        Cluster::TwoBesideOne,
        Cluster::Compound,
    ];
    let mut tried = Vec::new();
    alternatives.retain(|candidate| {
        if tried.contains(candidate) {
            false
        } else {
            tried.push(*candidate);
            true
        }
    });
    for candidate in alternatives {
        if let Some(rects) = arrange_cluster(
            specs,
            candidate,
            &[stage.left, stage.right],
            stage,
            occluders,
            rng,
        ) {
            return Some(rects);
        }
        if candidate == Cluster::Row {
            if let Some(rects) =
                arrange_cluster(specs, candidate, horizontal_lanes, stage, occluders, rng)
            {
                return Some(rects);
            }
        }
    }
    // Every fallback still uses declared tracks and spans. An occupied or
    // undersized scene can wait for another onset instead of scattering cards.
    None
}

fn arrange_cluster(
    specs: &[PlotSpec],
    cluster: Cluster,
    regions: &[Rect],
    stage: &Stage,
    occluders: &[Rect],
    rng: &mut Rng,
) -> Option<Vec<Rect>> {
    if !(1..=4).contains(&specs.len()) {
        return None;
    }
    let mut candidates = Vec::new();
    for region in regions {
        let width = region.width() / stage.unit;
        let height = region.height() / stage.unit;
        let minimum = cluster_minimum(specs, cluster);
        let Some(minimum_rects) = cluster_geometry(specs, cluster, minimum) else {
            continue;
        };
        let fits = |rects: &[Rect]| {
            let bounds = footprint(rects);
            bounds.width() <= width + 0.001
                && bounds.height() <= height + 0.001
                && rects.iter().zip(specs).all(|(rect, spec)| {
                    rect.width() - PLOT_GUTTER_WIDTH >= MIN_CHART_WIDTH - 0.001
                        && rect.height() - PLOT_GUTTER_HEIGHT
                            >= minimum_chart_height(spec.kind) - 0.001
                        && (spec.kind != PlotKind::Aggression
                            || rect.height() - PLOT_GUTTER_HEIGHT <= 32.0 + 0.001)
                })
        };
        if !fits(&minimum_rects) {
            continue;
        }
        let mut lower = minimum;
        let mut upper = minimum + 500.0;
        for _ in 0..16 {
            let middle = (lower + upper) * 0.5;
            if fits(&cluster_geometry(specs, cluster, middle).unwrap()) {
                lower = middle;
            } else {
                upper = middle;
            }
        }
        let maximum = lower;
        for _ in 0..8 {
            let size = minimum + f64::from(rng.unit()) * (maximum - minimum).min(95.0);
            let local = cluster_geometry(specs, cluster, size).unwrap();
            let bounds = footprint(&local);
            let scaled_width = stage.px(bounds.width());
            let scaled_height = stage.px(bounds.height());
            // Shared exterior anchors also align separate formations that
            // happen to occupy the same side column. Wide lanes use the same
            // viewport edge anchors instead of centering each group alone.
            let xs = if region.x1 <= stage.width * 0.5 {
                vec![region.x0]
            } else if region.x0 >= stage.width * 0.5 {
                vec![region.x1 - scaled_width]
            } else {
                vec![region.x0, region.x1 - scaled_width]
            };
            let mut ys = vec![region.y0, region.y1 - scaled_height];
            for other in occluders {
                if other.x1 >= region.x0 && other.x0 <= region.x1 {
                    ys.extend([
                        other.y1 + stage.px(10.0),
                        other.y0 - scaled_height - stage.px(10.0),
                    ]);
                }
            }
            for x in xs {
                for &y in &ys {
                    let rects: Vec<_> = local
                        .iter()
                        .map(|rect| {
                            Rect::new(
                                x + stage.px(rect.x0),
                                y + stage.px(rect.y0),
                                x + stage.px(rect.x1),
                                y + stage.px(rect.y1),
                            )
                        })
                        .collect();
                    if rects.iter().all(|rect| contains(*region, *rect))
                        && occluders
                            .iter()
                            .all(|other| !overlaps(footprint(&rects), *other, stage.px(10.0)))
                    {
                        candidates.push(rects);
                    }
                }
            }
        }
    }
    if candidates.is_empty() {
        None
    } else {
        Some(candidates.swap_remove(rng.range(candidates.len())))
    }
}

fn footprint(rects: &[Rect]) -> Rect {
    rects
        .iter()
        .copied()
        .reduce(|a, b| a.union(b))
        .unwrap_or(Rect::ZERO)
}

fn group_bounds(group: &GraphGroup) -> Rect {
    let rects: Vec<_> = group.cards.iter().map(|card| card.rect).collect();
    footprint(&rects)
}

fn group_footprints(group: &GraphGroup) -> Vec<Rect> {
    vec![group_bounds(group)]
}

fn minimum_chart_height(kind: PlotKind) -> f64 {
    match kind {
        PlotKind::Attack(_) => 70.0,
        PlotKind::Onsets => 60.0,
        PlotKind::Aggression => 20.0,
        PlotKind::SpectralShape | PlotKind::Energy | PlotKind::Bass => 62.0,
        PlotKind::Heatmap(_) => 0.0,
    }
}

fn cluster_minimum(specs: &[PlotSpec], cluster: Cluster) -> f64 {
    let mut minimum = if matches!(cluster, Cluster::Row | Cluster::TwoOverOne) {
        let count = if cluster == Cluster::TwoOverOne {
            2
        } else {
            specs.len()
        };
        specs[..count.min(specs.len())]
            .iter()
            .map(|spec| (MIN_CHART_WIDTH / spec.ratio).max(minimum_chart_height(spec.kind)))
            .fold(0.0, f64::max)
    } else {
        MIN_CHART_WIDTH
    };
    // A spanning chart can be narrower than the base track. Raise the shared
    // size until every chart has real room, then validate it again when fitting.
    for _ in 0..64 {
        let Some(rects) = cluster_geometry(specs, cluster, minimum) else {
            break;
        };
        if rects.iter().zip(specs).all(|(rect, spec)| {
            rect.width() - PLOT_GUTTER_WIDTH >= MIN_CHART_WIDTH - 0.001
                && rect.height() - PLOT_GUTTER_HEIGHT >= minimum_chart_height(spec.kind) - 0.001
        }) {
            break;
        }
        minimum *= 1.04;
    }
    minimum
}

/// One logical grid axis. Intrinsic chart dimensions determine track sizes;
/// every origin and span endpoint comes from these shared track boundaries.
struct GridAxis {
    starts: Vec<f64>,
    ends: Vec<f64>,
}

impl GridAxis {
    fn new(sizes: &[f64], gap: f64) -> Self {
        let mut starts = Vec::with_capacity(sizes.len());
        let mut ends = Vec::with_capacity(sizes.len());
        let mut cursor = 0.0;
        for &size in sizes {
            starts.push(cursor);
            ends.push(cursor + size);
            cursor += size + gap;
        }
        Self { starts, ends }
    }

    fn extent(&self) -> f64 {
        self.ends.last().copied().unwrap_or(0.0)
    }

    fn span(&self, first: usize, last: usize) -> (f64, f64) {
        (self.starts[first], self.ends[last])
    }
}

fn grid_rect(columns: &GridAxis, x: (usize, usize), rows: &GridAxis, y: (usize, usize)) -> Rect {
    let (x0, x1) = columns.span(x.0, x.1);
    let (y0, y1) = rows.span(y.0, y.1);
    Rect::new(x0, y0, x1, y1)
}

fn cluster_geometry(specs: &[PlotSpec], cluster: Cluster, size: f64) -> Option<Vec<Rect>> {
    let gap = CARD_GAP;
    let heights = |items: &[PlotSpec]| {
        items
            .iter()
            .map(|spec| size / spec.ratio + PLOT_GUTTER_HEIGHT)
            .collect::<Vec<_>>()
    };
    let mut rects = Vec::new();
    match cluster {
        Cluster::Row => {
            let widths: Vec<_> = specs
                .iter()
                .map(|spec| size * spec.ratio + PLOT_GUTTER_WIDTH)
                .collect();
            let columns = GridAxis::new(&widths, gap);
            let rows = GridAxis::new(&[size + PLOT_GUTTER_HEIGHT], gap);
            for index in 0..specs.len() {
                rects.push(grid_rect(&columns, (index, index), &rows, (0, 0)));
            }
        }
        Cluster::Column => {
            let columns = GridAxis::new(&[size + PLOT_GUTTER_WIDTH], gap);
            let rows = GridAxis::new(&heights(specs), gap);
            for index in 0..specs.len() {
                rects.push(grid_rect(&columns, (0, 0), &rows, (index, index)));
            }
        }
        Cluster::TwoOverOne => {
            if specs.len() != 3 {
                return None;
            }
            let columns = GridAxis::new(
                &[
                    size * specs[0].ratio + PLOT_GUTTER_WIDTH,
                    size * specs[1].ratio + PLOT_GUTTER_WIDTH,
                ],
                gap,
            );
            let rows = GridAxis::new(
                &[
                    size + PLOT_GUTTER_HEIGHT,
                    (columns.extent() - PLOT_GUTTER_WIDTH) / specs[2].ratio + PLOT_GUTTER_HEIGHT,
                ],
                gap,
            );
            rects.extend([
                grid_rect(&columns, (0, 0), &rows, (0, 0)),
                grid_rect(&columns, (1, 1), &rows, (0, 0)),
                grid_rect(&columns, (0, 1), &rows, (1, 1)),
            ]);
        }
        Cluster::TwoBesideOne => {
            if specs.len() != 3 {
                return None;
            }
            let rows = GridAxis::new(&heights(&specs[..2]), gap);
            let columns = GridAxis::new(
                &[
                    size + PLOT_GUTTER_WIDTH,
                    (rows.extent() - PLOT_GUTTER_HEIGHT) * specs[2].ratio + PLOT_GUTTER_WIDTH,
                ],
                gap,
            );
            rects.extend([
                grid_rect(&columns, (0, 0), &rows, (0, 0)),
                grid_rect(&columns, (0, 0), &rows, (1, 1)),
                grid_rect(&columns, (1, 1), &rows, (0, 1)),
            ]);
        }
        Cluster::Sidecar => {
            if specs.len() < 2 {
                return None;
            }
            let rows = GridAxis::new(&heights(&specs[1..]), gap);
            let hero_width = (rows.extent() - PLOT_GUTTER_HEIGHT) * specs[0].ratio;
            let columns = GridAxis::new(
                &[hero_width + PLOT_GUTTER_WIDTH, size + PLOT_GUTTER_WIDTH],
                gap,
            );
            rects.push(grid_rect(&columns, (0, 0), &rows, (0, specs.len() - 2)));
            for index in 1..specs.len() {
                rects.push(grid_rect(&columns, (1, 1), &rows, (index - 1, index - 1)));
            }
        }
        Cluster::Stepped => {
            let track_width = (size + PLOT_GUTTER_WIDTH - 2.0 * gap) / 3.0;
            let columns = GridAxis::new(&[track_width; 4], gap);
            let rows = GridAxis::new(&heights(specs), gap);
            // Each panel spans three tracks, then steps by one track. This
            // keeps the stagger compact while every edge remains on the grid.
            for index in 0..specs.len() {
                let column = index % 2;
                rects.push(grid_rect(
                    &columns,
                    (column, column + 2),
                    &rows,
                    (index, index),
                ));
            }
        }
        Cluster::Compound => {
            if specs.len() != 4 {
                return None;
            }
            let columns = GridAxis::new(&[size + PLOT_GUTTER_WIDTH; 2], FORMATION_AIR);
            let left_rows = GridAxis::new(&heights(&specs[..2]), gap);
            let right_rows = GridAxis::new(&heights(&specs[2..]), gap);
            // Two top-aligned subgrids share their column widths. Their row
            // tracks follow their own charts, separated by the same 5-unit
            // internal gutter; the 36-unit column gutter separates the kits.
            rects.extend([
                grid_rect(&columns, (0, 0), &left_rows, (0, 0)),
                grid_rect(&columns, (0, 0), &left_rows, (1, 1)),
                grid_rect(&columns, (1, 1), &right_rows, (0, 0)),
                grid_rect(&columns, (1, 1), &right_rows, (1, 1)),
            ]);
        }
    }
    Some(rects)
}

/// Wide rows can use the air above or below the wireframe when its actual
/// swept support leaves enough room. Side-only groups never pay this scan.
fn horizontal_lanes(visualizer: &Visualizer, stage: &Stage) -> Vec<Rect> {
    let Some(model) = model_bounds(visualizer, stage) else {
        return Vec::new();
    };
    let mut lanes = Vec::new();
    let gap = stage.px(12.0);
    for rect in [
        Rect::new(
            stage.margin,
            stage.margin,
            stage.width - stage.margin,
            model.y0 - gap,
        ),
        Rect::new(
            stage.margin,
            model.y1 + gap,
            stage.width - stage.margin,
            stage.height - stage.margin,
        ),
    ] {
        if rect.height() >= stage.px(138.0) && rect.width() > stage.px(230.0) {
            lanes.push(rect);
        }
    }
    lanes
}

fn model_bounds(visualizer: &Visualizer, stage: &Stage) -> Option<Rect> {
    let frame = visualizer.frame();
    let mut bounds: Option<Rect> = None;
    let mut include = |x: f32, y: f32, padding: f64| {
        if x.is_finite() && y.is_finite() {
            let point = Rect::new(
                f64::from(x) - padding,
                f64::from(y) - padding,
                f64::from(x) + padding,
                f64::from(y) + padding,
            );
            bounds = Some(bounds.map_or(point, |old| old.union(point)));
        }
    };
    for line in frame.model_lines {
        if line.start_color[3].max(line.end_color[3]) <= 0.0 {
            continue;
        }
        let padding = f64::from(line.style[0].max(1.0)) * 0.5 + 1.5;
        include(line.endpoints[0], line.endpoints[1], padding);
        include(line.endpoints[2], line.endpoints[3], padding);
    }
    for ribbon in frame.ribbons {
        if ribbon.start_color[3]
            .max(ribbon.end_color[3])
            .max(ribbon.third_color[3])
            <= 0.0
        {
            continue;
        }
        include(ribbon.start[0], ribbon.start[1], 0.0);
        include(ribbon.start[2], ribbon.start[3], 0.0);
        include(ribbon.end[0], ribbon.end[1], 0.0);
    }
    bounds.map(|rect| rect.inflate(stage.px(24.0), stage.px(24.0)))
}

fn matching_event(kind: PlotKind, events: Events) -> bool {
    match kind {
        PlotKind::Heatmap(channel) | PlotKind::Attack(channel) => match channel {
            0 => events.kick,
            1 => events.snare,
            2 => events.hat,
            _ => false,
        },
        PlotKind::Bass => events.kick,
        PlotKind::SpectralShape => events.drop,
        PlotKind::Aggression | PlotKind::Energy => events.kick || events.snare,
        PlotKind::Onsets => events.any(),
    }
}

fn draw_beat_accent(
    scene: &mut Scene,
    card: &GraphCard,
    stage: &Stage,
    palette: ScenePalette,
    alpha: f32,
) {
    let opacity = card.pulse * alpha * 0.65;
    if opacity < 0.005 {
        return;
    }
    let bounds = plot_bounds(card.rect, stage);
    let length = stage.px(7.0 + 3.0 * f64::from(card.pulse));
    let color = palette.accent.with_alpha(opacity);
    for (corner, direction) in [
        (Point::new(bounds.x0, bounds.y0), 1.0),
        (Point::new(bounds.x1, bounds.y1), -1.0),
    ] {
        for end in [
            Point::new(corner.x + direction * length, corner.y),
            Point::new(corner.x, corner.y + direction * length),
        ] {
            scene.stroke(
                &Stroke::new(stage.px(1.8)),
                Affine::IDENTITY,
                color,
                None,
                &Line::new(corner, end),
            );
        }
    }
}

fn shuffle<T>(values: &mut [T], rng: &mut Rng) {
    for index in (1..values.len()).rev() {
        values.swap(index, rng.range(index + 1));
    }
}

fn contains(outer: Rect, inner: Rect) -> bool {
    inner.x0 >= outer.x0 - 0.001
        && inner.y0 >= outer.y0 - 0.001
        && inner.x1 <= outer.x1 + 0.001
        && inner.y1 <= outer.y1 + 0.001
}

fn overlaps(a: Rect, b: Rect, padding: f64) -> bool {
    a.x0 < b.x1 + padding && a.x1 > b.x0 - padding && a.y0 < b.y1 + padding && a.y1 > b.y0 - padding
}

fn smoothstep(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Size;

    fn poi_visualizer() -> Visualizer {
        let mut visualizer = Visualizer::new();
        visualizer.load_models();
        for _ in 0..64 {
            if visualizer.model_id() == Some("rif_ak47") {
                break;
            }
            visualizer.next_model();
        }
        assert_eq!(visualizer.model_id(), Some("rif_ak47"));
        visualizer
    }

    #[test]
    fn mode_entries_can_spawn_without_detector_hits_after_the_brief_cooldown() -> anyhow::Result<()>
    {
        let mut visualizer = poi_visualizer();
        let mut fonts = Fonts::new()?;
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut mode_rng = Rng::new(7);
        let mut graphs = GraphCompositions::default();
        let mut rng = Rng::new(1);
        graphs.update(0.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(graphs.groups.is_empty());
        graphs.update(0.2, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(graphs.groups.is_empty());
        graphs.update(0.2, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert_eq!(
            graphs.groups.len(),
            1,
            "normal entry can introduce graphs without a hit"
        );

        // Let the original group retire normally before testing each next entry.
        for mode in [ShowcaseMode::Poi, ShowcaseMode::Normal] {
            graphs.update(106.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
            graphs.update(1.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
            assert!(graphs.groups.is_empty());
            match mode {
                ShowcaseMode::Poi => assert!(visualizer.showcase.enter_focus(&mut mode_rng, 1.1)),
                ShowcaseMode::Normal => visualizer.showcase.exit_focus(),
            }
            rng = Rng::new(1);
            graphs.update(0.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
            assert_eq!(
                graphs.groups.len(),
                1,
                "each normal/POI entry gets its own opportunity"
            );
        }
        assert_eq!(visualizer.trigger_counts(), TriggerCounts::default());
        Ok(())
    }

    #[test]
    fn a_mode_can_start_empty_and_add_graphs_on_a_later_hit() -> anyhow::Result<()> {
        let mut visualizer = Visualizer::new();
        let mut fonts = Fonts::new()?;
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut graphs = GraphCompositions::default();
        let mut rng = Rng::new(0);
        graphs.update(
            INITIAL_COOLDOWN,
            &visualizer,
            &mut fonts,
            &stage,
            &mut rng,
            &[],
        );
        assert!(
            graphs.groups.is_empty(),
            "zero graphs is a valid entry decision"
        );
        for _ in 0..8 {
            graphs.update(0.5, &visualizer, &mut fonts, &stage, &mut rng, &[]);
            assert!(
                graphs.groups.is_empty(),
                "an empty decision is not retried every frame"
            );
        }
        visualizer.kick_hits += 1;
        rng = Rng::new(1);
        graphs.update(0.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert_eq!(graphs.groups.len(), 1, "later triggers may add graphs");
        Ok(())
    }

    #[test]
    fn scene_selector_and_poi_target_changes_do_not_create_entry_opportunities(
    ) -> anyhow::Result<()> {
        let mut visualizer = poi_visualizer();
        let mut fonts = Fonts::new()?;
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut graphs = GraphCompositions::default();
        let mut rng = Rng::new(0);
        let mut mode_rng = Rng::new(7);
        graphs.update(
            INITIAL_COOLDOWN,
            &visualizer,
            &mut fonts,
            &stage,
            &mut rng,
            &[],
        );
        assert!(graphs.groups.is_empty());

        for _ in 0..visualizer.showcase.settings().selector_entry_kicks {
            visualizer.showcase.kick(&mut mode_rng, true);
        }
        assert!(visualizer.showcase.selector_active());
        rng = Rng::new(1);
        graphs.update(1.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(
            graphs.groups.is_empty(),
            "selector entry is still normal mode"
        );
        visualizer.showcase.drop();
        assert!(!visualizer.showcase.selector_active());
        graphs.update(1.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(graphs.groups.is_empty());

        assert!(visualizer.showcase.enter_focus(&mut mode_rng, 1.1));
        rng = Rng::new(0);
        graphs.update(1.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(graphs.groups.is_empty());
        let target = visualizer.showcase_target_id().unwrap().to_owned();
        visualizer.showcase.advance(2.0);
        visualizer.showcase.snare(&mut mode_rng, 32, 32, 1.1);
        assert_ne!(visualizer.showcase_target_id(), Some(target.as_str()));
        rng = Rng::new(1);
        graphs.update(1.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(graphs.groups.is_empty(), "another POI stays in POI mode");

        visualizer.showcase.exit_focus();
        rng = Rng::new(0);
        graphs.update(1.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        visualizer.next_model();
        rng = Rng::new(1);
        graphs.update(1.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        graphs.update(1.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(
            graphs.groups.is_empty(),
            "model navigation does not enter a mode again"
        );
        Ok(())
    }

    #[test]
    fn blocked_mode_entry_is_consumed_without_a_deferred_spawn() -> anyhow::Result<()> {
        let visualizer = Visualizer::new();
        let mut fonts = Fonts::new()?;
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut graphs = GraphCompositions::default();
        let mut rng = Rng::new(1);
        graphs.update(
            INITIAL_COOLDOWN,
            &visualizer,
            &mut fonts,
            &stage,
            &mut rng,
            &[Rect::new(0.0, 0.0, stage.width, stage.height)],
        );
        assert!(graphs.groups.is_empty());
        graphs.update(2.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(
            graphs.groups.is_empty(),
            "clearing space is not another entry opportunity"
        );
        Ok(())
    }

    #[test]
    fn a_scene_change_cancels_a_pending_entry_in_the_same_mode() -> anyhow::Result<()> {
        let mut visualizer = poi_visualizer();
        let mut fonts = Fonts::new()?;
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut graphs = GraphCompositions::default();
        let mut rng = Rng::new(1);
        graphs.update(0.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(graphs.groups.is_empty());

        visualizer.next_model();
        assert_eq!(visualizer.showcase_mode(), ShowcaseMode::Normal);
        graphs.update(0.1, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        graphs.update(0.5, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(
            graphs.groups.is_empty(),
            "an entry queued for the previous model must not appear on a new scene"
        );
        Ok(())
    }

    #[test]
    fn a_real_mode_change_keeps_its_entry_when_the_model_also_changes() -> anyhow::Result<()> {
        let mut visualizer = poi_visualizer();
        let mut fonts = Fonts::new()?;
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut graphs = GraphCompositions::default();
        let mut rng = Rng::new(0);
        graphs.update(
            INITIAL_COOLDOWN,
            &visualizer,
            &mut fonts,
            &stage,
            &mut rng,
            &[],
        );
        assert!(graphs.groups.is_empty());

        let old_model = visualizer.model_id().unwrap().to_owned();
        visualizer.next_model();
        for _ in 0..64 {
            if visualizer.showcase.poi_count() > 0 {
                break;
            }
            visualizer.next_model();
        }
        assert_ne!(visualizer.model_id(), Some(old_model.as_str()));
        assert!(visualizer.showcase.enter_focus(&mut Rng::new(7), 1.1));
        rng = Rng::new(1);
        graphs.update(0.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        graphs.update(0.5, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert_eq!(
            graphs.groups.len(),
            1,
            "normal-to-POI entry survives simultaneous model replacement"
        );
        assert_eq!(visualizer.trigger_counts(), TriggerCounts::default());
        Ok(())
    }

    fn measured_attacks() -> Visualizer {
        let mut visualizer = Visualizer::new();
        let mut settings = visualizer.detector_settings();
        for channel in &mut settings.percussion.channels {
            channel.cloud_enabled = false;
            channel.cloud_sharpness_enabled = false;
            channel.filters.input_energy = false;
            channel.filters.attack_level = false;
            channel.filters.mix_rms = false;
            channel.filters.output_interval = false;
        }
        visualizer.set_detector_settings(settings);
        let input: Vec<_> = (0..48_000)
            .map(|index| {
                let time = index as f32 / 48_000.0;
                let age = (time - 0.1).rem_euclid(0.3);
                let value =
                    0.6 * (-age / 0.04).exp() * (std::f32::consts::TAU * 120.0 * time).sin();
                [value, -value]
            })
            .collect();
        visualizer.consume_buf(&input, 1.0);
        assert!(visualizer
            .percussion_decisions()
            .iter()
            .any(|event| event.channel == 0 && event.eligible && event.envelope_len > 1));
        visualizer
    }

    fn active_attack_pair(
        visualizer: &Visualizer,
        fonts: &mut Fonts,
        stage: &Stage,
        rng: &mut Rng,
    ) -> GraphCompositions {
        let mut graphs = GraphCompositions {
            last_mode: Some(visualizer.showcase_mode()),
            ..GraphCompositions::default()
        };
        graphs.update(0.0, visualizer, fonts, stage, rng, &[]);
        assert!(graphs.spawn(
            vec![
                PlotSpec {
                    kind: PlotKind::Heatmap(0),
                    ratio: 1.0,
                    cluster: None,
                },
                PlotSpec {
                    kind: PlotKind::Attack(0),
                    ratio: 2.0,
                    cluster: None,
                },
            ],
            visualizer,
            fonts,
            stage,
            rng,
            &[],
        ));
        graphs
    }

    #[test]
    fn static_formations_survive_focus_and_poi_switches_until_the_model_changes(
    ) -> anyhow::Result<()> {
        let mut visualizer = measured_attacks();
        visualizer.load_models();
        for _ in 0..64 {
            if visualizer.model_id() == Some("rif_ak47") {
                break;
            }
            visualizer.next_model();
        }
        assert_eq!(visualizer.model_id(), Some("rif_ak47"));
        let mut fonts = Fonts::new()?;
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut rng = Rng::new(19);
        let mut graphs = active_attack_pair(&visualizer, &mut fonts, &stage, &mut rng);
        let bounds = graphs.occluders();

        assert!(visualizer.showcase.enter_focus(&mut rng, 1.1));
        graphs.update(0.05, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(!graphs.groups[0].life.exiting());
        let first_target = visualizer.showcase_target_id().unwrap().to_owned();
        for _ in 0..120 {
            visualizer.showcase.advance(1.0 / 60.0);
        }
        visualizer.showcase.snare(&mut rng, 32, 32, 1.1);
        assert_ne!(visualizer.showcase_target_id(), Some(first_target.as_str()));
        graphs.update(0.05, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(!graphs.groups[0].life.exiting());
        assert!(!graphs.groups[0].data_frozen);

        visualizer.showcase.exit_focus();
        graphs.update(0.05, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(!graphs.groups[0].life.exiting());
        assert_eq!(
            graphs.groups[0]
                .cards
                .iter()
                .map(|card| card.rect)
                .collect::<Vec<_>>(),
            bounds,
            "existing formations keep their bounds while mode entries may add another group"
        );

        visualizer.next_model();
        graphs.update(0.05, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(graphs.groups[0].life.exiting());
        Ok(())
    }

    #[test]
    fn gain_epoch_retires_the_pair_and_freezes_its_original_feature_scale() -> anyhow::Result<()> {
        let mut visualizer = measured_attacks();
        let mut fonts = Fonts::new()?;
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut rng = Rng::new(91);
        let mut graphs = active_attack_pair(&visualizer, &mut fonts, &stage, &mut rng);
        let epochs = Context::percussion_epochs(&visualizer);
        let now = visualizer.percussion_audio_time();

        visualizer.consume_buf(&[[0.0; 2]; 2400], 1.0);
        graphs.update(0.05, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert_eq!(Context::percussion_epochs(&visualizer), epochs);
        assert!(!graphs.groups[0].data_frozen);
        assert!(!graphs.groups[0].life.exiting());

        visualizer.consume_buf(&[[0.0; 2]; 2400], 0.5);
        assert_ne!(Context::percussion_epochs(&visualizer), epochs);
        assert!(visualizer.percussion_audio_time() > now);
        graphs.update(0.05, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(graphs.groups[0].data_frozen);
        assert!(graphs.groups[0].life.exiting());
        assert_eq!(
            graphs.groups[0].cards.len(),
            2,
            "both cards dissolve as one composition"
        );
        graphs.update(1.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(graphs.groups.is_empty());
        Ok(())
    }

    #[test]
    fn capture_clock_rollback_retires_snapshots_even_when_epochs_repeat() -> anyhow::Result<()> {
        let mut visualizer = measured_attacks();
        let mut fonts = Fonts::new()?;
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut rng = Rng::new(92);
        let mut graphs = active_attack_pair(&visualizer, &mut fonts, &stage, &mut rng);
        let epochs = Context::percussion_epochs(&visualizer);
        visualizer.reset_audio();
        assert_eq!(Context::percussion_epochs(&visualizer), epochs);
        assert!(visualizer.percussion_audio_time() < graphs.last_audio_time.unwrap());
        graphs.update(0.05, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(graphs.groups[0].data_frozen);
        assert!(graphs.groups[0].life.exiting());
        Ok(())
    }

    #[test]
    fn reset_hit_counters_retire_snapshots_after_the_clock_has_caught_up() -> anyhow::Result<()> {
        let mut visualizer = measured_attacks();
        let mut fonts = Fonts::new()?;
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut rng = Rng::new(93);
        let mut graphs = active_attack_pair(&visualizer, &mut fonts, &stage, &mut rng);
        let epochs = Context::percussion_epochs(&visualizer);
        assert!(graphs.last_triggers.unwrap().kick > 0);
        visualizer.reset_audio();
        visualizer.consume_buf(&vec![[0.0; 2]; 48_000], 1.0);
        assert_eq!(Context::percussion_epochs(&visualizer), epochs);
        assert_eq!(
            visualizer.percussion_audio_time(),
            graphs.last_audio_time.unwrap()
        );
        graphs.update(0.05, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(graphs.groups[0].data_frozen);
        assert!(graphs.groups[0].life.exiting());
        Ok(())
    }

    #[test]
    fn event_baseline_does_not_replay_old_or_reset_counts() {
        let current = TriggerCounts {
            kick: 7,
            snare: 3,
            hat: 90,
            drop: 1,
        };
        assert!(!Events::between(None, current).any());
        assert!(!Events::between(Some(current), current).any());
        assert!(!Events::between(Some(current), TriggerCounts::default()).any());
        assert!(
            Events::between(
                Some(current),
                TriggerCounts {
                    snare: 4,
                    ..current
                }
            )
            .snare
        );
        assert!(
            !Events::between(
                Some(current),
                TriggerCounts {
                    snare: 4,
                    ..current
                }
            )
            .kick
        );
    }

    #[test]
    fn readable_dwell_survives_dense_hits_and_silence_has_bounded_lifetime() {
        let mut rng = Rng::new(19);
        let mut life = Lifetime::new(3, &mut rng);
        let dwell = life.dwell_until;
        while life.age + 0.05 < dwell {
            life.advance(
                0.05,
                Events {
                    kick: true,
                    hat: true,
                    ..Events::default()
                },
                false,
                &mut rng,
            );
            assert!(life.exiting_at.is_none());
        }
        life.advance(life.maximum - life.age, Events::default(), false, &mut rng);
        assert!(life.exiting_at.is_some());
        life.advance(1.0, Events::default(), false, &mut rng);
        assert!(life.finished());
    }

    #[test]
    fn retirement_requires_both_readable_time_and_a_real_onset_budget() {
        let mut rng = Rng::new(54);
        let mut life = Lifetime::new(2, &mut rng);
        assert!((48..=96).contains(&life.event_budget));
        assert!((75.0..=105.0).contains(&life.maximum));
        assert!((18.0..=28.0).contains(&(life.dwell_until - life.entrance - GROUP_STAGGER)));
        life.advance(life.dwell_until + 0.1, Events::default(), false, &mut rng);
        assert!(
            !life.exiting(),
            "time alone does not consume the onset budget"
        );
        life.advance(
            0.0,
            Events {
                kick: true,
                event_count: life.event_budget - 1,
                ..Events::default()
            },
            false,
            &mut rng,
        );
        assert!(!life.exiting(), "one required onset still remains");
        life.advance(
            0.0,
            Events {
                hat: true,
                ..Events::default()
            },
            false,
            &mut rng,
        );
        assert!(life.exiting());

        let mut early = Lifetime::new(1, &mut rng);
        early.advance(
            0.5,
            Events {
                drop: true,
                event_count: early.event_budget + 10,
                ..Events::default()
            },
            false,
            &mut rng,
        );
        assert!(!early.exiting(), "even a drop respects the readable dwell");
        early.advance(early.dwell_until, Events::default(), false, &mut rng);
        assert!(
            !early.exiting(),
            "early onsets do not pre-pay the retirement budget"
        );
        early.advance(
            0.0,
            Events {
                kick: true,
                ..Events::default()
            },
            false,
            &mut rng,
        );
        assert!(
            !early.exiting(),
            "one later hit cannot dismiss the formation"
        );
        assert_eq!(early.events_seen, 1);
        early.advance(
            0.0,
            Events {
                hat: true,
                event_count: early.event_budget - 1,
                ..Events::default()
            },
            false,
            &mut rng,
        );
        assert!(early.exiting());
    }

    #[test]
    fn batched_onsets_count_every_hit_and_have_matching_spawn_opportunities() {
        let previous = TriggerCounts {
            kick: 3,
            snare: 2,
            hat: 10,
            drop: 0,
        };
        let events = Events::between(
            Some(previous),
            TriggerCounts {
                kick: 5,
                snare: 3,
                hat: 18,
                drop: 1,
            },
        );
        assert_eq!(events.count(), 12);
        let strong = Events {
            kick: true,
            ..Events::default()
        };
        let hat = Events {
            hat: true,
            ..Events::default()
        };
        assert!(strong.spawn_chance() >= 0.8);
        assert!(hat.spawn_chance() >= 0.35);
        assert!(
            Events {
                hat: true,
                event_count: 3,
                ..Events::default()
            }
            .spawn_chance()
                > 0.7
        );
        assert_eq!(Events::default().spawn_chance(), 0.0);
        assert_eq!(events.spawn_chance(), 1.0);
        assert!(GraphCompositions::default().cooldown < 0.5);
    }

    #[test]
    fn second_composition_enters_while_first_retains_its_own_budget_and_bounds(
    ) -> anyhow::Result<()> {
        let mut visualizer = Visualizer::new();
        let mut fonts = Fonts::new()?;
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut rng = Rng::new(55);
        let mut graphs = GraphCompositions {
            last_mode: Some(visualizer.showcase_mode()),
            ..GraphCompositions::default()
        };
        graphs.update(0.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(graphs.spawn(
            vec![
                PlotSpec {
                    kind: PlotKind::Heatmap(0),
                    ratio: 2.0,
                    cluster: None,
                },
                PlotSpec {
                    kind: PlotKind::Heatmap(1),
                    ratio: 2.0,
                    cluster: None,
                },
            ],
            &visualizer,
            &mut fonts,
            &stage,
            &mut rng,
            &[]
        ));
        let first_rects: Vec<_> = graphs.groups[0]
            .cards
            .iter()
            .map(|card| card.rect)
            .collect();
        for _ in 0..12 {
            visualizer.kick_hits += 1;
            graphs.update(0.25, &visualizer, &mut fonts, &stage, &mut rng, &[]);
            if graphs.groups.len() == 2 {
                break;
            }
        }
        assert_eq!(graphs.groups.len(), 2);
        assert!(graphs.plot_count() <= MAX_PLOTS);
        assert!(!graphs.groups[0].life.exiting());
        assert!(graphs.groups[1].life.age < graphs.groups[0].life.age);
        for card in &graphs.groups[1].cards {
            assert!(first_rects
                .iter()
                .all(|rect| !overlaps(card.rect, *rect, stage.px(6.0))));
        }
        assert_eq!(
            first_rects,
            graphs.groups[0]
                .cards
                .iter()
                .map(|card| card.rect)
                .collect::<Vec<_>>()
        );
        assert!(!graphs.spawn(
            vec![PlotSpec {
                kind: PlotKind::Bass,
                ratio: 2.0,
                cluster: None,
            }],
            &visualizer,
            &mut fonts,
            &stage,
            &mut rng,
            &[]
        ));
        Ok(())
    }

    #[test]
    fn interrupted_entrance_never_resurrects_coverage_and_whole_group_retires() {
        let mut rng = Rng::new(22);
        let mut life = Lifetime::new(3, &mut rng);
        life.age = 0.23;
        let before = life.animation(0).0;
        life.retire();
        life.age += 0.1;
        assert!(life.animation(0).0 <= before);
        life.advance(1.0, Events::default(), false, &mut rng);
        for index in 0..3 {
            assert_eq!(life.animation(index), (0.0, 0.0));
        }
        assert!(life.finished());
    }

    #[test]
    fn static_plots_flicker_as_complete_neon_lamps_and_hold_steady() {
        let mut life = Lifetime::new(1, &mut Rng::new(22));
        for kind in [
            PlotKind::Attack(0),
            PlotKind::SpectralShape,
            PlotKind::Aggression,
            PlotKind::Energy,
            PlotKind::Bass,
            PlotKind::Onsets,
        ] {
            life.exiting_at = None;
            let mut levels = Vec::new();
            for step in 0..=100 {
                life.age = life.entrance * step as f32 / 100.0;
                let animation = graph_animation(kind, &life, 0, 71);
                assert_eq!(animation, graph_animation(kind, &life, 0, 71));
                assert!(
                    [0.0, 1.0].contains(&animation.0),
                    "geometry never dissolves"
                );
                assert!((0.0..=1.0).contains(&animation.1));
                levels.push(animation.1);
            }
            assert_eq!(levels[0], 0.0);
            assert_eq!(*levels.last().unwrap(), 1.0);
            assert!(levels.windows(2).any(|pair| pair[1] < pair[0]));
            assert!(levels.windows(2).any(|pair| pair[1] > pair[0]));
            for age in [life.entrance, life.dwell_until * 0.5, life.dwell_until] {
                life.age = age;
                assert_eq!(graph_animation(kind, &life, 0, 71), (1.0, 1.0));
            }

            life.retire();
            let start = life.age;
            let mut shutdown = Vec::new();
            for step in 0..=100 {
                life.age = start + life.exit * step as f32 / 100.0;
                shutdown.push(graph_animation(kind, &life, 0, 71).1);
            }
            assert_eq!(shutdown[0], 1.0);
            assert_eq!(*shutdown.last().unwrap(), 0.0);
            assert!(
                shutdown.windows(2).any(|pair| pair[1] > pair[0]),
                "shutdown sputters before going dark"
            );
        }
    }

    #[test]
    fn mixed_formation_heatmaps_keep_their_existing_animation() {
        let mut life = Lifetime::new(3, &mut Rng::new(22));
        for exiting_at in [None, Some(0.23), Some(life.dwell_until)] {
            life.exiting_at = exiting_at;
            for step in 0..=200 {
                life.age = step as f32 * 0.2;
                for index in 0..3 {
                    for seed in [0, 71, u64::MAX] {
                        assert_eq!(
                            graph_animation(PlotKind::Heatmap(index), &life, index, seed),
                            life.animation(index)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn interrupted_neon_ignition_cannot_light_during_shutdown() {
        let mut life = Lifetime::new(3, &mut Rng::new(22));
        life.age = GROUP_STAGGER * 1.5;
        let brightness: Vec<_> = (0..3)
            .map(|index| graph_animation(PlotKind::Energy, &life, index, 71).1)
            .collect();
        assert!(brightness[0] > 0.0);
        assert_eq!(brightness[1..], [0.0, 0.0]);
        life.retire();
        let start = life.age;
        for step in 0..=100 {
            life.age = start + (life.exit + GROUP_STAGGER * 2.0) * step as f32 / 100.0;
            for (index, before) in brightness.iter().enumerate() {
                let (_, alpha) = graph_animation(PlotKind::Energy, &life, index, 71);
                assert!(
                    alpha <= *before,
                    "an interrupted lamp cannot ignite further"
                );
            }
        }
        life.age += 1.0;
        for index in 0..3 {
            assert_eq!(
                graph_animation(PlotKind::Energy, &life, index, 71),
                (0.0, 0.0)
            );
        }
    }

    #[test]
    fn random_heatmaps_have_distinct_channels_and_all_counts_and_shapes() {
        let mut rng = Rng::new(9);
        let mut counts = [false; 3];
        let mut shapes = [false; 3];
        for _ in 0..200 {
            let specs = choose_specs(Family::Heatmaps, &mut rng, Events::default(), [true; 3]);
            let mut channels = Vec::new();
            for spec in specs {
                let PlotKind::Heatmap(channel) = spec.kind else {
                    continue;
                };
                assert!(!channels.contains(&channel));
                channels.push(channel);
                shapes[if spec.ratio == 0.5 {
                    0
                } else if spec.ratio == 1.0 {
                    1
                } else {
                    2
                }] = true;
            }
            counts[channels.len() - 1] = true;
        }
        assert!(counts.into_iter().all(|seen| seen));
        assert!(shapes.into_iter().all(|seen| seen));
    }

    #[test]
    fn semantic_kits_include_mixed_and_non_heatmap_formations() {
        let mut rng = Rng::new(73);
        let mut mixed = false;
        let mut compound = false;
        for family in [
            Family::Heatmaps,
            Family::Attack,
            Family::SpectralShape,
            Family::Aggression,
            Family::Energy,
            Family::Bass,
            Family::Onsets,
        ] {
            for _ in 0..40 {
                let specs = choose_specs(family, &mut rng, Events::default(), [true; 3]);
                assert!((1..=4).contains(&specs.len()));
                let has_maps = specs
                    .iter()
                    .any(|spec| matches!(spec.kind, PlotKind::Heatmap(_)));
                let has_other = specs
                    .iter()
                    .any(|spec| !matches!(spec.kind, PlotKind::Heatmap(_)));
                mixed |= has_maps && has_other;
                compound |= specs[0].cluster == Some(Cluster::Compound);
                if matches!(
                    family,
                    Family::SpectralShape | Family::Aggression | Family::Energy | Family::Bass
                ) {
                    assert!(specs.len() >= 2 && !has_maps);
                }
                for spec in specs {
                    if spec.kind == PlotKind::Aggression {
                        assert!(spec.ratio >= AGGRESSION_MIN_RATIO);
                    }
                }
            }
        }
        assert!(mixed && compound);
    }

    fn obeys_grid_edges(rects: &[Rect], gap: f64, air: f64) -> bool {
        let eq = |a: f64, b: f64| (a - b).abs() < 1e-5;
        if rects.len() == 1 {
            return true;
        }
        let first = rects[0];
        let row = rects
            .iter()
            .all(|rect| eq(rect.y0, first.y0) && eq(rect.y1, first.y1))
            && rects
                .windows(2)
                .all(|pair| eq(pair[1].x0, pair[0].x1 + gap));
        let column = rects
            .iter()
            .all(|rect| eq(rect.x0, first.x0) && eq(rect.x1, first.x1))
            && rects
                .windows(2)
                .all(|pair| eq(pair[1].y0, pair[0].y1 + gap));
        let stepped = rects.iter().all(|rect| eq(rect.width(), first.width()))
            && eq(rects[1].x0, first.x0 + (first.width() + gap) / 3.0)
            && rects
                .iter()
                .enumerate()
                .all(|(index, rect)| eq(rect.x0, rects[index % 2].x0))
            && rects
                .windows(2)
                .all(|pair| eq(pair[1].y0, pair[0].y1 + gap));
        let sidecar = eq(rects[1].x0, first.x1 + gap)
            && eq(rects[1].y0, first.y0)
            && eq(rects.last().unwrap().y1, first.y1)
            && rects[1..]
                .iter()
                .all(|rect| eq(rect.x0, rects[1].x0) && eq(rect.x1, rects[1].x1))
            && rects[1..]
                .windows(2)
                .all(|pair| eq(pair[1].y0, pair[0].y1 + gap));
        let spans = rects.len() == 3
            && ((eq(first.y0, rects[1].y0)
                && eq(first.y1, rects[1].y1)
                && eq(rects[1].x0, first.x1 + gap)
                && eq(rects[2].x0, first.x0)
                && eq(rects[2].x1, rects[1].x1)
                && eq(rects[2].y0, first.y1 + gap))
                || (eq(first.x0, rects[1].x0)
                    && eq(first.x1, rects[1].x1)
                    && eq(rects[1].y0, first.y1 + gap)
                    && eq(rects[2].x0, first.x1 + gap)
                    && eq(rects[2].y0, first.y0)
                    && eq(rects[2].y1, rects[1].y1)));
        let compound = rects.len() == 4
            && eq(first.x0, rects[1].x0)
            && eq(first.x1, rects[1].x1)
            && eq(rects[2].x0, rects[3].x0)
            && eq(rects[2].x1, rects[3].x1)
            && eq(first.width(), rects[2].width())
            && eq(first.y0, rects[2].y0)
            && eq(rects[2].x0, first.x1 + air)
            && eq(rects[1].y0, first.y1 + gap)
            && eq(rects[3].y0, rects[2].y1 + gap);
        row || column || stepped || sidecar || spans || compound
    }

    #[test]
    fn every_placed_kit_obeys_shared_grid_edges_across_kinds_and_viewports() {
        let mut rng = Rng::new(78);
        let mut placements = 0;
        for size in [
            Size::new(640.0, 360.0),
            Size::new(800.0, 600.0),
            Size::new(1280.0, 720.0),
            Size::new(1920.0, 1080.0),
            Size::new(3440.0, 1440.0),
        ] {
            let stage = Stage::new(size);
            for family in [
                Family::Heatmaps,
                Family::Attack,
                Family::SpectralShape,
                Family::Aggression,
                Family::Energy,
                Family::Bass,
                Family::Onsets,
            ] {
                for _ in 0..16 {
                    let specs = choose_specs(family, &mut rng, Events::default(), [true; 3]);
                    let Some(rects) = arrange(&specs, &stage, &[], &mut rng) else {
                        continue;
                    };
                    placements += 1;
                    assert!(obeys_grid_edges(
                        &rects,
                        stage.px(CARD_GAP),
                        stage.px(FORMATION_AIR)
                    ));
                    let bounds = footprint(&rects);
                    if contains(stage.left, bounds) {
                        assert!((bounds.x0 - stage.left.x0).abs() < 1e-5);
                    } else {
                        assert!(contains(stage.right, bounds));
                        assert!((bounds.x1 - stage.right.x1).abs() < 1e-5);
                    }
                    for (rect, spec) in rects.iter().zip(&specs) {
                        let chart = plot_bounds(*rect, &stage);
                        assert!(chart.width() >= stage.px(MIN_CHART_WIDTH) - 0.001);
                        assert!((chart.width() / chart.height() - spec.ratio).abs() < 1e-5);
                    }
                }
            }
        }
        assert!(placements > 200);
    }

    #[test]
    fn a_failed_preferred_recipe_falls_back_to_a_grid_or_declines() {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut rng = Rng::new(79);
        let specs = [
            PlotSpec {
                kind: PlotKind::Heatmap(0),
                ratio: 1.0,
                cluster: Some(Cluster::Row),
            },
            PlotSpec {
                kind: PlotKind::Attack(0),
                ratio: 2.6,
                cluster: Some(Cluster::Row),
            },
        ];
        assert!(arrange_cluster(
            &specs,
            Cluster::Row,
            &[stage.left, stage.right],
            &stage,
            &[],
            &mut rng
        )
        .is_none());
        let rects = arrange(&specs, &stage, &[], &mut rng).unwrap();
        assert!(obeys_grid_edges(
            &rects,
            stage.px(CARD_GAP),
            stage.px(FORMATION_AIR)
        ));
        assert!((rects[0].x0 - rects[1].x0).abs() < 1e-5);
        assert!((rects[0].x1 - rects[1].x1).abs() < 1e-5);
        assert!(arrange(&specs, &stage, &[stage.left, stage.right], &mut rng).is_none());
        assert!(arrange(&specs, &Stage::new(Size::new(20.0, 20.0)), &[], &mut rng).is_none());
        let wide_stage = Stage::new(Size::new(3440.0, 1440.0));
        let narrow_hero = [
            PlotSpec {
                kind: PlotKind::Heatmap(0),
                ratio: 0.5,
                cluster: Some(Cluster::Sidecar),
            },
            PlotSpec {
                kind: PlotKind::Attack(0),
                ratio: 2.8,
                cluster: Some(Cluster::Sidecar),
            },
            PlotSpec {
                kind: PlotKind::Onsets,
                ratio: 3.6,
                cluster: Some(Cluster::Sidecar),
            },
        ];
        let rects = arrange(&narrow_hero, &wide_stage, &[], &mut rng).unwrap();
        assert!(rects
            .iter()
            .all(|rect| plot_bounds(*rect, &wide_stage).width()
                >= wide_stage.px(MIN_CHART_WIDTH) - 0.001));
        assert!(obeys_grid_edges(
            &rects,
            wide_stage.px(CARD_GAP),
            wide_stage.px(FORMATION_AIR)
        ));
    }

    #[test]
    fn mixed_sidecar_and_stepped_formations_have_tight_gaps_and_different_extents() {
        let mut rng = Rng::new(74);
        let kinds = [PlotKind::Heatmap(0), PlotKind::Aggression, PlotKind::Energy];
        let mut specs: Vec<_> = kinds
            .into_iter()
            .map(|kind| plot_spec(kind, &mut rng))
            .collect();
        specs[0].ratio = 1.0;
        let sidecar = cluster_geometry(
            &specs,
            Cluster::Sidecar,
            cluster_minimum(&specs, Cluster::Sidecar),
        )
        .unwrap();
        assert!((sidecar[1].x0 - sidecar[0].x1 - CARD_GAP).abs() < 1e-8);
        assert!((sidecar[2].y0 - sidecar[1].y1 - CARD_GAP).abs() < 1e-8);
        assert_ne!(sidecar[0].width(), sidecar[1].width());
        assert_eq!(sidecar[0].y0, sidecar[1].y0);
        assert!((sidecar[0].y1 - sidecar[2].y1).abs() < 1e-8);
        assert!(sidecar[1].height() < sidecar[2].height());
        let stepped = cluster_geometry(
            &specs,
            Cluster::Stepped,
            cluster_minimum(&specs, Cluster::Stepped),
        )
        .unwrap();
        assert!(stepped[0].x0 != stepped[1].x0 && stepped[1].x0 != stepped[2].x0);
        let occupied: f64 = stepped.iter().map(|rect| rect.area()).sum();
        assert!(occupied >= footprint(&stepped).area() * 0.7);
        for rects in [&sidecar, &stepped] {
            for (index, (rect, spec)) in rects.iter().zip(&specs).enumerate() {
                assert!(rects[..index].iter().all(|other| !overlaps(
                    *rect,
                    *other,
                    CARD_GAP - 0.01
                )));
                assert!(
                    ((rect.width() - PLOT_GUTTER_WIDTH) / (rect.height() - PLOT_GUTTER_HEIGHT)
                        - spec.ratio)
                        .abs()
                        < 1e-8
                );
            }
        }
    }

    fn compound_specs() -> Vec<PlotSpec> {
        [
            (PlotKind::Heatmap(0), 1.0),
            (PlotKind::Attack(0), 2.5),
            (PlotKind::SpectralShape, 2.4),
            (PlotKind::Energy, 2.8),
        ]
        .into_iter()
        .map(|(kind, ratio)| PlotSpec {
            kind,
            ratio,
            cluster: Some(Cluster::Compound),
        })
        .collect()
    }

    #[test]
    fn compound_formations_distinguish_tight_inner_gaps_from_air_between_kits() {
        let specs = compound_specs();
        let rects = cluster_geometry(
            &specs,
            Cluster::Compound,
            cluster_minimum(&specs, Cluster::Compound),
        )
        .unwrap();
        assert!((rects[1].y0 - rects[0].y1 - CARD_GAP).abs() < 1e-8);
        assert!((rects[3].y0 - rects[2].y1 - CARD_GAP).abs() < 1e-8);
        assert!((rects[2].x0 - rects[0].x1 - FORMATION_AIR).abs() < 1e-8);
        assert!(FORMATION_AIR > CARD_GAP * 5.0);
        assert_eq!(rects[0].y0, rects[2].y0);
    }

    #[test]
    fn a_formation_cannot_wrap_another_reserved_footprint_with_its_empty_pocket() {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut rng = Rng::new(77);
        let specs: Vec<_> = [
            (PlotKind::Heatmap(0), 1.0),
            (PlotKind::Aggression, AGGRESSION_MIN_RATIO),
            (PlotKind::Energy, 2.4),
        ]
        .into_iter()
        .map(|(kind, ratio)| PlotSpec {
            kind,
            ratio,
            cluster: Some(Cluster::Sidecar),
        })
        .collect();
        let local = cluster_geometry(
            &specs,
            Cluster::Stepped,
            cluster_minimum(&specs, Cluster::Stepped),
        )
        .unwrap();
        let region = footprint(&local);
        let pocket = Rect::new(
            local[0].x1 + 12.0,
            local[0].y0 + 12.0,
            local[1].x1 - 12.0,
            local[0].y1 - 12.0,
        );
        assert!(pocket.height() > 0.0);
        assert!(local.iter().all(|rect| !overlaps(*rect, pocket, CARD_GAP)));
        assert!(arrange_cluster(
            &specs,
            Cluster::Stepped,
            &[region],
            &stage,
            &[pocket],
            &mut rng
        )
        .is_none());
    }

    #[test]
    fn two_four_plot_formations_fit_without_interleaving_and_reach_the_cap() -> anyhow::Result<()> {
        let visualizer = Visualizer::new();
        let mut fonts = Fonts::new()?;
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut rng = Rng::new(75);
        let mut graphs = GraphCompositions::default();
        for _ in 0..2 {
            assert!(graphs.spawn(
                compound_specs(),
                &visualizer,
                &mut fonts,
                &stage,
                &mut rng,
                &[stage.right]
            ));
        }
        assert_eq!(graphs.plot_count(), MAX_PLOTS);
        let first = group_bounds(&graphs.groups[0]);
        let second = group_bounds(&graphs.groups[1]);
        assert!((first.x0 - stage.left.x0).abs() < 1e-5);
        assert!((second.x0 - first.x0).abs() < 1e-5);
        assert!(!overlaps(first, second, stage.px(FORMATION_AIR - 0.01)));
        assert!(!graphs.spawn(
            compound_specs(),
            &visualizer,
            &mut fonts,
            &stage,
            &mut rng,
            &[]
        ));
        Ok(())
    }

    #[test]
    fn aggression_stays_a_thin_progress_indicator_in_every_formation() {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut rng = Rng::new(76);
        for _ in 0..60 {
            let specs = choose_specs(Family::Aggression, &mut rng, Events::default(), [true; 3]);
            let rects = arrange(&specs, &stage, &[], &mut rng).unwrap();
            for (rect, spec) in rects.iter().zip(&specs) {
                if spec.kind == PlotKind::Aggression {
                    let chart = plot_bounds(*rect, &stage);
                    assert!(chart.width() / chart.height() >= AGGRESSION_MIN_RATIO - 0.001);
                    assert!(chart.height() <= stage.px(32.0) + 0.001);
                    assert!(chart.width() >= stage.px(MIN_CHART_WIDTH) - 0.001);
                }
            }
        }
    }

    #[test]
    fn mixed_heatmap_layouts_fit_side_columns_and_preserve_chart_ratios() {
        for size in [
            Size::new(1920.0, 1080.0),
            Size::new(1280.0, 720.0),
            Size::new(1024.0, 768.0),
            Size::new(720.0, 1280.0),
            Size::new(2560.0, 1080.0),
        ] {
            let stage = Stage::new(size);
            let mut rng = Rng::new(27);
            let mut placed = 0;
            for _ in 0..80 {
                let specs = choose_specs(Family::Heatmaps, &mut rng, Events::default(), [true; 3]);
                let Some(rects) = arrange(&specs, &stage, &[], &mut rng) else {
                    continue;
                };
                placed += 1;
                for (index, (&rect, spec)) in rects.iter().zip(&specs).enumerate() {
                    assert!(contains(stage.left, rect) || contains(stage.right, rect));
                    let plot_width = rect.width() - stage.px(PLOT_GUTTER_WIDTH);
                    let plot_height = rect.height() - stage.px(PLOT_GUTTER_HEIGHT);
                    assert!((plot_width / plot_height - spec.ratio).abs() < 1e-8);
                    assert!(rects[..index].iter().all(|other| !overlaps(
                        rect,
                        *other,
                        stage.px(CARD_GAP - 0.01)
                    )));
                }
            }
            assert!(
                placed >= 40,
                "normal viewports retain a useful range of fitting grids"
            );
        }
    }

    #[test]
    fn placement_respects_overlay_bounds_and_skips_a_fully_occupied_scene() {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut rng = Rng::new(48);
        let specs = [PlotSpec {
            kind: PlotKind::Heatmap(0),
            ratio: 1.0,
            cluster: None,
        }];
        let rects = arrange(&specs, &stage, &[stage.left], &mut rng).unwrap();
        assert!(rects.iter().all(|rect| contains(stage.right, *rect)));
        assert!(arrange(&specs, &stage, &[stage.left, stage.right], &mut rng).is_none());
    }

    fn cluster_specs(cluster: Cluster) -> Vec<PlotSpec> {
        let ratios = match cluster {
            Cluster::Row | Cluster::Column => [1.0, 2.0, 1.0],
            Cluster::TwoOverOne => [1.0, 1.0, 2.0],
            Cluster::TwoBesideOne => [1.0, 1.0, 0.5],
            Cluster::Stepped | Cluster::Sidecar | Cluster::Compound => [1.0, 2.0, 1.0],
        };
        ratios
            .into_iter()
            .enumerate()
            .map(|(channel, ratio)| PlotSpec {
                kind: PlotKind::Heatmap(channel),
                ratio,
                cluster: Some(cluster),
            })
            .collect()
    }

    #[test]
    fn cluster_rows_and_columns_keep_coaxial_plots_and_mixed_sizes() {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut rng = Rng::new(63);
        let lane = Rect::new(32.0, 800.0, 1888.0, 1048.0);
        for cluster in [Cluster::Row, Cluster::Column] {
            let specs = cluster_specs(cluster);
            let regions = if cluster == Cluster::Row {
                vec![lane]
            } else {
                vec![stage.left]
            };
            let rects = arrange_cluster(&specs, cluster, &regions, &stage, &[], &mut rng).unwrap();
            let charts: Vec<_> = rects
                .iter()
                .map(|rect| plot_bounds(*rect, &stage))
                .collect();
            for (chart, spec) in charts.iter().zip(&specs) {
                assert!((chart.width() / chart.height() - spec.ratio).abs() < 1e-8);
                assert!(chart.width() >= stage.px(MIN_CHART_WIDTH) - 0.001);
            }
            if cluster == Cluster::Row {
                assert!(charts
                    .iter()
                    .all(|chart| (chart.y0 - charts[0].y0).abs() < 1e-8
                        && (chart.height() - charts[0].height()).abs() < 1e-8));
                assert!((charts[1].width() - charts[0].width() * 2.0).abs() < 1e-8);
            } else {
                assert!(charts
                    .iter()
                    .all(|chart| (chart.x0 - charts[0].x0).abs() < 1e-8
                        && (chart.width() - charts[0].width()).abs() < 1e-8));
                assert!((charts[1].height() * 2.0 - charts[0].height()).abs() < 1e-8);
            }
        }
    }

    #[test]
    fn two_plus_one_clusters_form_blocks_without_stretching_any_map() {
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut rng = Rng::new(64);
        for cluster in [Cluster::TwoOverOne, Cluster::TwoBesideOne] {
            let specs = cluster_specs(cluster);
            let rects = arrange_cluster(
                &specs,
                cluster,
                &[stage.left, stage.right],
                &stage,
                &[],
                &mut rng,
            )
            .unwrap();
            for (rect, spec) in rects.iter().zip(&specs) {
                let chart = plot_bounds(*rect, &stage);
                assert!((chart.width() / chart.height() - spec.ratio).abs() < 1e-8);
            }
            if cluster == Cluster::TwoOverOne {
                assert!((rects[0].y0 - rects[1].y0).abs() < 1e-8);
                assert!((rects[0].x0 - rects[2].x0).abs() < 1e-8);
                assert!((rects[1].x1 - rects[2].x1).abs() < 1e-8);
                assert!(rects[2].y0 > rects[0].y1);
            } else {
                assert!((rects[0].x0 - rects[1].x0).abs() < 1e-8);
                assert!((rects[0].y0 - rects[2].y0).abs() < 1e-8);
                assert!((rects[1].y1 - rects[2].y1).abs() < 1e-8);
                assert!(rects[2].x0 > rects[0].x1);
            }
        }
    }

    #[test]
    fn resizing_retains_each_groups_cluster_recipe_and_reserves_its_neighbors() -> anyhow::Result<()>
    {
        let visualizer = Visualizer::new();
        let mut fonts = Fonts::new()?;
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut rng = Rng::new(68);
        let mut graphs = GraphCompositions {
            last_mode: Some(visualizer.showcase_mode()),
            ..GraphCompositions::default()
        };
        graphs.update(0.0, &visualizer, &mut fonts, &stage, &mut rng, &[]);
        assert!(graphs.spawn(
            cluster_specs(Cluster::Column),
            &visualizer,
            &mut fonts,
            &stage,
            &mut rng,
            &[]
        ));
        assert!(graphs.spawn(
            vec![PlotSpec {
                kind: PlotKind::Bass,
                ratio: 2.0,
                cluster: None,
            }],
            &visualizer,
            &mut fonts,
            &stage,
            &mut rng,
            &[]
        ));
        let resized = Stage::new(Size::new(1280.0, 720.0));
        graphs.update(0.0, &visualizer, &mut fonts, &resized, &mut rng, &[]);
        assert_eq!(graphs.groups.len(), 2);
        for group in &graphs.groups {
            let rects: Vec<_> = group.cards.iter().map(|card| card.rect).collect();
            assert!(obeys_grid_edges(
                &rects,
                resized.px(CARD_GAP),
                resized.px(FORMATION_AIR)
            ));
        }
        let first = &graphs.groups[0].cards;
        assert!(first
            .iter()
            .all(|card| (card.rect.x0 - first[0].rect.x0).abs() < 1e-8));
        for card in &graphs.groups[1].cards {
            assert!(first
                .iter()
                .all(|other| !overlaps(card.rect, other.rect, resized.px(6.0))));
        }
        Ok(())
    }

    #[test]
    fn full_width_rows_reserve_the_model_and_its_motion_blur() {
        use crate::visualizer::{LineInstance, RibbonInstance};
        let stage = Stage::new(Size::new(1920.0, 1080.0));
        let mut visualizer = Visualizer::new();
        visualizer.model_lines.push(LineInstance {
            endpoints: [700.0, 400.0, 1200.0, 600.0],
            start_color: [1.0; 4],
            end_color: [1.0; 4],
            style: [2.0, 0.0, 0.0, 0.0],
        });
        visualizer.model_ribbons.push(RibbonInstance {
            start: [700.0, 600.0, 1200.0, 650.0],
            end: [1000.0, 700.0],
            start_color: [1.0; 4],
            end_color: [1.0; 4],
            third_color: [1.0; 4],
            style: [0.0; 2],
        });
        let model = model_bounds(&visualizer, &stage).unwrap();
        assert!(
            model.y1 >= 724.0,
            "ribbon sweep extends the reserved support"
        );
        let lanes = horizontal_lanes(&visualizer, &stage);
        assert_eq!(lanes.len(), 2);
        let specs = cluster_specs(Cluster::Row);
        let mut rng = Rng::new(65);
        let rects = arrange_in(&specs, &stage, &[], &mut rng, &lanes).unwrap();
        assert!(rects
            .iter()
            .all(|rect| !overlaps(*rect, model, stage.px(6.0))));
        assert!(rects
            .iter()
            .all(|rect| (rect.y0 - rects[0].y0).abs() < 1e-8));
    }

    #[test]
    fn attack_compositions_pair_related_channels_and_omit_missing_snapshots() {
        let mut rng = Rng::new(53);
        for events in [
            Events {
                kick: true,
                ..Events::default()
            },
            Events {
                snare: true,
                ..Events::default()
            },
            Events {
                hat: true,
                ..Events::default()
            },
        ] {
            for _ in 0..20 {
                let specs = choose_specs(Family::Attack, &mut rng, events, [true; 3]);
                let channel = specs
                    .iter()
                    .find_map(|spec| match spec.kind {
                        PlotKind::Attack(channel) => Some(channel),
                        _ => None,
                    })
                    .expect("an available attack is included");
                assert!(matching_event(PlotKind::Attack(channel), events));
                assert_eq!(specs[0].kind, PlotKind::Heatmap(channel));
                assert!(specs.iter().any(|spec| spec.kind == PlotKind::Onsets));
                let missing = choose_specs(Family::Attack, &mut rng, events, [false; 3]);
                assert!(missing
                    .iter()
                    .all(|spec| !matches!(spec.kind, PlotKind::Attack(_))));
                assert_eq!(missing[0].kind, PlotKind::Heatmap(channel));
            }
        }
    }

    #[test]
    fn beat_accents_follow_the_graphs_channel() {
        let kick = Events {
            kick: true,
            ..Events::default()
        };
        assert!(matching_event(PlotKind::Heatmap(0), kick));
        assert!(matching_event(PlotKind::Attack(0), kick));
        assert!(matching_event(PlotKind::Bass, kick));
        assert!(matching_event(PlotKind::Onsets, kick));
        assert!(!matching_event(PlotKind::Heatmap(1), kick));
        assert!(!matching_event(PlotKind::Attack(2), kick));
    }
}
