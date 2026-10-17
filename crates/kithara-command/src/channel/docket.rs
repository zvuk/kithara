use super::{ledger::Ledger, schedule::Schedule, sender::Sent};
use crate::{ChannelConfig, Protocol, Seq, When};

pub(super) struct Docket<P: Protocol> {
    pub(super) schedule: Schedule<P>,
    pub(super) ledger: Ledger,
    pub(super) parked: Vec<Sent<P>>,
    pub(super) committed: Vec<(P::Clock, Sent<P>)>,
    pub(super) arrived: Vec<Sent<P>>,
}

impl<P: Protocol> Docket<P> {
    pub(super) fn new(config: ChannelConfig) -> Self {
        let capacity = config.capacity.get();
        Self {
            schedule: Schedule::new(capacity),
            ledger: Ledger::new(config.targets),
            parked: Vec::with_capacity(capacity),
            committed: Vec::with_capacity(capacity),
            arrived: Vec::with_capacity(capacity),
        }
    }

    pub(super) fn insert(&mut self, sent: Sent<P>) {
        if matches!(sent.when, When::Deferred) {
            debug_assert!(
                self.arrived.len() < self.arrived.capacity(),
                "credits bound arrivals"
            );
            self.arrived.push(sent);
        } else {
            self.schedule.insert(sent);
        }
    }

    pub(super) fn park(&mut self, sent: Sent<P>) {
        debug_assert!(
            self.parked.len() < self.parked.capacity(),
            "credits bound parked batches"
        );
        self.parked.push(sent);
    }

    pub(super) fn frames_until_due(&self, start: P::Clock) -> Option<u64> {
        match self.schedule.peek()? {
            When::Next => Some(0),
            When::At(at) => Some(P::frames_since(at, start).unwrap_or(0)),
            When::Deferred => None,
        }
    }

    pub(super) fn is_parked(&self, seq: Seq) -> bool {
        self.parked.iter().any(|sent| sent.seq == seq)
    }

    pub(super) fn committed_mut(&mut self, seq: Seq) -> Option<&mut [P::Command]> {
        self.committed
            .iter_mut()
            .find_map(|(_, sent)| (sent.seq == seq).then_some(sent.batch.commands.as_mut_slice()))
    }

    pub(super) fn take_outdated(&mut self) -> Option<Sent<P>> {
        if let Some(sent) = self.schedule.take_outdated(&self.ledger) {
            return Some(sent);
        }
        for items in [&mut self.arrived, &mut self.parked] {
            if let Some(index) = items
                .iter()
                .position(|sent| self.ledger.outdates(&sent.batch.basis))
            {
                return Some(items.remove(index));
            }
        }
        None
    }
}
