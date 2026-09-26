use kithara::platform::sync::Arc;

use crate::{
    deck::DeckId,
    engine::{Command, DeckCmd, DeckSnapshot, EngineSnapshot, MixCmd},
    mix::MixStrip,
};

#[derive(Default)]
pub(crate) struct Overlay {
    pending: Vec<(u64, Edit)>,
}

#[derive(Clone, Copy)]
enum Edit {
    Mix(MixCmd),
    Deck(DeckId, DeckCmd),
}

impl Overlay {
    pub(crate) fn over(&self, snapshot: &Arc<EngineSnapshot>) -> Arc<EngineSnapshot> {
        if self.pending.is_empty() {
            return Arc::clone(snapshot);
        }
        let mut drawn = EngineSnapshot::clone(snapshot);
        for (_, edit) in &self.pending {
            edit.lay_over(&mut drawn);
        }
        Arc::new(drawn)
    }

    pub(crate) fn record(&mut self, seq: u64, command: &Command) {
        let edit = match command {
            Command::Mix(cmd) => Edit::Mix(*cmd),
            Command::Deck {
                deck,
                cmd:
                    cmd @ (DeckCmd::SetEqGain { .. } | DeckCmd::SetQuality(_) | DeckCmd::SetTempo(_)),
            } => Edit::Deck(*deck, *cmd),
            Command::Deck { .. } | Command::LoadOntoDeck { .. } | Command::App(_) => return,
        };
        self.pending.push((seq, edit));
    }

    pub(crate) fn retire(&mut self, applied_seq: u64) {
        self.pending.retain(|(seq, _)| *seq > applied_seq);
    }
}

impl Edit {
    fn lay_over(self, drawn: &mut EngineSnapshot) {
        match self {
            Self::Mix(cmd) => lay_mix(cmd, drawn),
            Self::Deck(id, cmd) => lay_deck(id, cmd, drawn),
        }
    }
}

fn lay_deck(id: DeckId, cmd: DeckCmd, drawn: &mut EngineSnapshot) {
    let eq_mode = drawn.eq_mode;
    let Some(deck) = deck_mut(drawn, id) else {
        return;
    };
    match cmd {
        DeckCmd::SetEqGain { layout, band, gain } if layout == eq_mode => {
            if let Some(slot) = deck.eq_bands.get_mut(band) {
                *slot = gain;
            }
        }
        DeckCmd::SetQuality(variant) => {
            deck.stream.selected = variant;
            deck.stream.is_auto = variant.is_none();
        }
        DeckCmd::SetTempo(tempo) => deck.tempo = tempo,
        _ => {}
    }
}

fn deck_mut(drawn: &mut EngineSnapshot, id: DeckId) -> Option<&mut DeckSnapshot> {
    drawn.decks.iter_mut().find(|deck| deck.id == id)
}

fn strip_mut(drawn: &mut EngineSnapshot, id: DeckId) -> Option<&mut MixStrip> {
    let at = drawn.decks.iter().position(|deck| deck.id == id)?;
    drawn.mix.strips.get_mut(at)
}

fn lay_mix(cmd: MixCmd, drawn: &mut EngineSnapshot) {
    match cmd {
        MixCmd::Crossfader(position) => drawn.mix.position = position,
        MixCmd::Master(gain) => drawn.mix.group_master = gain,
        MixCmd::Muted(id, muted) => {
            if let Some(strip) = strip_mut(drawn, id) {
                strip.muted = muted;
            }
        }
        MixCmd::Trim(id, trim) => {
            if let Some(strip) = strip_mut(drawn, id) {
                strip.trim = trim;
            }
        }
    }
}

#[cfg(all(test, not(feature = "broadcast")))]
mod tests {
    use std::convert::Infallible;

    use ::kithara::ui::render::ControlAction;
    use kithara_test_utils::{kithara, off_thread::OffThread};

    use crate::gui::rig::Rig;

    #[kithara::test(native, tokio, flash(false))]
    async fn a_moved_fader_draws_its_position_before_and_after_the_echo() {
        let rig = OffThread::spawn("engine", || Ok::<_, Infallible>(Rig::offline()))
            .await
            .expect("rig fixture is infallible");
        rig.call(|rig| {
            assert!((rig.scalar("mix.crossfader") - 0.5).abs() < f64::EPSILON);
            let before = rig.applied_seq();

            rig.send("mixer/xfade", ControlAction::SetScalar(1.0));

            assert!(
                (rig.scalar("mix.crossfader") - 1.0).abs() < f64::EPSILON,
                "the fader draws the position it was moved to at once"
            );
            assert_eq!(rig.applied_seq(), before, "the engine has not applied it");
            assert!((rig.snapshots.load().mix.position - 0.5).abs() < f32::EPSILON);

            rig.frame();
            assert!(
                (rig.scalar("mix.crossfader") - 1.0).abs() < f64::EPSILON,
                "a frame that reloads an older snapshot keeps the pending position"
            );

            let applied = rig.pump();
            assert_eq!(applied.len(), 1, "one move, one command");
            rig.frame();

            assert_eq!(rig.applied_seq(), applied[0]);
            assert!((rig.snapshots.load().mix.position - 1.0).abs() < f32::EPSILON);
            assert!(
                (rig.scalar("mix.crossfader") - 1.0).abs() < f64::EPSILON,
                "the echoed snapshot keeps the position"
            );
        })
        .await;
        rig.close().await;
    }
}
