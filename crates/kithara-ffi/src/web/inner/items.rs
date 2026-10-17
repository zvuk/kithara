use std::collections::{HashMap, HashSet};

use kithara::{
    platform::{
        sync::Arc,
        tokio::{sync::oneshot, task::spawn},
    },
    play::Position,
    queue::{QueueCommand, TrackId},
};
use kithara_command::Seq;

use super::WasmInner;
use crate::{item::AudioPlayerItem, pools::FfiPools, types::FfiError};

/// Owns caller objects until the owner rejects admission or publishes their removal.
#[derive(Default)]
pub(in crate::web) struct ItemRegistry {
    items: HashMap<TrackId, Arc<AudioPlayerItem>>,
    pending: HashMap<Seq, Option<TrackId>>,
    waiting: HashMap<Seq, Vec<oneshot::Sender<()>>>,
}

impl ItemRegistry {
    fn begin(&mut self, seq: Seq, item: Option<&Arc<AudioPlayerItem>>) {
        let id = item.map(|item| item.track_id());
        if let Some(item) = item {
            self.items
                .entry(item.track_id())
                .or_insert_with(|| Arc::clone(item));
        }
        self.pending.insert(seq, id);
    }

    pub(in crate::web) fn get(&self, id: TrackId) -> Option<Arc<AudioPlayerItem>> {
        self.items.get(&id).cloned()
    }

    pub(super) fn admissions(&mut self) -> Vec<oneshot::Receiver<()>> {
        let pending: Vec<_> = self.pending.keys().copied().collect();
        pending
            .into_iter()
            .map(|seq| {
                let (sender, receiver) = oneshot::channel();
                self.waiting.entry(seq).or_default().push(sender);
                receiver
            })
            .collect()
    }

    pub(in crate::web) fn complete(
        &mut self,
        seq: Seq,
        members: &HashSet<TrackId>,
    ) -> (Vec<Arc<AudioPlayerItem>>, Vec<oneshot::Sender<()>>) {
        if self.pending.remove(&seq).is_none() {
            return (Vec::new(), Vec::new());
        }
        let completed = self.waiting.remove(&seq).unwrap_or_default();
        (self.reconcile(members), completed)
    }

    pub(in crate::web) fn reconcile(
        &mut self,
        members: &HashSet<TrackId>,
    ) -> Vec<Arc<AudioPlayerItem>> {
        let mut prime: Vec<Arc<AudioPlayerItem>> = Vec::new();
        let pending = &self.pending;
        self.items.retain(|id, item| {
            let present = members.contains(id);
            let was_inserted = {
                let mut inserted = item.inserted.lock();
                std::mem::replace(&mut *inserted, present)
            };
            if present && !was_inserted {
                prime.push(Arc::clone(item));
            }
            present || pending.values().any(|pending| *pending == Some(*id))
        });
        prime
    }
}

impl WasmInner {
    fn enqueue(
        &self,
        item: Option<&Arc<AudioPlayerItem>>,
        command: QueueCommand<FfiPools>,
    ) -> Result<(), FfiError> {
        let ticket = self.post(command)?;
        let seq = ticket.seq();
        self.items.lock().begin(seq, item);
        let routes = self.routes.clone();
        spawn(async move {
            let result = super::core::answer(ticket).await;
            routes.complete(seq);
            if let Err(error) = result {
                routes.error(error.to_string());
            }
        });
        Ok(())
    }

    pub(crate) fn append(&self, item: &Arc<AudioPlayerItem>) -> Result<(), FfiError> {
        let source = self.bridge.source(item.url())?;
        self.enqueue(
            Some(item),
            QueueCommand::Append {
                position: Position::ZERO,
                id: item.track_id(),
                source,
            },
        )
    }

    pub(crate) fn item_for_command(&self, id: TrackId) -> Option<Arc<AudioPlayerItem>> {
        self.items.lock().get(id)
    }

    pub(crate) fn current_item(&self) -> Option<Arc<AudioPlayerItem>> {
        let current = self.bridge.current_track_id()?;
        self.items.lock().get(current)
    }

    pub(crate) fn insert(
        &self,
        item: &Arc<AudioPlayerItem>,
        after: Option<&Arc<AudioPlayerItem>>,
    ) -> Result<(), FfiError> {
        let after = after.map(|item| item.track_id());
        let source = self.bridge.source(item.url())?;
        self.enqueue(
            Some(item),
            QueueCommand::Insert {
                id: item.track_id(),
                source,
                after,
            },
        )
    }

    pub(crate) fn item_count(&self) -> u32 {
        let len = self.bridge.queue().map_or(0, |queue| queue.len());
        u32::try_from(len).unwrap_or_else(|_| {
            tracing::error!(queue_len = len, "BUG: queue length exceeds u32::MAX");
            0
        })
    }

    pub(crate) fn items(&self) -> Vec<Arc<AudioPlayerItem>> {
        let Some(queue) = self.bridge.queue() else {
            return Vec::new();
        };
        let tracks = queue.tracks();
        let items = self.items.lock();
        tracks
            .iter()
            .filter_map(|track| items.get(track.id))
            .collect()
    }

