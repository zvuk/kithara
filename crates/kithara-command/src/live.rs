use std::collections::VecDeque;

use kithara_config::{ConfigOwner, LiveConfig};

use crate::{Batch, Outcome, Protocol, Receipt, SendError, Sender, Seq, When};

/// A change in flight: the batch carrying it, its moment and the change.
type InFlight<C, P> = (Seq, When<<P as Protocol>::Clock>, <C as LiveConfig>::Change);

/// A live configuration as its executor confirmed it, with a copy of every
/// change sent to the executor and not yet answered.
///
/// Getters read the confirmed configuration; a sent change shows once its
/// receipt settles it as applied.
#[derive(ConfigOwner, Debug)]
#[config_owner(config)]
pub struct Live<C: LiveConfig, P: Protocol> {
    config: C,
    in_flight: VecDeque<InFlight<C, P>>,
}

/// Why a change was not sent; nothing was sent or changed.
#[derive(Debug, thiserror::Error)]
pub enum LiveError<E, P: Protocol> {
    /// A field check refused the change.
    #[error("a field check refused the change")]
    Invalid(#[source] E),
    /// The channel returned the batch carrying the change.
    #[error(transparent)]
    Send(SendError<P>),
}

/// The copy of a change its receipt settled; the outcome is the receipt's.
pub struct SettledChange<C: LiveConfig, P: Protocol> {
    /// The change.
    pub change: C::Change,
    /// Moment the change was sent to apply at.
    pub when: When<P::Clock>,
}

impl<C: LiveConfig, P: Protocol> Live<C, P> {
    /// A live configuration checked whole, with nothing in flight.
    ///
    /// # Errors
    ///
    /// Returns the refusal of the first field that fails its check.
    pub fn new(config: C) -> Result<Self, C::Error> {
        Ok(Self {
            config: config.validated()?,
            in_flight: VecDeque::new(),
        })
    }

    /// Folds every copy in flight into the configuration in the order the
    /// executor applies them, `(When, Seq)`, for a queue destroyed without
    /// answering them. Returns whether there was a copy, so the configuration
    /// may have changed.
    pub fn abandon(&mut self) -> bool {
        let abandoned = !self.in_flight.is_empty();
        self.in_flight
            .make_contiguous()
            .sort_by_key(|&(seq, when, _)| (when, seq));
        for (_, _, change) in self.in_flight.drain(..) {
            self.config.apply_change(change);
        }
        abandoned
    }

    /// Checks `change` and applies it at once, for a configuration without an
    /// executor or a field its owner executes itself.
    ///
    /// # Errors
    ///
    /// Returns the field check's refusal; the configuration stays as it was.
    pub fn apply(&mut self, change: C::Change) -> Result<(), C::Error> {
        self.config.apply_change(C::check(change)?);
        Ok(())
    }

    /// Copies of the changes in flight, in send order.
    pub fn pending(&self) -> impl Iterator<Item = InFlight<C, P>> {
        self.in_flight.iter().copied()
    }

    /// Checks `change` and sends it to apply at `when`: one batch with an
    /// empty basis and the single command `wrap` makes of it. A copy waits
    /// for the batch's receipt.
    ///
    /// # Errors
    ///
    /// Returns [`LiveError::Invalid`] when a field check refuses the change
    /// and [`LiveError::Send`] when the channel returns the batch.
    pub fn send<W>(
        &mut self,
        sender: &mut Sender<P>,
        when: When<P::Clock>,
        change: C::Change,
        wrap: W,
    ) -> Result<Seq, LiveError<C::Error, P>>
    where
        W: FnOnce(C::Change) -> P::Command,
    {
        let change = C::check(change).map_err(LiveError::Invalid)?;
        let batch = Batch {
            basis: Vec::new(),
            commands: vec![wrap(change)],
        };
        let seq = sender.send(when, batch).map_err(LiveError::Send)?;
        self.in_flight.push_back((seq, when, change));
        Ok(seq)
    }

