use kithara_app_library::AccessToken;
use kithara_net::Net;
use kithara_platform::{maybe_send::MaybeSend, time::Instant};

use super::core::{Faults, Load, Operation, Source, accept_playlists, playlists, unlist_playlists};
use crate::{Client, Command, Error, SearchScope, TrackId, job::Job, ui::request::Request};

impl<N: Net + Clone + 'static> Source<N> {
    /// Queues the current node's request for `due`, dropping the one in flight.
    pub(super) fn queue(&mut self, due: Instant) {
        self.queue_request(self.catalogue.request(), due);
    }

    fn queue_request(&mut self, request: Option<Request>, due: Instant) {
        self.load = request.map_or(Load::Idle, |request| Load::Due { due, request });
    }

    pub(super) fn start(&mut self) {
        if !self.streaming.loaded() {
            return;
        }
        let Load::Due { due, request } = &self.load else {
            return;
        };
        if *due > Instant::now() {
            return;
        }
        let request = request.clone();
        self.load = self
            .spawn(move |client, token| async move { request.load(&client, &token).await })
            .map_or(Load::Idle, Load::Running);
    }

    pub(super) fn change_scope(&mut self, scope: SearchScope) {
        if !self.catalogue.set_scope(scope) {
            return;
        }
        self.suggestions = None;
        self.queue(Instant::now());
        self.start_suggestions();
    }

    pub(super) fn choose_suggestion(&mut self, slot: usize) {
        let Some(query) = self.catalogue.suggestion(slot).map(str::to_owned) else {
            return;
        };
        if self.catalogue.search(&query) {
            self.suggestions = None;
            self.queue(Instant::now());
            self.start_suggestions();
        }
    }

    pub(super) fn open_entity(&mut self, action: &str) {
        if !self.catalogue.open(action) {
            return;
        }
        self.suggestions = None;
        self.faults.record(Operation::Page, Ok(()));
        self.queue(Instant::now());
    }

    pub(super) fn back(&mut self) {
        if !self.catalogue.back() {
            return;
        }
        self.load = Load::Idle;
        self.suggestions = None;
        self.faults.record(Operation::Page, Ok(()));
    }

    pub(super) fn load_more(&mut self) {
        if !matches!(self.load, Load::Idle) {
            return;
        }
        let Some(request) = self.catalogue.continuation() else {
            return;
        };
        self.faults.record(Operation::Page, Ok(()));
        self.queue_request(Some(request), Instant::now());
    }

    pub(super) fn start_suggestions(&mut self) {
        if self.suggestions.is_some() {
            return;
        }
        self.suggestions = self.catalogue.autocomplete_query().and_then(|query| {
            self.spawn(
                move |client, token| async move { client.autocomplete(&token, &query).await },
            )
        });
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
        self.suggestions = None;
        unlist_playlists(&mut self.branch);
        self.catalogue.clear();
        self.faults = Faults::default();
        self.queue(Instant::now());
        if self.expanded {
            self.list_playlists();
        }
        self.start_suggestions();
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
            if matches!(self.load, Load::Running(_) | Load::Enriching(_))
                || (reload && matches!(self.load, Load::Idle))
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
        if let Some(result) = self.suggestions.as_mut().and_then(Job::finished) {
            self.suggestions = None;
            if let Some(suggestions) = self.faults.record(Operation::Suggestions, result) {
                self.catalogue.accept_suggestions(suggestions);
            }
        }
        if let Load::Running(job) = &mut self.load
            && let Some(result) = job.finished()
        {
            self.load = Load::Idle;
            if let Some(loaded) = self.faults.record(Operation::Page, result) {
                let ids = self.catalogue.accept(loaded, self.streaming.adaptive);
                self.load = self
                    .spawn(move |client, token| async move { client.streams(&token, &ids).await })
                    .map_or(Load::Idle, Load::Enriching);
            }
        }
        if let Load::Enriching(job) = &mut self.load
            && let Some(result) = job.finished()
        {
            self.load = Load::Idle;
            if let Some(streams) = self.faults.record(Operation::Page, result) {
                self.catalogue.enrich(streams, self.streaming.adaptive);
            }
        }
    }
}