    pub(crate) fn remove(&self, item: &AudioPlayerItem) -> Result<(), FfiError> {
        let id = item.track_id();
        self.enqueue(None, QueueCommand::Remove(id))
    }

    pub(crate) fn remove_all_items(&self) {
        self.report(self.enqueue(None, QueueCommand::RemoveAll));
    }

    pub(crate) fn replace_item(
        &self,
        index: u32,
        item: &Arc<AudioPlayerItem>,
    ) -> Result<(), FfiError> {
        let queue = self.bridge.control()?;
        let tracks = queue.tracks();
        let index = index as usize;
        let old = tracks
            .get(index)
            .ok_or_else(|| FfiError::InvalidArgument {
                reason: format!("item index {index} out of range (len: {})", tracks.len()),
            })?
            .id;
        let after = index
            .checked_sub(1)
            .and_then(|index| tracks.get(index))
            .map(|track| track.id);
        let source = self.bridge.source(item.url())?;
        let ticket = self.post(QueueCommand::Insert {
            id: item.track_id(),
            source,
            after,
        })?;
        let seq = ticket.seq();
        self.items.lock().begin(seq, Some(item));
        let routes = self.routes.clone();
        spawn(async move {
            let result = match super::core::answer(ticket).await {
                Ok(()) => match queue.post(QueueCommand::Remove(old)) {
                    Ok(ticket) => super::core::answer(ticket).await,
                    Err(error) => Err(FfiError::Internal {
                        description: error.to_string(),
                    }),
                },
                Err(error) => Err(error),
            };
            routes.complete(seq);
            if let Err(error) = result {
                routes.error(error.to_string());
            }
        });
        Ok(())
    }

