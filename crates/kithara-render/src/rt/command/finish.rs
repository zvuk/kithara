use std::ops::Range;

use kithara_command::LevelInbox;
use kithara_signal::SessionFrame;

use super::super::processor::Deck;
use crate::bridge::DeckProtocol;

impl Deck {
    pub(in crate::rt) fn finish_range(
        &mut self,
        level: &mut LevelInbox<'_, DeckProtocol>,
        start: SessionFrame,
        range: Range<usize>,
        input: Option<[&mut [f32]; 2]>,
    ) {
        let at = SessionFrame::new(
            i64::from(start).saturating_add(i64::try_from(range.start).expect("prepared frame")),
        );
        let animated = {
            let eq = self.eq_programs.curves(level);
            let filter = self.filter_programs.program(level);
            let animated = eq.iter().any(Option::is_some) || filter.is_some();
            if let Some([left, right]) = input {
                if animated {
                    self.render.finish_curves(left, right, at, &eq, filter);
                } else {
                    self.render.finish(left, right);
                }
            } else {
                self.render.advance(at, range.len(), &eq, filter);
                self.render.idle();
            }
            animated
        };
        if !animated {
            return;
        }
        let Some(last) = range.end.checked_sub(1) else {
            return;
        };
        let at = SessionFrame::new(
            i64::from(start).saturating_add(i64::try_from(last).expect("prepared frame")),
        );
        self.finish_filter_programs(level, at);
        self.finish_eq_programs(level, at);
    }
}
