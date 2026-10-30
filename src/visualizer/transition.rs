//! Exchange model edges through shared slots instead of drawing two complete
//! wireframes throughout a global-opacity crossfade.

use super::{smoothstep, Rng};

/// Fraction of transition progress used for each side of an edge handoff.
/// Outgoing fades to zero before incoming fades up, so their supports do not
/// overlap. The shuffled handoffs distribute the small brightness dip spatially.
const FEATHER: f32 = 0.04;

#[derive(Clone, Copy)]
enum Selection {
    All,
    Incoming { progress: f32, slots: usize },
    Outgoing { progress: f32, slots: usize },
}

pub(super) struct EdgeDissolve {
    /// Each edge has one unique, stable rank; building this order is linear.
    ranks: Vec<u32>,
    selection: Selection,
    /// A switch may interrupt a partially revealed model. Retain that exact
    /// coverage as its outgoing starting point instead of resurrecting edges.
    inherited: Option<Vec<f32>>,
}

impl EdgeDissolve {
    pub fn new(edge_count: usize, seed: u64) -> Self {
        let mut ranks: Vec<_> =
            (0..u32::try_from(edge_count).expect("too many model edges")).collect();
        let mut rng = Rng::new(seed);
        for index in (1..edge_count).rev() {
            ranks.swap(index, rng.range(index + 1));
        }
        Self {
            ranks,
            selection: Selection::All,
            inherited: None,
        }
    }

    pub fn show_all(&mut self) {
        self.selection = Selection::All;
        self.inherited = None;
    }

    pub fn incoming(&mut self, progress: f32, slots: usize) {
        self.selection = Selection::Incoming { progress, slots };
    }

    pub fn outgoing(&mut self, progress: f32, slots: usize) {
        self.selection = Selection::Outgoing { progress, slots };
    }

    pub fn freeze(&mut self) {
        if !matches!(self.selection, Selection::All) {
            self.inherited = Some(
                (0..self.ranks.len())
                    .map(|index| self.alpha(index))
                    .collect(),
            );
            self.selection = Selection::All;
        }
    }

    pub fn alpha(&self, index: usize) -> f32 {
        let inherited = self
            .inherited
            .as_ref()
            .map_or(1.0, |coverage| coverage[index]);
        if inherited == 0.0 {
            return 0.0;
        }
        let (progress, slots, incoming) = match self.selection {
            Selection::All => return inherited,
            Selection::Incoming { progress, slots } => (progress, slots, true),
            Selection::Outgoing { progress, slots } => (progress, slots, false),
        };
        debug_assert!(slots >= self.ranks.len());
        // Inject each model's ranks into a common grid. Each slot contains at
        // most one edge from either model; only one side can be nonzero at once.
        // Use integer arithmetic so differently sized models share exact slots.
        let slot = self.ranks[index] as u64 * slots as u64 / self.ranks.len() as u64;
        let threshold = (slot as f32 + 0.5) / slots as f32;
        let position = progress.clamp(0.0, 1.0) * (1.0 + 2.0 * FEATHER) - FEATHER;
        let distance = if incoming {
            position - threshold
        } else {
            threshold - position
        };
        inherited * smoothstep(distance / FEATHER)
    }
}

/// FNV-1a keeps the order reproducible across runs and platforms without adding
/// transition work to the animation RNG or correlating it with mesh edge order.
pub(super) fn model_seed(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests;
