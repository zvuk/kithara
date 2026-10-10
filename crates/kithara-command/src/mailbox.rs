use std::{mem, task::Waker};

use kithara_platform::sync::{Arc, Mutex, mpsc};

use crate::protocol::Seq;

/// The owner dropped its mailbox, so a post reaches no one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the owner dropped its mailbox")]
pub struct PostError;

/// Why a post did not apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused<R> {
    /// The owner refused it for a reason of its own domain.
    Owner(R),
    /// The owner dropped it without answering, or dropped its mailbox with
    /// the post still in it.
    Unanswered,
}

/// Who runs the posts a mailbox takes.
enum Holder {
    Nobody,
    Executor(Waker),
    Gone,
}

struct State<C, R> {
    holder: Holder,
    posts: Vec<Post<C, R>>,
    next: Seq,
}

/// Creates a mailbox for an owner and the first postbox that reaches it.
#[must_use]
pub fn mailbox<C, R>() -> (Postbox<C, R>, Mailbox<C, R>) {
    let state = Arc::new(Mutex::new(State {
        holder: Holder::Nobody,
        posts: Vec::new(),
        next: Seq::FIRST,
    }));
    (
        Postbox {
            state: Arc::clone(&state),
        },
        Mailbox { state },
    )
}

/// Posting half of a mailbox. Every clone posts into the same queue, so the
/// owner runs posts from all of them in the order they were posted.
pub struct Postbox<C, R> {
    state: Arc<Mutex<State<C, R>>>,
}

impl<C, R> Clone for Postbox<C, R> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

impl<C, R> Postbox<C, R> {
    /// Queues `command` for the owner, wakes the executor that holds it, and
    /// returns the ticket its answer comes to. A post made while no executor
    /// holds the owner waits for the next one. Posts are numbered as they are
    /// queued, so their numbers grow in the order the owner runs them.
    ///
    /// # Errors
    ///
    /// Returns [`PostError`] once the owner dropped its mailbox; the command
    /// is dropped.
    pub fn post(&self, command: C) -> Result<Ticket<R>, PostError> {
        let (answer, answered) = mpsc::channel();
        let mut state = self.state.lock();
        let waker = match &state.holder {
            Holder::Gone => return Err(PostError),
            Holder::Nobody => None,
            Holder::Executor(waker) => Some(waker.clone()),
        };
        let seq = state.next;
        state.next = seq.next();
        state.posts.push(Post {
            command,
            answer: Answer(answer),
        });
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(Ticket { seq, answered })
    }

    /// Whether the owner dropped its mailbox, so a post reaches no one.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        matches!(self.state.lock().holder, Holder::Gone)
    }
}

/// Where a post's answer comes. Dropping it gives the answer up; the owner
/// runs the post all the same.
pub struct Ticket<R> {
    seq: Seq,
    answered: mpsc::Receiver<Result<(), R>>,
}

impl<R> Ticket<R> {
    /// Number of the post this ticket answers.
    #[must_use]
    pub fn seq(&self) -> Seq {
        self.seq
    }

    /// Waits for the owner to answer the post. Never call it on the thread
    /// whose executor holds the owner: the answer would never come.
    ///
    /// # Errors
    ///
    /// Returns the owner's refusal, or [`Refused::Unanswered`] when the post
    /// went unanswered.
    pub fn wait(self) -> Result<(), Refused<R>> {
        self.answered.recv().map_or_else(
            |_| Err(Refused::Unanswered),
            |answer| answer.map_err(Refused::Owner),
        )
    }
}

/// One command an owner drained, with the answer its poster waits for.
pub struct Post<C, R> {
    /// The command to run.
    pub command: C,
    /// The answer to give once it ran.
    pub answer: Answer<R>,
}

/// The answer to one post. Dropping it unanswered leaves the post
/// [`Refused::Unanswered`].
pub struct Answer<R>(mpsc::Sender<Result<(), R>>);

impl<R> Answer<R> {
    /// Answers the post: `Ok` when it applied, the owner's refusal otherwise.
    /// A poster that gave its ticket up loses only the answer.
    pub fn answer(self, outcome: Result<(), R>) {
        let _ = self.0.send(outcome);
    }
}

/// Receiving half of a mailbox, kept by the owner its commands are for.
pub struct Mailbox<C, R> {
    state: Arc<Mutex<State<C, R>>>,
}

impl<C, R> Mailbox<C, R> {
    /// Wakes `waker` after each post from now on, so the executor behind it
    /// drains the owner. Posts that waited for a holder are the executor's
    /// to drain once it holds the owner.
    pub fn hold(&mut self, waker: Waker) {
        self.state.lock().holder = Holder::Executor(waker);
    }

    /// Stops waking the executor; posts wait for the next holder.
    pub fn release(&mut self) {
        self.state.lock().holder = Holder::Nobody;
    }

    /// Takes every post queued so far, in the order they were posted.
    pub fn drain(&mut self) -> impl Iterator<Item = Post<C, R>> + use<C, R> {
        mem::take(&mut self.state.lock().posts).into_iter()
    }
}

