use kithara_app_library::AccessToken;
use kithara_net::Net;
use kithara_platform::{maybe_send::MaybeSend, time::Instant};

use super::core::{Faults, Load, Operation, Source, accept_playlists, playlists, unlist_playlists};
use crate::{Client, Command, Error, TrackId, job::Job, ui::request::Batch};

impl<N: Net + Clone + 'static> Source<N> {
    /// Queues the current node's request for `due`, dropping the one in flight.
    pub(super) fn queue(&mut self, due: Instant) {
        self.load = if self.catalogue.request().is_some() {
            Load::Due(due)
        } else {
            Load::Idle
        };
    }

    pub(super) fn start(&mut self) {
        if !self.streaming.loaded() {
            return;
        }
        let Load::Due(due) = self.load else {
            return;
        };
        if due > Instant::now() {
            return;
        }
        self.load = self
            .catalogue
            .request()
            .and_then(|(mode, query)| {
                self.spawn(
                    move |client, token| async move { mode.load(&client, &token, &query).await },
                )
            })
            .map_or(Load::Idle, Load::Running);
    }

    /// Runs `work` with the account's current token, if any; a refusal that
    /// arrives before the job is dropped reaches the account.
    fn spawn<T, W, F>(&self, work: W) -> Option<Job<Result<T, Error>>>
    where
        T: MaybeSend + 'static,
        W: FnOnce(Client<N>, AccessToken) -> F,
        F: Future<Output = Result<T, Error>> + MaybeSend + 'static,
    {
        let token = self.account.token.borrow().clone()?;
        let commands = self.account.commands.clone();
        let operation = work(self.client.clone(), token.clone());
        Some(Job::spawn(&self.runtime, &self.cancel, async move {
            let result = operation.await;
            if let Err(Error::AuthenticationRejected) = result {
                let _ = commands.send(Command::Rejected(token));
            }
            result
        }))
    }

    /// Drops the work of the previous token, then reloads the current node
    /// and the expanded playlists; signed out, the page drops its rows.
    pub(super) fn reconnect(&mut self) {
        self.listing = None;
        self.likes.clear();
        unlist_playlists(&mut self.branch);
        if !self.has_token {
            self.catalogue.clear();
        }
        self.faults = Faults::default();
        self.queue(Instant::now());
        if self.expanded {
            self.list_playlists();
        }
    }

    pub(super) fn list_playlists(&mut self) {
        if self.listing.is_none() && playlists(&mut self.branch).is_some_and(|node| node.unlisted) {
            self.listing =
                self.spawn(|client, token| async move { client.playlists(&token).await });
        }
    }

    pub(super) fn like(&mut self, id: &str) {
        let Some((id, liked)) = self.catalogue.reaction(id) else {
            return;
        };
        if self.likes.contains_key(&id) {
            return;
        }
        let track = id.clone();
        if let Some(job) = self.spawn(move |client, token| async move {
            client
                .set_liked(&token, &track, liked)
                .await
                .map(|()| liked)
        }) {
            self.likes.insert(id, job);
        }
    }

    /// Takes the results of finished jobs; a reaction confirmed while the
    /// page loads reloads the page, whose snapshot may predate it.
    pub(super) fn settle(&mut self) {
        let confirmed: Vec<(TrackId, Result<bool, Error>)> = self
            .likes
            .iter_mut()
            .filter_map(|(id, job)| job.finished().map(|result| (id.clone(), result)))
            .collect();
        for (id, result) in confirmed {
            self.likes.remove(&id);
            let Some(liked) = self.faults.record(Operation::Like, result) else {
                continue;
            };
            let reload = self.catalogue.confirm_reaction(&id, liked);
            if matches!(self.load, Load::Running(_)) || (reload && matches!(self.load, Load::Idle))
            {
                self.queue(Instant::now());
            }
        }
        if let Some(result) = self.listing.as_mut().and_then(Job::finished) {
            self.listing = None;
            if let Some(listed) = self.faults.record(Operation::Playlists, result) {
                accept_playlists(&mut self.branch, listed);
            }
        }
        if let Load::Running(job) = &mut self.load
            && let Some(result) = job.finished()
        {
            self.load = Load::Idle;
            if let Some(batch) = self.faults.record(Operation::Page, result) {
                self.accept(batch);
            }
        }
    }

    fn accept(&mut self, batch: Batch) {
        let streams = self
            .faults
            .record(Operation::Page, batch.streams)
            .unwrap_or_default();
        self.catalogue
            .accept(batch.page, streams, self.streaming.adaptive);
    }
}
