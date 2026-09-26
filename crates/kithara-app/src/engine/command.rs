use kithara::{effects::GainDb, queue::TrackId};

use crate::deck::{DeckId, EqMode, TempoPercent};

#[derive(Debug)]
pub(crate) struct Envelope {
    pub(crate) command: Command,
    pub(crate) seq: u64,
}

#[derive(Debug)]
pub(crate) enum Command {
    Deck { deck: DeckId, cmd: DeckCmd },
    Mix(MixCmd),
    LoadOntoDeck { deck: DeckId, source: String },
    App(AppCmd),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DeckCmd {
    Play,
    Pause,
    Next,
    Prev,
    SeekFraction(f64),
    SetEqGain {
        layout: EqMode,
        band: usize,
        gain: GainDb,
    },
    RemoveTrack(TrackId),
    SetTempo(TempoPercent),
    SetQuality(Option<usize>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum MixCmd {
    Crossfader(f32),
    Master(f32),
    Muted(DeckId, bool),
    Trim(DeckId, f32),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum AppCmd {
    SetEqMode(EqMode),
    BroadcastToggle,
    Shutdown,
}

#[cfg(all(test, not(feature = "broadcast")))]
mod tests {
    use std::convert::Infallible;

    use ::kithara::{effects::GainDb, ui::render::ControlAction};
    use kithara_test_utils::{kithara, off_thread::OffThread};

    use crate::gui::rig::Rig;

    const KNOB: f64 = 1e-4;

    #[kithara::test(native, tokio, flash(false))]
    async fn a_gain_for_a_replaced_band_layout_is_rejected_and_echoed() {
        let rig = OffThread::spawn("engine", || Ok::<_, Infallible>(Rig::offline()))
            .await
            .expect("rig fixture is infallible");
        rig.call(|rig| {
            let mid = f32::from(GainDb::at_knob(0.75));
            rig.send("mixer/a/mid-3", ControlAction::SetScalar(0.75));
            rig.pump();
            rig.frame();
            assert!((rig.scalar("deck.eq.mid@deck=a") - 0.75).abs() < KNOB);
            assert_eq!(rig.queues[0].eq_gain(1), Some(mid));

            rig.send("mixer/a/eq-4", ControlAction::Activate);
            rig.send("mixer/a/high-3", ControlAction::SetScalar(0.0));
            let applied = rig.pump();
            assert_eq!(
                applied.len(),
                2,
                "the UI still draws three bands, so the gain names the three-band layout"
            );
            rig.frame();

            assert_eq!(
                rig.applied_seq(),
                applied[1],
                "a rejected command is echoed like any other"
            );
            let queue = &rig.queues[0];
            assert_eq!(queue.eq_band_count(), 4);
            assert_eq!(
                queue.eq_gain(2),
                Some(mid),
                "high-mid keeps the gain the mid band folded into it"
            );
            assert_eq!(queue.eq_gain(3), Some(0.0), "the high band is untouched");
            assert!((rig.scalar("deck.eq.bands@deck=a") - 4.0).abs() < f64::EPSILON);
            assert!((rig.scalar("deck.eq.high_mid@deck=a") - 0.75).abs() < KNOB);
            assert!((rig.scalar("deck.eq.high@deck=a") - 0.5).abs() < KNOB);
        })
        .await;
        rig.close().await;
    }
}
