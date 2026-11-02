//! Exchange model edges through shared slots instead of drawing two complete
//! wireframes throughout a global-opacity crossfade.

use std::sync::Arc;

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

/// Immutable shuffle prepared with a model and shared by its runtime instances.
#[derive(Clone)]
pub(super) struct EdgeOrder {
    ranks: Arc<[u32]>,
}

impl EdgeOrder {
    pub fn new(edge_count: usize, seed: u64) -> Self {
        let mut ranks: Vec<_> =
            (0..u32::try_from(edge_count).expect("too many model edges")).collect();
        let mut rng = Rng::new(seed);
        for index in (1..edge_count).rev() {
            ranks.swap(index, rng.range(index + 1));
        }
        Self {
            ranks: ranks.into(),
        }
    }

    fn alpha(&self, selection: Selection, index: usize) -> f32 {
        let (progress, slots, incoming) = match selection {
            Selection::All => return 1.0,
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
        smoothstep(distance / FEATHER)
    }
}

enum InheritedCoverage {
    All,
    #[cfg(test)]
    Selection(Selection),
    /// Supports repeated freezes of an already inherited transition. Normal
    /// model switches freeze a newly entering model only once.
    #[cfg(test)]
    Combined(Vec<f32>),
}

pub(super) struct EdgeDissolve {
    order: EdgeOrder,
    selection: Selection,
    /// Preserve an interrupted reveal without scanning or allocating per edge.
    inherited: InheritedCoverage,
}

impl EdgeDissolve {
    #[cfg(test)]
    pub fn new(edge_count: usize, seed: u64) -> Self {
        Self::from_order(EdgeOrder::new(edge_count, seed))
    }

    pub fn from_order(order: EdgeOrder) -> Self {
        Self {
            order,
            selection: Selection::All,
            inherited: InheritedCoverage::All,
        }
    }

    pub fn show_all(&mut self) {
        self.selection = Selection::All;
        self.inherited = InheritedCoverage::All;
    }

    pub fn incoming(&mut self, progress: f32, slots: usize) {
        self.selection = Selection::Incoming { progress, slots };
    }

    pub fn outgoing(&mut self, progress: f32, slots: usize) {
        self.selection = Selection::Outgoing { progress, slots };
    }

    #[cfg(test)]
    pub fn freeze(&mut self) {
        if !matches!(self.selection, Selection::All) {
            self.inherited = match self.inherited {
                InheritedCoverage::All => InheritedCoverage::Selection(self.selection),
                _ => InheritedCoverage::Combined(
                    (0..self.order.ranks.len())
                        .map(|index| self.alpha(index))
                        .collect(),
                ),
            };
            self.selection = Selection::All;
        }
    }

    pub fn alpha(&self, index: usize) -> f32 {
        let inherited = match &self.inherited {
            InheritedCoverage::All => 1.0,
            #[cfg(test)]
            InheritedCoverage::Selection(selection) => self.order.alpha(*selection, index),
            #[cfg(test)]
            InheritedCoverage::Combined(coverage) => coverage[index],
        };
        if inherited == 0.0 {
            return 0.0;
        }
        inherited * self.order.alpha(self.selection, index)
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