    /// Settles the copy of the change `receipt` answers: an applied change
    /// moves into the configuration and a rejected one is dropped. Returns the
    /// copy either way, or `None` when the batch carried no change of this
    /// configuration.
    pub fn settle(&mut self, receipt: &Receipt<P>) -> Option<SettledChange<C, P>> {
        let index = self
            .in_flight
            .iter()
            .position(|&(seq, ..)| seq == receipt.seq())?;
        let (_, when, change) = self.in_flight.remove(index)?;
        if matches!(receipt.outcome(), Outcome::Applied { .. }) {
            self.config.apply_change(change);
        }
        Some(SettledChange { change, when })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use kithara_config::{Config, ConfigOwner, LiveConfig};
    use kithara_test_utils::kithara;

    use super::{Live, LiveError};
    use crate::{
        ChannelConfig, Clock, Inbox, Outcome, Protocol, Rejection, SendError, Sender, Seq, Target,
        When, channel,
    };

    /// A level of at most ten and a mute switch.
    #[derive(Clone, Copy, Debug, PartialEq, Config)]
    #[config(default, owner_access, check(error = Loud), fields(value, get(copy), builder(default)))]
    struct Mix {
        #[config(live)]
        muted: bool,
        #[config(live, check = quiet)]
        level: u8,
    }

    /// A level over ten.
    #[derive(Debug, PartialEq)]
    struct Loud(u8);

    fn quiet(level: u8) -> Result<u8, Loud> {
        if level <= 10 {
            Ok(level)
        } else {
            Err(Loud(level))
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Config)]
    #[config(default, owner_access, fields(value, get(copy), builder(default)))]
    struct Tone {
        #[config(live)]
        pitch: i8,
    }

    /// The executor's commands: a change of either configuration.
    #[derive(Debug)]
    enum Part {
        Mix(MixChange),
        Tone(ToneChange),
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Test {}

    impl Protocol for Test {
        type Applied = ();
        type Clock = Frame;
        type Command = Part;
        type Refusal = ();
        type Target = NoTarget;
    }

    /// Changes of a configuration shift no time, so no batch names a target.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum NoTarget {}

    impl Target for NoTarget {
        fn index(self) -> usize {
            match self {}
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    struct Frame(u64);

    impl Clock for Frame {
        fn frames_since(self, start: Self) -> Option<u64> {
            self.0.checked_sub(start.0)
        }
    }

    fn pair(capacity: usize) -> (Sender<Test>, Inbox<Test>) {
        let capacity = NonZeroUsize::new(capacity).expect("a test channel holds a batch");
        channel(ChannelConfig::builder().capacity(capacity).build())
    }

    fn mix() -> Live<Mix, Test> {
        Live::new(Mix::default()).expect("the default mix is quiet")
    }

    /// The configurations the executor renders with.
    #[derive(Default)]
    struct Executor {
        mix: Mix,
        tone: Tone,
    }

    impl Executor {
        /// Frames of one block.
        const BLOCK: usize = 64;

        /// Applies every batch due in the block from `start` and lists each as
        /// its offset, basis size and commands.
        fn render(&mut self, inbox: &mut Inbox<Test>, start: u64) -> Vec<String> {
            inbox.drain();
            let mut applied = Vec::new();
            while let Some(due) = inbox.next_due(Frame(start), Self::BLOCK) {
                applied.push(format!(
                    "{} {} {:?}",
                    due.offset(),
                    due.basis().len(),
                    due.commands()
                ));
                for command in due.commands() {
                    match *command {
                        Part::Mix(change) => self.mix.apply_change(change),
                        Part::Tone(change) => self.tone.apply_change(change),
                    }
                }
                due.apply(());
            }
            applied
        }
    }

    /// Refuses every batch due in the block from `start`.
    fn refuse(inbox: &mut Inbox<Test>, start: u64) {
        inbox.drain();
        while let Some(due) = inbox.next_due(Frame(start), Executor::BLOCK) {
            due.refuse(());
        }
    }

    #[kithara::test]
    fn new_refuses_a_config_whose_field_fails_its_check() {
        let refused = Live::<Mix, Test>::new(Mix {
            muted: false,
            level: 11,
        });
        assert!(matches!(refused, Err(Loud(11))));
    }

    #[kithara::test]
    fn apply_shows_at_once_and_a_refused_change_keeps_the_value() {
        let mut live = mix();
        live.apply(MixChange::Level(4)).expect("level in bounds");
        assert_eq!(live.level(), 4);
        assert_eq!(live.apply(MixChange::Level(11)), Err(Loud(11)));
        assert_eq!(live.level(), 4);
    }

    #[kithara::test]
    fn a_sent_change_shows_once_its_receipt_settles() {
        let (mut sender, mut inbox) = pair(4);
        let mut executor = Executor::default();
        let mut live = mix();
        live.send(&mut sender, When::Next, MixChange::Level(5), Part::Mix)
            .expect("the channel has room");
        assert_eq!(live.level(), 0, "the executor has not confirmed it yet");
        executor.render(&mut inbox, 0);
        let settled = sender
            .receipts()
            .filter_map(|receipt| live.settle(&receipt))
            .count();
        assert_eq!(settled, 1);
        assert_eq!(live.level(), 5);
        assert_eq!(live.config(), &executor.mix);
        assert_eq!(live.pending().count(), 0);
    }

    #[kithara::test]
    fn a_change_at_a_frame_applies_at_that_frame() {
        let (mut sender, mut inbox) = pair(4);
        let mut executor = Executor::default();
        let mut live = mix();
        live.send(
            &mut sender,
            When::At(Frame(70)),
            MixChange::Muted(true),
            Part::Mix,
        )
        .expect("the channel has room");
        assert!(executor.render(&mut inbox, 0).is_empty());
        assert_eq!(executor.render(&mut inbox, 64), ["6 0 [Mix(Muted(true))]"]);
        let receipt = sender.receipts().next().expect("the batch was answered");
        assert_eq!(
            receipt.outcome(),
            &Outcome::Applied {
                at: Frame(70),
                data: ()
            }
        );
        assert!(live.settle(&receipt).is_some());
        assert!(live.muted());
        assert_eq!(live.config(), &executor.mix);
    }

    #[kithara::test]
    fn a_rejected_change_comes_back_and_the_config_stays() {
        let (mut sender, mut inbox) = pair(4);
        let mut live = mix();
        live.send(&mut sender, When::Next, MixChange::Level(5), Part::Mix)
            .expect("the channel has room");
        refuse(&mut inbox, 0);
        let receipt = sender.receipts().next().expect("the batch was answered");
        assert_eq!(
            receipt.outcome(),
            &Outcome::Rejected(Rejection::Refused(()))
        );
        let settled = live.settle(&receipt).expect("its copy was in flight");
        assert_eq!(
            (settled.when, format!("{:?}", settled.change)),
            (When::Next, "Level(5)".to_owned())
        );
        assert_eq!(live.level(), 0);
        assert_eq!(live.pending().count(), 0);
    }

    #[kithara::test]
    fn each_live_settles_only_the_receipts_of_its_own_batches() {
        let (mut sender, mut inbox) = pair(4);
        let mut executor = Executor::default();
        let mut mix = mix();
        let mut tone = Live::<Tone, Test>::new(Tone::default()).expect("nothing to refuse");
        let mixed = mix
            .send(&mut sender, When::Next, MixChange::Level(3), Part::Mix)
            .expect("the channel has room");
        let toned = tone
            .send(&mut sender, When::Next, ToneChange::Pitch(-2), Part::Tone)
            .expect("the channel has room");
        executor.render(&mut inbox, 0);
        for receipt in sender.receipts() {
            let settled = (
                mix.settle(&receipt).is_some(),
                tone.settle(&receipt).is_some(),
            );
            assert_eq!(settled, (receipt.seq() == mixed, receipt.seq() == toned));
        }
        assert_eq!(
            (mix.config(), tone.config()),
            (&executor.mix, &executor.tone)
        );
        assert_eq!((mix.level(), tone.pitch()), (3, -2));
    }

    #[kithara::test]
    fn send_hands_over_one_batch_of_one_command_with_an_empty_basis() {
        let (mut sender, mut inbox) = pair(4);
        let mut executor = Executor::default();
        let mut live = mix();
        live.send(&mut sender, When::Next, MixChange::Level(2), Part::Mix)
            .expect("the channel has room");
        assert_eq!(executor.render(&mut inbox, 0), ["0 0 [Mix(Level(2))]"]);
    }

    #[kithara::test]
    fn pending_lists_the_copies_in_send_order() {
        let (mut sender, _inbox) = pair(4);
        let mut live = mix();
        let changes = [
            (When::At(Frame(100)), MixChange::Level(1)),
            (When::Next, MixChange::Level(2)),
            (When::At(Frame(50)), MixChange::Muted(true)),
        ];
        let sent: Vec<(Seq, When<Frame>, String)> = changes
            .into_iter()
            .map(|(when, change)| {
                let seq = live
                    .send(&mut sender, when, change, Part::Mix)
                    .expect("the channel has room");
                (seq, when, format!("{change:?}"))
            })
            .collect();
        let pending: Vec<(Seq, When<Frame>, String)> = live
            .pending()
            .map(|(seq, when, change)| (seq, when, format!("{change:?}")))
            .collect();
        assert_eq!(pending, sent);
    }

    #[kithara::test]
    fn a_full_channel_returns_the_batch_and_changes_nothing() {
        let (mut sender, mut inbox) = pair(1);
        let mut executor = Executor::default();
        let mut live = mix();
        live.send(&mut sender, When::Next, MixChange::Level(1), Part::Mix)
            .expect("the channel has room");
        let full = live.send(&mut sender, When::Next, MixChange::Level(2), Part::Mix);
        assert!(matches!(full, Err(LiveError::Send(SendError::Full(_)))));
        assert_eq!(live.pending().count(), 1);
        executor.render(&mut inbox, 0);
        let settled = sender
            .receipts()
            .filter_map(|receipt| live.settle(&receipt))
            .count();
        assert_eq!((settled, live.level()), (1, 1));
        live.send(&mut sender, When::Next, MixChange::Level(2), Part::Mix)
            .expect("the receipt returned its credit");
    }

    #[kithara::test]
    fn abandon_folds_the_copies_in_the_order_the_executor_applies_them() {
        let (mut sender, _inbox) = pair(4);
        let mut live = mix();
        for (when, change) in [
            (When::At(Frame(100)), MixChange::Level(1)),
            (When::Next, MixChange::Level(2)),
            (When::At(Frame(50)), MixChange::Level(3)),
            (When::Next, MixChange::Muted(true)),
        ] {
            live.send(&mut sender, when, change, Part::Mix)
                .expect("the channel has room");
        }
        assert!(live.abandon());
        assert_eq!((live.level(), live.muted()), (1, true));
        assert_eq!(live.pending().count(), 0);
        assert!(!live.abandon(), "nothing is left in flight");
    }

    #[kithara::test]
    fn a_change_its_check_refuses_is_never_sent() {
        let (mut sender, mut inbox) = pair(4);
        let mut executor = Executor::default();
        let mut live = mix();
        let refused = live.send(&mut sender, When::Next, MixChange::Level(11), Part::Mix);
        assert!(matches!(refused, Err(LiveError::Invalid(Loud(11)))));
        assert!(executor.render(&mut inbox, 0).is_empty());
        assert_eq!(live.pending().count(), 0);
    }
}