/// Drops the posts not drained yet, so each reads unanswered, and refuses
/// every later post.
impl<C, R> Drop for Mailbox<C, R> {
    fn drop(&mut self) {
        let posts = {
            let mut state = self.state.lock();
            state.holder = Holder::Gone;
            mem::take(&mut state.posts)
        };
        drop(posts);
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{PostError, Refused, mailbox};
    use crate::wakes::waker;

    #[kithara::test]
    fn a_post_made_before_a_holder_waits_for_it() {
        let (postbox, mut mailbox) = mailbox::<u32, ()>();
        let (waker, wakes) = waker();

        drop(postbox.post(1).expect("an open mailbox takes the post"));
        mailbox.hold(waker);

        assert_eq!(wakes.count(), 0, "nobody held it to wake");
        assert_eq!(commands(&mut mailbox), [1]);
    }

    #[kithara::test]
    fn a_post_wakes_the_holder() {
        let (postbox, mut mailbox) = mailbox::<u32, ()>();
        let (waker, wakes) = waker();
        mailbox.hold(waker);

        drop(postbox.post(1).expect("a held mailbox takes the post"));

        assert_eq!(wakes.count(), 1, "one post, one wake");
    }

    #[kithara::test]
    fn the_holder_drains_posts_from_every_postbox_in_the_order_they_were_posted() {
        let (first, mut mailbox) = mailbox::<u32, ()>();
        let second = first.clone();
        mailbox.hold(waker().0);

        for (postbox, command) in [(&first, 1), (&second, 2), (&first, 3), (&second, 4)] {
            drop(
                postbox
                    .post(command)
                    .expect("a held mailbox takes the post"),
            );
        }

        assert_eq!(commands(&mut mailbox), [1, 2, 3, 4]);
        assert_eq!(mailbox.drain().count(), 0, "a drained post runs once");
    }

    #[kithara::test]
    fn posts_are_numbered_in_the_order_the_owner_drains_them() {
        let (first, mut mailbox) = mailbox::<(), u32>();
        let second = first.clone();
        let tickets = [&first, &second, &first, &second]
            .map(|postbox| postbox.post(()).expect("an open mailbox takes the post"));

        for (answer, post) in (1..).zip(mailbox.drain()) {
            post.answer.answer(Err(answer));
        }

        assert!(
            tickets.windows(2).all(|pair| pair[0].seq() < pair[1].seq()),
            "numbers grow in post order across postboxes"
        );
        let answers = tickets.map(super::Ticket::wait);
        assert_eq!(answers, [1, 2, 3, 4].map(|n| Err(Refused::Owner(n))));
    }

    #[kithara::test]
    fn an_answered_post_reads_applied() {
        let (postbox, mut mailbox) = mailbox::<u32, ()>();
        let ticket = postbox.post(1).expect("an open mailbox takes the post");

        for post in mailbox.drain() {
            post.answer.answer(Ok(()));
        }

        assert_eq!(ticket.wait(), Ok(()));
    }

    #[kithara::test]
    fn a_post_the_owner_drops_unanswered_reads_unanswered() {
        let (postbox, mut mailbox) = mailbox::<u32, ()>();
        let ticket = postbox.post(1).expect("an open mailbox takes the post");

        drop(mailbox.drain().collect::<Vec<_>>());

        assert_eq!(ticket.wait(), Err(Refused::Unanswered));
    }

    #[kithara::test]
    fn a_post_after_release_wakes_no_one_and_waits_for_the_next_holder() {
        let (postbox, mut mailbox) = mailbox::<u32, ()>();
        let (released, released_wakes) = waker();
        mailbox.hold(released);

        mailbox.release();
        drop(postbox.post(1).expect("an open mailbox takes the post"));

        assert_eq!(released_wakes.count(), 0);
        mailbox.hold(waker().0);
        assert_eq!(commands(&mut mailbox), [1]);
    }

    #[kithara::test]
    fn a_dropped_mailbox_leaves_its_posts_unanswered_and_closes_its_postboxes() {
        let (postbox, mailbox) = mailbox::<u32, ()>();
        let ticket = postbox.post(1).expect("an open mailbox takes the post");

        drop(mailbox);

        assert_eq!(ticket.wait(), Err(Refused::Unanswered));
        assert_eq!(postbox.post(2).err(), Some(PostError));
    }

    #[kithara::test]
    fn a_postbox_reads_closed_only_once_the_owner_drops_its_mailbox() {
        let (postbox, mut mailbox) = mailbox::<u32, ()>();
        assert!(!postbox.is_closed(), "nobody holds it yet, but it is open");
        mailbox.hold(waker().0);
        mailbox.release();
        assert!(!postbox.is_closed(), "a released mailbox stays open");

        drop(mailbox);

        assert!(postbox.is_closed());
    }

    /// Drains `mailbox` and gives back the commands, unanswered.
    fn commands(mailbox: &mut super::Mailbox<u32, ()>) -> Vec<u32> {
        mailbox.drain().map(|post| post.command).collect()
    }
}
