use crate::{Batch, Outcome, Protocol, Receipt, Seq, Target, When};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct Book<P: Protocol> {
    #[field(get = available, vis = "pub(super)")]
    credits: usize,
    targets: usize,
    applied: Vec<Option<Seq>>,
    pending: Vec<Pending<P>>,
}

struct Pending<P: Protocol> {
    when: When<P::Clock>,
    seq: Seq,
    basis: Vec<(usize, Option<Seq>)>,
}

impl<P: Protocol> Book<P> {
    pub(super) fn new(credits: usize, targets: usize) -> Self {
        Self {
            credits,
            targets,
            applied: vec![None; targets],
            pending: Vec::with_capacity(credits),
        }
    }

    pub(super) fn admits(&self, batch: &Batch<P>) -> bool {
        batch
            .basis
            .iter()
            .all(|&(target, _)| target.index() < self.targets)
    }

    pub(super) fn spend(
        &mut self,
        when: When<P::Clock>,
        seq: Seq,
        basis: &[(P::Target, Option<Seq>)],
    ) {
        self.credits -= 1;
        if basis.is_empty() {
            return;
        }
        let index = self
            .pending
            .partition_point(|pending| (pending.when, pending.seq) < (when, seq));
        self.pending.insert(
            index,
            Pending {
                when,
                seq,
                basis: basis
                    .iter()
                    .map(|&(target, basis)| (target.index(), basis))
                    .collect(),
            },
        );
    }

    pub(super) fn basis(&self, target: P::Target, when: When<P::Clock>) -> Option<Seq> {
        if target.index() >= self.targets {
            return None;
        }
        let mut ledger = self.applied.clone();
        for pending in &self.pending {
            if pending.when > when {
                break;
            }
            if pending
                .basis
                .iter()
                .all(|&(target, basis)| ledger[target] == basis)
            {
                for &(target, _) in &pending.basis {
                    ledger[target] = Some(pending.seq);
                }
            }
        }
        ledger[target.index()]
    }

    pub(super) fn settle(&mut self, receipt: &Receipt<P>) {
        self.credits += 1;
        self.pending.retain(|pending| pending.seq != receipt.seq());
        if matches!(receipt.outcome(), Outcome::Applied { .. }) {
            for &(target, _) in &receipt.batch().basis {
                self.applied[target.index()] =
                    self.applied[target.index()].max(Some(receipt.seq()));
            }
        }
    }

    pub(super) fn reset(&mut self, targets: usize) {
        self.targets = targets;
        self.applied.fill(None);
        self.pending.clear();
    }
}
