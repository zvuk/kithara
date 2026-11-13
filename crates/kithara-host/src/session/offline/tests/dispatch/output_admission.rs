use std::task::Poll;

use kithara_command::{Refused, Seq};
use kithara_platform::time::Instant;
use kithara_play::{DeckPass, Outbox};
use kithara_signal::SessionFrame;

use super::*;

struct PreparingOwner {
    inner: TestState,
    preparation: Poll<Result<(), PlayError>>,
}

impl HostSettingsExec<()> for PreparingOwner {
    type At = When<SessionFrame>;
    type Output = Result<Option<Seq>, PlayError>;

    delegate::delegate! {
        to self.inner.owner {
            fn exec_sample_rate(&mut self, value: NonZeroU32, at: Self::At, cx: &mut ()) -> Self::Output;
            fn exec_tempo(&mut self, value: Tempo, at: Self::At, cx: &mut ()) -> Self::Output;
            fn exec_live(&mut self, change: HostSettingsChange, at: Self::At, cx: &mut ()) -> Self::Output;
        }
    }
}

impl HostOwner<TestPools> for PreparingOwner {
    type Command = Command;
    type Deck = Queue<TestPools>;

    fn poll_output(&mut self) -> Poll<Result<(), PlayError>> {
        match self.preparation.clone() {
            Poll::Ready(Ok(())) => self.inner.owner.poll_output(),
            outcome => outcome,
        }
    }

    delegate::delegate! {
        to self.inner.owner {
            fn apply(&mut self, command: Self::Command) -> Result<Option<Seq>, PlayError>;
            fn register(&mut self, id: BeatGridId, deck: Box<Self::Deck>) -> Result<(), PlayError>;
            fn each_deck(&mut self, visit: &mut dyn FnMut(BeatGridId, &mut Self::Deck, &mut Outbox<'_, TestPools>, DeckPass<'_>));
            fn with_deck(&mut self, id: BeatGridId, visit: &mut dyn FnMut(&mut Self::Deck, &mut Outbox<'_, TestPools>, DeckPass<'_>)) -> Result<(), PlayError>;
            fn clock(&self) -> Option<crate::PassClock>;
            fn prepare_offline(&mut self) -> Result<(), PlayError>;
            fn render_offline(&mut self, position: u64, frames: usize, output: &mut [f32]) -> Result<(), PlayError>;
            fn transport(&mut self) -> Option<crate::api::SessionTransportSnapshot>;
            fn host_room(&self) -> usize;
            fn begin_pass(&mut self);
            fn pass(&mut self) -> Vec<crate::HostSettled>;
        }
    }

    fn release_id(command: &Self::Command) -> Option<BeatGridId> {
        <crate::HostCore<TestPools, Queue<TestPools>> as HostOwner<TestPools>>::release_id(command)
    }
}

#[kithara::test]
fn a_pending_output_keeps_posts_in_the_same_mailbox_and_preserves_fifo() {
    let mut owner = PreparingOwner {
        inner: test_state(start_route_loss_stream),
        preparation: Poll::Pending,
    };
    let (postbox, mut mailbox) = mailbox::<Command, PlayError>();
    let id = BeatGridId::allocate().expect("deck identity");
    let first = postbox.post(registration(id)).expect("first post");
    let second = postbox.post(registration(id)).expect("second post");
    let mut posts = OwnerPosts::new();

    posts.drain(&mut owner, &mut mailbox);
    let first = first.wait_until(Instant::now());
    let second = second.wait_until(Instant::now());
    let pending = first.is_err() && second.is_err();
    let untouched = owner.inner.ctx.is_none() && owner.inner.deck_nodes.is_empty();

    owner.preparation = Poll::Ready(Ok(()));
    posts.drain(&mut owner, &mut mailbox);
    let [first, second] = [first, second].map(|early| match early {
        Ok(answer) => Some(answer),
        Err(ticket) => ticket.wait_until(Instant::now()).ok(),
    });
    let held = owner.inner.deck_nodes.len();
    drop(owner);
    assert!(pending, "pending output must leave both posts unanswered");
    assert!(
        untouched,
        "pending preparation must not execute registration"
    );
    assert!(matches!(first, Some(Ok(()))));
    assert!(matches!(
        second,
        Some(Err(Refused::Owner(PlayError::Session(SessionError::DeckAttached(held))))) if held == id
    ));
    assert_eq!(held, 1, "only the first registration owns the deck");
}

#[kithara::test]
fn a_failed_output_answers_each_held_post_with_the_typed_start_error() {
    let mut owner = PreparingOwner {
        inner: test_state(start_route_loss_stream),
        preparation: Poll::Ready(Err(PlayError::Session(SessionError::StreamStart(
            "bootstrap refused".into(),
        )))),
    };
    let (postbox, mut mailbox) = mailbox::<Command, PlayError>();
    let id = BeatGridId::allocate().expect("deck identity");
    let first = postbox.post(registration(id)).expect("first post");
    let second = postbox.post(registration(id)).expect("second post");
    let mut posts = OwnerPosts::new();

    posts.drain(&mut owner, &mut mailbox);
    let answers = [first, second].map(|ticket| ticket.wait_until(Instant::now()).ok());
    let untouched = owner.inner.ctx.is_none() && owner.inner.deck_nodes.is_empty();
    drop(owner);
    for answer in answers {
        assert!(matches!(
            answer,
            Some(Err(Refused::Owner(PlayError::Session(SessionError::StreamStart(reason)))))
                if reason == "bootstrap refused"
        ));
    }
    assert!(untouched, "a refused start must not execute registration");
}

#[kithara::test]
fn an_empty_mailbox_does_not_prepare_an_idle_offline_output() {
    route_loss(RouteLossProbe::reset);
    let mut state = test_state(start_route_loss_stream);
    let (_postbox, mut mailbox) = mailbox::<Command, PlayError>();
    let mut posts = OwnerPosts::new();
    for _ in 0..4 {
        posts.drain(&mut state.owner, &mut mailbox);
    }
    let idle = state.ctx.is_none() && state.stream.is_none();
    let starts = route_loss(|probe| probe.start_count.load(Ordering::SeqCst));
    drop(state);
    assert!(idle, "an empty mailbox must not acquire the idle output");
    assert_eq!(starts, 0);
}

#[kithara::test]
fn a_started_offline_owner_executes_a_post_in_the_same_drain() {
    let mut state = test_state(start_route_loss_stream);
    let prepared = state.owner.poll_output();
    let actual = state
        .ctx
        .as_ref()
        .is_some_and(|ctx| ctx.is_active() && ctx.stream_info().is_some())
        && state.session_output_node_id.is_some()
        && !state.stream_needs_restart;
    let (postbox, mut mailbox) = mailbox::<Command, PlayError>();
    let id = BeatGridId::allocate().expect("deck identity");
    let ticket = postbox
        .post(registration(id))
        .expect("post after output start");
    OwnerPosts::new().drain(&mut state.owner, &mut mailbox);
    let answer = ticket.wait_until(Instant::now()).ok();
    let held = state.deck_nodes.len();
    drop(state);
    assert!(matches!(prepared, Poll::Ready(Ok(()))));
    assert!(
        actual,
        "SDK, output master and geometry must exist before the post"
    );
    assert!(matches!(answer, Some(Ok(()))));
    assert_eq!(held, 1);
}
