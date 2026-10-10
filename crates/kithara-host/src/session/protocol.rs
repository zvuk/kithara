use firewheel::FirewheelContext;
use kithara_command::{Mailbox, PostError, Postbox, Refused, Ticket};
use kithara_platform::maybe_send::{MaybeSend, MaybeSync};
use kithara_play::PlayError;
pub(crate) use kithara_play::{SessionError, SessionSampleRate};

pub(crate) type StartStreamFn<T> =
    Box<dyn FnMut(&mut FirewheelContext, u32) -> Result<T, String> + Send + 'static>;
pub(crate) type HostPostbox<C> = Postbox<C, PlayError>;
pub(crate) type HostMailbox<C> = Mailbox<C, PlayError>;

pub(crate) enum HostDispatchError {
    NotTaken(PlayError),
    Refused(PlayError),
    Unanswered,
}

impl From<HostDispatchError> for PlayError {
    fn from(error: HostDispatchError) -> Self {
        match error {
            HostDispatchError::NotTaken(error) | HostDispatchError::Refused(error) => error,
            HostDispatchError::Unanswered => Self::SessionGone {
                reason: "the owner dropped a command unanswered",
            },
        }
    }
}

pub(crate) trait HostDispatcher<C>: MaybeSend + MaybeSync {
    fn dispatch(&self, command: C) -> Result<Ticket<PlayError>, HostDispatchError>;
    fn shutdown(&self);
}

pub(crate) fn ask<C, D: HostDispatcher<C> + ?Sized>(
    dispatcher: &D,
    command: C,
) -> Result<(), HostDispatchError> {
    dispatcher
        .dispatch(command)?
        .wait()
        .map_err(|refused| match refused {
            Refused::Owner(error) => HostDispatchError::Refused(error),
            Refused::Unanswered => HostDispatchError::Unanswered,
        })
}

pub(crate) fn not_taken(_: PostError) -> HostDispatchError {
    HostDispatchError::NotTaken(PlayError::SessionGone {
        reason: "the owner stopped taking commands",
    })
}

#[cfg(test)]
mod tests {
    use kithara_command::{Post, mailbox};
    use kithara_platform::sync::Mutex;
    use kithara_test_utils::{bufpool::TestPools, kithara};

    use super::*;
    use crate::HostCommand;

    type BaseCommand = HostCommand<TestPools, dyn kithara_play::HostedDeck<TestPools>>;

    /// A session that drains each post as it lands and answers it with what
    /// `outcome` gives, or drops it unanswered for `None`.
    struct Session {
        postbox: HostPostbox<BaseCommand>,
        mailbox: Mutex<HostMailbox<BaseCommand>>,
        outcome: fn() -> Option<Result<(), PlayError>>,
    }

    impl HostDispatcher<BaseCommand> for Session {
        fn dispatch(&self, cmd: BaseCommand) -> Result<Ticket<PlayError>, HostDispatchError> {
            let ticket = self.postbox.post(cmd).map_err(not_taken)?;
            for Post { answer, .. } in self.mailbox.lock().drain() {
                if let Some(outcome) = (self.outcome)() {
                    answer.answer(outcome);
                }
            }
            Ok(ticket)
        }

        fn shutdown(&self) {}
    }

    fn session(outcome: fn() -> Option<Result<(), PlayError>>) -> Session {
        let (postbox, mailbox) = mailbox();
        Session {
            postbox,
            mailbox: Mutex::new(mailbox),
            outcome,
        }
    }

    #[kithara::test]
    fn a_post_the_session_drops_reads_unanswered() {
        let asked = ask(&session(|| None), HostCommand::Restart);

        assert!(matches!(asked, Err(HostDispatchError::Unanswered)));
    }

    #[kithara::test]
    fn the_session_refusal_reaches_the_caller() {
        let asked = ask(
            &session(|| Some(Err(PlayError::Late))),
            HostCommand::Restart,
        );

        assert!(matches!(
            asked,
            Err(HostDispatchError::Refused(PlayError::Late))
        ));
    }
}