    pub(crate) fn select(
        &self,
        item: &AudioPlayerItem,
        transition: crate::types::FfiTransition,
    ) -> Result<(), FfiError> {
        let id = item.track_id();
        self.submit(QueueCommand::Select {
            id,
            transition: transition.try_into()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, sync::OnceLock};

    use ::kithara::{
        platform::{sync::Mutex, tokio::sync::oneshot::error::TryRecvError},
        play::{
            DeckMixSettings, DeckPass, DeckProtocol, DeckSnapshot, DispatcherProtocol, HostedDeck,
            Outbox, PlayError, PlayWorker, PlayWorkerConfig, ResourceLoad, ResourcePrep,
            SessionOutputView,
        },
        queue::{QueueConfig, QueueError, TrackSource},
    };
    use kithara_command::{ChannelConfig, ScopedConfig, channel, scoped_channel};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        pools::FfiQueue,
        types::FfiItemConfig,
        web::{inner::core::answer, observer::router::Routes},
    };

    fn queue() -> FfiQueue {
        let worker = PlayWorker::new(
            PlayWorkerConfig::builder(crate::pools::build().expect("queue pools")).build(),
        );
        FfiQueue::new(
            QueueConfig::builder()
                .prep(ResourcePrep::builder().worker(worker).build())
                .build(),
        )
    }

    fn drain(queue: &mut FfiQueue) {
        let config = HostedDeck::mixer_config(queue);
        let (mut ring, _inbox) = scoped_channel::<DeckProtocol, DeckProtocol>(
            ScopedConfig::builder()
                .scope(
                    ChannelConfig::builder()
                        .targets(config.slots().get())
                        .build(),
                )
                .build(),
        );
        let scope = ring.open(config.slots().get()).expect("open deck scope");
        let (mut dispatcher, _opens) =
            channel::<DispatcherProtocol<ResourceLoad<FfiPools>>>(ChannelConfig::builder().build());
        let rate = NonZeroU32::new(48_000).expect("sample rate");
        let output = SessionOutputView::new(rate).get();
        let mut deck = DeckSnapshot::new(config);
        deck.sample_rate = rate.get();
        let pass = DeckPass {
            mix: DeckMixSettings::default(),
            suspended: false,
            now: Default::default(),
            delivery: Default::default(),
            output: &output,
            deck: &deck,
        };
        let mut scope = ring.scope(scope).expect("borrow deck scope");
        let mut out = Outbox::new(&mut scope, &mut dispatcher).in_pass(pass);
        HostedDeck::drain(queue, pass, &mut out);
    }

    #[kithara::test(wasm, flash(false))]
    async fn captured_admissions_keep_the_partial_replacement_until_both_operations_finish() {
        let mut queue = queue();
        let control = queue.control();
        let published = Arc::new(OnceLock::new());
        assert!(published.set(control.clone()).is_ok());
        let items = Arc::new(Mutex::new(ItemRegistry::default()));
        let routes = Routes::new(Arc::clone(&items), published);
        let old = AudioPlayerItem::new(FfiItemConfig::for_test("https://example.com/old.mp3"));
        let old_id = old.track_id();
        let old_weak = Arc::downgrade(&old);
        let ticket = control
            .post(QueueCommand::Append {
                position: Position::ZERO,
                id: old_id,
                source: TrackSource::Uri(old.url()),
            })
            .expect("post old item");
        let seq = ticket.seq();
        items.lock().begin(seq, Some(&old));
        drain(&mut queue);
        answer(ticket).await.expect("owner admits old item");
        routes.complete(seq);
        drop(old);

        let new = AudioPlayerItem::new(FfiItemConfig::for_test("https://example.com/new.mp3"));
        let new_id = new.track_id();
        let new_weak = Arc::downgrade(&new);
        let insert = control
            .post(QueueCommand::Insert {
                id: new_id,
                source: TrackSource::Uri(new.url()),
                after: None,
            })
            .expect("post replacement insertion");
        let lower = insert.seq();
        items.lock().begin(lower, Some(&new));
        drop(new);
        let remove = control
            .post(QueueCommand::Remove(old_id))
            .expect("post independent removal");
        let higher = remove.seq();
        items.lock().begin(higher, None);
        assert!(higher.get() > lower.get());
        let mut admissions = items.lock().admissions();
        assert_eq!(admissions.len(), 2);
        assert!(
            admissions
                .iter_mut()
                .all(|pending| matches!(pending.try_recv(), Err(TryRecvError::Empty)))
        );
        assert!(new_weak.upgrade().is_some());

        drain(&mut queue);
        answer(insert).await.expect("owner admits replacement");
        answer(remove).await.expect("owner removes old item");
        routes.complete(higher);
        assert_eq!(
            control
                .tracks()
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            vec![new_id]
        );
        assert!(old_weak.upgrade().is_none());
        assert!(new_weak.upgrade().is_some());
        assert!(items.lock().pending.contains_key(&lower));
        let mut pending: Vec<_> = admissions
            .into_iter()
            .filter_map(|mut admission| match admission.try_recv() {
                Ok(()) => None,
                Err(TryRecvError::Empty) => Some(admission),
                Err(TryRecvError::Closed) => panic!("admission sender dropped without completion"),
            })
            .collect();
        assert_eq!(
            pending.len(),
            1,
            "the lower composite still owns its admission"
        );

        let second_remove = control
            .post(QueueCommand::Remove(old_id))
            .expect("post composite removal");
        drain(&mut queue);
        let refusal = answer(second_remove)
            .await
            .expect_err("old item is already absent");
        assert!(matches!(
            refusal,
            FfiError::Internal { description }
                if description == QueueError::UnknownTrackId(old_id).to_string()
        ));
        assert!(matches!(pending[0].try_recv(), Err(TryRecvError::Empty)));
        routes.complete(lower);
        assert_eq!(pending[0].try_recv(), Ok(()));
        assert!(items.lock().pending.is_empty());
        assert!(new_weak.upgrade().is_some());
        assert!(
            items
                .lock()
                .get(new_id)
                .is_some_and(|item| *item.inserted.lock())
        );

        let clear = control.post(QueueCommand::RemoveAll).expect("post clear");
        let seq = clear.seq();
        items.lock().begin(seq, None);
        drain(&mut queue);
        answer(clear).await.expect("owner clears membership");
        routes.complete(seq);
        assert!(control.tracks().is_empty());
        assert!(new_weak.upgrade().is_none());
        assert!(items.lock().items.is_empty());
    }

    #[kithara::test(wasm, flash(false))]
    async fn dropping_the_unhosted_owner_refuses_admission_and_releases_caller_custody() {
        let queue = queue();
        let control = queue.control();
        let published = Arc::new(OnceLock::new());
        assert!(published.set(control.clone()).is_ok());
        let items = Arc::new(Mutex::new(ItemRegistry::default()));
        let routes = Routes::new(Arc::clone(&items), published);
        let item = AudioPlayerItem::new(FfiItemConfig::for_test("https://example.com/pending.mp3"));
        let id = item.track_id();
        let weak = Arc::downgrade(&item);
        let ticket = control
            .post(QueueCommand::Append {
                position: Position::ZERO,
                id,
                source: TrackSource::Uri(item.url()),
            })
            .expect("post before registration");
        let seq = ticket.seq();
        items.lock().begin(seq, Some(&item));
        let mut admissions = items.lock().admissions();
        assert_eq!(admissions.len(), 1);
        drop(item);
        assert!(weak.upgrade().is_some());
        assert!(control.tracks().is_empty());
        assert!(matches!(admissions[0].try_recv(), Err(TryRecvError::Empty)));

        drop(queue);
        let refusal = answer(ticket)
            .await
            .expect_err("dropped owner cannot admit the item");
        assert!(matches!(
            refusal,
            FfiError::Internal { description }
                if description == PlayError::Closed.to_string()
        ));
        assert!(control.is_closed());
        assert!(control.tracks().is_empty());
        assert!(weak.upgrade().is_some());
        routes.complete(seq);
        assert_eq!(admissions[0].try_recv(), Ok(()));
        assert!(weak.upgrade().is_none());
        let registry = items.lock();
        assert!(registry.items.is_empty());
        assert!(registry.pending.is_empty());
        assert!(registry.waiting.is_empty());
        assert!(matches!(
            control.post(QueueCommand::RemoveAll),
            Err(QueueError::Play(PlayError::Closed))
        ));
    }
}
