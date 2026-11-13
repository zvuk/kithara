use std::ops::Range;

use kithara_command::LevelInbox;
use kithara_effects::eq::GainShapeError;
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
    ) -> Result<(), GainShapeError> {
        let at = SessionFrame::new(
            i64::from(start).saturating_add(i64::try_from(range.start).unwrap_or(i64::MAX)),
        );
        self.programs.write(level, at, range.len());
        if let Some([left, right]) = input {
            if self.programs.any_written() {
                self.render.finish_curves(left, right, &self.programs)?;
            } else {
                self.render.finish(left, right)?;
            }
        } else {
            self.render.advance(range.len(), &self.programs);
            self.render.idle();
        }
        if let Some(last) = range.end.checked_sub(1) {
            self.finish_programs(
                level,
                SessionFrame::new(
                    i64::from(start).saturating_add(i64::try_from(last).unwrap_or(i64::MAX)),
                ),
            );
        }
        Ok(())
    }
}
