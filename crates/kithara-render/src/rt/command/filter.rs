use kithara_command::Due;

use super::super::processor::Deck;
use crate::bridge::{DeckPart, DeckProtocol, DeckRefusal};

impl Deck {
    pub(in crate::rt) fn take_filter_due(
        &mut self,
        mut due: Due<'_, DeckProtocol>,
        context_valid: bool,
    ) {
        let binding = match due.commands() {
            [DeckPart::Filter { binding, .. }] => *binding,
            _ => {
                due.refuse(DeckRefusal::FilterBatch);
                return;
            }
        };
        if !context_valid || self.epoch != Some(binding.epoch()) {
            due.refuse(DeckRefusal::AxisRestarted);
            return;
        }
        if binding != self.render.filter_binding(self.scope, binding.epoch()) {
            due.refuse(DeckRefusal::FilterOutdated);
            return;
        }
        let seq = due.seq();
        let DeckPart::Filter { binding, change } = due.commands_mut().remove(0) else {
            unreachable!("single Filter command admitted");
        };
        match self
            .render
            .change_filter(change, binding, seq, self.sample_rate)
        {
            Ok(returned) => {
                if let Some(returned) = returned {
                    due.commands_mut().push(returned);
                }
                due.apply(());
            }
            Err((reason, change)) => {
                due.commands_mut()
                    .push(DeckPart::Filter { binding, change });
                due.refuse(reason);
            }
        }
    }
}
