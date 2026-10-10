use crate::protocol::{Seq, Target};

/// Last applied batch that shifted each target's time.
pub(super) struct Ledger {
    last: Vec<Option<Seq>>,
}

impl Ledger {
    pub(super) fn new(targets: usize) -> Self {
        Self {
            last: vec![None; targets],
        }
    }

    /// Whether every target of `basis` was last shifted by the batch it names.
    pub(super) fn is_current<T: Target>(&self, basis: &[(T, Option<Seq>)]) -> bool {
        basis
            .iter()
            .all(|&(target, expected)| self.last.get(target.index()) == Some(&expected))
    }

    /// Records `seq` as the last batch to shift each target of `basis`.
    pub(super) fn record<T: Target>(&mut self, basis: &[(T, Option<Seq>)], seq: Seq) {
        self.last
            .iter_mut()
            .enumerate()
            .filter(|&(index, _)| basis.iter().any(|&(target, _)| target.index() == index))
            .for_each(|(_, last)| *last = Some(seq));
    }

    pub(super) fn reset(&mut self) {
        self.last.fill(None);
    }

    pub(super) fn outdates<T: Target>(&self, basis: &[(T, Option<Seq>)]) -> bool {
        basis.iter().any(|&(target, basis)| {
            self.last
                .get(target.index())
                .copied()
                .flatten()
                .is_some_and(|current| basis < Some(current))
        })
    }
}
