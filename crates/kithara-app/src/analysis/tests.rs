use std::num::NonZeroU32;

use ::kithara::{
    analysis::{AnalysisFile, AnalysisProgress},
    assets::{
        AssetLayout, AssetLayoutRegistry, AssetResource, AssetSource, ReadSide, StorageBackend,
    },
    events::TrackId,
    file::File,
    platform::{
        CancelToken,
        sync::Arc,
        time::{self, Duration},
        tokio::sync::watch,
    },
};
use kithara_test_utils::kithara;

use super::{
    AnalysisService,
    entry::{Stage, complete_for},
    fixtures::{
        analysis, app_config, axis, fingerprint, grid, memory_store, mp3_track, other_axis,
        persistence, progress, queue, revision_held, revision_of, snapshot, test_pools, track,
        wav_track,
    },
    run::{Activity, Run},
    service::{Owner, resource_config_from_source},
};
use crate::{
    pools::{AppHost, AppPools, AppQueueControl, AppStore, AppTrackSource},
    wave_cache::{AnalysisTarget, token_for},
};

fn owner_in(cancel: &CancelToken, store: AppStore) -> Owner {
    let config = app_config(cancel, store);
    let persistence = persistence(cancel, test_pools());
    let (service, _handle) = AnalysisService::new(&config, persistence, cancel.child());
    service.owner
}

fn owner(cancel: &CancelToken) -> Owner {
    owner_in(cancel, memory_store())
}

fn target_of(owner: &Owner, source: &AppTrackSource) -> AnalysisTarget {
    let config = resource_config_from_source(source.clone(), &owner.config)
        .expect("source yields a resource");
    AnalysisTarget::for_config(&config).expect("source has an analysis target")
}

fn running_entry(owner: &Owner) -> Option<usize> {
    match owner.active.as_ref() {
        Some(Activity::Running(run)) => Some(run.entry),
        _ => None,
    }
}

fn running_track(owner: &Owner) -> Option<TrackId> {
    running_entry(owner).map(|index| owner.entries[index].track_id())
}

fn requeued(owner: &Owner) -> bool {
    matches!(owner.active.as_ref(), Some(Activity::Running(run)) if run.requeue)
}

fn pending_tracks(owner: &Owner) -> Vec<TrackId> {
    owner
        .pending
        .iter()
        .map(|&index| owner.entries[index].track_id())
        .collect()
}

/// Replace whatever pass the runner opened with a channel the test feeds.
fn take_over_run(
    owner: &mut Owner,
    value: Option<AnalysisProgress>,
) -> watch::Sender<Option<AnalysisProgress>> {
    let index = running_entry(owner).expect("a pass is in flight");
    owner.runner.clear();
    let (tx, rx) = watch::channel(value);
    owner.active = Some(Activity::Running(Run {
        entry: index,
        axis: axis(),
        rx,
        requeue: false,
    }));
    tx
}

/// A stored snapshot that covers only part of the track carries both
/// artifacts and no resume, so it looks finished to a check that counts
/// artifacts. It is not: the track is analysed to the end, not to where
/// an earlier pass happened to stop.
#[kithara::test(native, tokio, flash(false))]
async fn an_incomplete_stored_analysis_is_finished_rather_than_served_as_final() {
    let directory = tempfile::tempdir().expect("temporary track dir");
    let url = wav_track(directory.path(), 2);
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host, queue) = queue();
    let (track_id, source) = track(&queue, 1, &url);
    let partial = snapshot(
        "test-track".into(),
        3,
        400,
        owner.runner.fingerprint().clone(),
        Some(grid()),
    );
    assert!(!partial.is_complete());
    owner
        .cache
        .put(target_of(&owner, &source), progress(partial));

    let rx = owner.subscribe(queue, track_id, source, axis());

    assert_eq!(
        revision_held(&rx),
        Some(3),
        "the partial snapshot is served as far as it goes"
    );
    assert_eq!(
        running_track(&owner),
        Some(track_id),
        "and a pass finishes it"
    );
    settle(&mut owner).await;
    let held = rx.borrow().clone().expect("the deck holds the final value");
    assert!(
        held.analysis().is_complete(),
        "the finished value reaches the deck"
    );
    cancel.cancel();
}

/// A hit whose configuration expects an artifact it lacks is served and
/// refilled by a pass for the same entry.
#[kithara::test(native, tokio)]
async fn a_hit_missing_an_artifact_is_served_and_refilled() {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let fingerprint = owner.runner.fingerprint().clone();
    assert!(
        fingerprint.beat().is_some(),
        "fixture needs an artifact to omit"
    );
    let (_host, queue) = queue();
    let (track_id, source) = track(&queue, 1, "file:///tmp/track-1.mp3");
    owner.cache.put(
        target_of(&owner, &source),
        progress(snapshot("test-track".into(), 7, 1_000, fingerprint, None)),
    );

    let rx = owner.subscribe(queue, track_id, source, axis());

    assert_eq!(revision_held(&rx), Some(7), "the hit is served");
    assert_eq!(
        running_track(&owner),
        Some(track_id),
        "the artifact is refilled"
    );
    cancel.cancel();
}

#[kithara::test(native, tokio)]
async fn an_entry_is_held_only_while_a_deck_keeps_its_receiver() {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host, queue) = queue();
    let (track_id, source) = track(&queue, 1, "file:///tmp/track-1.mp3");

    let rx = owner.subscribe(queue, track_id, source, axis());
    assert!(owner.entries[0].is_held());

    drop(rx);
    assert!(
        !owner.entries[0].is_held(),
        "the owner's own handle is no receiver"
    );
    cancel.cancel();
}

#[kithara::test(native, tokio)]
async fn a_complete_hit_is_served_without_a_pass() {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host, queue) = queue();
    let (track_id, source) = track(&queue, 1, "file:///tmp/track-1.mp3");
    let complete = snapshot(
        "test-track".into(),
        5,
        1_000,
        owner.runner.fingerprint().clone(),
        Some(grid()),
    );
    owner
        .cache
        .put(target_of(&owner, &source), progress(complete));

    let rx = owner.subscribe(queue, track_id, source, axis());

    assert_eq!(revision_held(&rx), Some(5));
    assert!(owner.active.is_none(), "nothing is left to analyse");
    assert!(owner.pending.is_empty());
    cancel.cancel();
}

/// The run publishes for the track it was opened on. Which track the
/// player reports as current is playback state; it does not decide whether
/// a deck holding the analysed track gets to see the revision.
#[kithara::test(native, tokio)]
async fn every_revision_reaches_the_deck_that_holds_the_track() {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host, queue) = queue();
    let (_playing, _) = track(&queue, 8, "file:///tmp/track-8.mp3");
    let (held, source) = track(&queue, 7, "file:///tmp/track-7.mp3");
    assert_eq!(
        queue.current_index(),
        Some(0),
        "the player sits on another track"
    );
    let rx = owner.subscribe(queue, held, source, axis());
    let tx = take_over_run(&mut owner, None);

    tx.send(Some(progress(revision_of(1))))
        .expect("run publishes");
    owner.publish();
    assert_eq!(revision_held(&rx), Some(1));
    tx.send(Some(progress(revision_of(2))))
        .expect("run publishes");
    owner.publish();

    assert_eq!(
        revision_held(&rx),
        Some(2),
        "the deck holding the track sees the latest revision"
    );
    cancel.cancel();
}

#[kithara::test(native, tokio)]
async fn two_decks_holding_one_track_share_one_pass() {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host_a, queue_a) = queue();
    let (_host_b, queue_b) = queue();
    let (track_a, source_a) = track(&queue_a, 1, "file:///tmp/shared.mp3");
    let (track_b, source_b) = track(&queue_b, 2, "file:///tmp/shared.mp3");

    let rx_a = owner.subscribe(queue_a, track_a, source_a, axis());
    let tx = take_over_run(&mut owner, None);
    let rx_b = owner.subscribe(queue_b, track_b, source_b, axis());

    assert_eq!(owner.entries.len(), 1, "one resource, one entry");
    assert!(!requeued(&owner), "the pass in flight serves both decks");
    assert!(owner.pending.is_empty());
    tx.send(Some(progress(revision_of(1))))
        .expect("run publishes");
    owner.publish();
    assert_eq!(revision_held(&rx_a), Some(1));
    assert_eq!(revision_held(&rx_b), Some(1));
    cancel.cancel();
}

#[kithara::test(native, tokio)]
async fn a_held_track_preempts_a_background_run() {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host, queue) = queue();
    let (background, _) = track(&queue, 1, "file:///tmp/track-1.mp3");
    let (held, source) = track(&queue, 2, "file:///tmp/track-2.mp3");
    owner.warm(&queue, &[background], axis());
    assert_eq!(running_track(&owner), Some(background));
    let tx = take_over_run(&mut owner, None);

    let _rx = owner.subscribe(queue, held, source, axis());

    assert_eq!(
        running_track(&owner),
        Some(background),
        "the ended pass stays owned until its channel closes"
    );
    assert!(requeued(&owner), "and goes back in line");
    assert_eq!(pending_tracks(&owner), vec![held]);

    drop(tx);
    owner.drive().await;
    assert_eq!(
        running_track(&owner),
        Some(held),
        "the held track takes the runner"
    );
    assert_eq!(pending_tracks(&owner), vec![background]);
    cancel.cancel();
}

#[kithara::test(native, tokio)]
async fn a_background_track_waits_for_a_held_one() {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host, queue) = queue();
    let (held, source) = track(&queue, 1, "file:///tmp/track-1.mp3");
    let (background, _) = track(&queue, 2, "file:///tmp/track-2.mp3");
    let (later, later_source) = track(&queue, 3, "file:///tmp/track-3.mp3");
    let _rx = owner.subscribe(queue.clone(), held, source, axis());
    let _tx = take_over_run(&mut owner, None);

    owner.warm(&queue, &[background], axis());
    assert_eq!(running_track(&owner), Some(held));
    assert!(!requeued(&owner), "a warm request ends no pass");

    let _later_rx = owner.subscribe(queue, later, later_source, axis());
    assert!(
        !requeued(&owner),
        "a held pass is not preempted by another held track"
    );
    drop(_tx);
    owner.drive().await;
    assert_eq!(
        running_track(&owner),
        Some(later),
        "the held track takes the runner before the warm one"
    );
    drop(take_over_run(&mut owner, None));
    owner.drive().await;
    assert_eq!(
        running_track(&owner),
        Some(background),
        "the warm track follows"
    );
    cancel.cancel();
}

#[kithara::test(native, tokio)]
async fn a_pass_restarts_on_the_axis_the_next_request_names() {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host, queue) = queue();
    let (track_id, source) = track(&queue, 1, "file:///tmp/track-1.mp3");
    let _rx = owner.subscribe(queue.clone(), track_id, source.clone(), axis());
    let tx = take_over_run(&mut owner, None);

    let _again = owner.subscribe(queue, track_id, source, other_axis());
    assert_eq!(running_track(&owner), Some(track_id));
    assert!(
        requeued(&owner),
        "the stale pass ends and the entry waits for its close"
    );

    drop(tx);
    owner.drive().await;
    let Some(Activity::Running(run)) = owner.active.as_ref() else {
        panic!("the pass reopens after the old run closes");
    };
    assert_eq!(owner.entries[run.entry].track_id(), track_id);
    assert_eq!(run.axis, other_axis(), "on the axis the request named");
    cancel.cancel();
}

#[kithara::test(native, tokio)]
async fn preemption_commits_a_checkpoint_before_starting_the_next_track() {
    let directory = tempfile::tempdir().expect("temporary analysis store");
    let pools = test_pools();
    let store = AppStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: directory.path().into(),
        })
        .build();
    let cancel = CancelToken::root();
    let mut owner = owner_in(&cancel, store.clone());
    let (_host, queue) = queue();
    let (track_a, source_a) = track(&queue, 1, "file:///tmp/track-a.mp3");
    let (track_b, source_b) = track(&queue, 2, "file:///tmp/track-b.mp3");
    let target = target_of(&owner, &source_a);
    let rx_a = owner.subscribe(queue.clone(), track_a, source_a, axis());
    let publication = take_over_run(&mut owner, Some(progress(analysis())));
    drop(rx_a);

    let _rx_b = owner.subscribe(queue, track_b, source_b, axis());
    assert_eq!(running_track(&owner), Some(track_a));

    drop(publication);
    owner.drive().await;
    assert!(matches!(owner.active, Some(Activity::Committing(_))));
    assert_eq!(pending_tracks(&owner), vec![track_b, track_a]);

    owner.drive().await;
    assert_eq!(running_track(&owner), Some(track_b));

    let reader = store
        .open_resource(target.key(), None)
        .expect("acknowledged checkpoint is committed");
    let mut bytes = pools.get::<u8>();
    reader
        .read_into(&mut bytes)
        .expect("committed checkpoint reads");
    let restored =
        AnalysisFile::parse(&bytes, &fingerprint()).expect("committed checkpoint validates");
    assert_eq!(restored.latest().analysis().revision(), 1);
    cancel.cancel();
}

/// One deck holding one track whose pass the test feeds.
struct HeldRun {
    _host: AppHost,
    queue: AppQueueControl,
    owner: Owner,
    track_id: TrackId,
    source: AppTrackSource,
    target: AnalysisTarget,
    rx: watch::Receiver<Option<AnalysisProgress>>,
}

/// Subscribe for one track and close its pass holding `value`.
async fn close_run(cancel: &CancelToken, url: &str, value: Option<AnalysisProgress>) -> HeldRun {
    let mut owner = owner(cancel);
    let (host, queue) = queue();
    let (track_id, source) = track(&queue, 1, url);
    let target = target_of(&owner, &source);
    let rx = owner.subscribe(queue.clone(), track_id, source.clone(), axis());
    let tx = take_over_run(&mut owner, value);

    drop(tx);
    owner.drive().await;
    HeldRun {
        _host: host,
        queue,
        owner,
        track_id,
        source,
        target,
        rx,
    }
}

#[kithara::test(native, tokio)]
async fn a_close_carrying_a_complete_value_publishes_and_caches_it() {
    let cancel = CancelToken::root();
    let mut run = close_run(
        &cancel,
        "file:///tmp/track-1.mp3",
        Some(progress(analysis())),
    )
    .await;

    assert_eq!(revision_held(&run.rx), Some(1));
    assert!(run.owner.cache.get(&run.target, axis()).is_some());
    assert_eq!(
        run.owner.entries[0].stage(),
        Stage::Ended(axis()),
        "the pass ran its course"
    );
    cancel.cancel();
}

/// A pass that closes before it publishes (a reader that failed to open, a
/// cancelled open) settles nothing: the next deck to ask runs it again.
#[kithara::test(native, tokio)]
async fn a_close_without_a_value_is_retried_on_the_next_subscribe() {
    let cancel = CancelToken::root();
    let mut run = close_run(&cancel, "file:///tmp/track-1.mp3", None).await;

    assert_eq!(revision_held(&run.rx), None);
    assert!(
        run.owner.cache.get(&run.target, axis()).is_none(),
        "a run that closes with no value caches nothing"
    );
    assert_eq!(run.owner.entries[0].stage(), Stage::Idle);

    let _again = run
        .owner
        .subscribe(run.queue.clone(), run.track_id, run.source.clone(), axis());
    assert_eq!(
        running_track(&run.owner),
        Some(run.track_id),
        "the track is retried on the next subscribe"
    );
    cancel.cancel();
}

/// The first resumable publication of a real pass over `url`.
async fn resumable_progress(url: &str) -> AnalysisProgress {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host, queue) = queue();
    let (track_id, source) = track(&queue, 1, url);
    let rx = owner.subscribe(queue, track_id, source, axis());
    loop {
        time::timeout(Duration::from_secs(2), owner.drive())
            .await
            .expect("the pass progresses");
        let held = rx.borrow().clone();
        if let Some(progress) = held.filter(AnalysisProgress::is_resumable) {
            cancel.cancel();
            return progress;
        }
        assert!(owner.active.is_some(), "the pass published no checkpoint");
    }
}

/// A pass that closes on an unsettled value did not run its course; the
/// checkpoint is kept and the next deck to ask resumes it.
#[kithara::test(native, tokio, flash(false))]
async fn a_close_on_an_unsettled_value_is_resumed_on_the_next_subscribe() {
    let directory = tempfile::tempdir().expect("temporary track dir");
    let url = wav_track(directory.path(), 12);
    let checkpoint = resumable_progress(&url).await;
    let cancel = CancelToken::root();
    let mut run = close_run(&cancel, &url, Some(checkpoint.clone())).await;

    assert_eq!(
        revision_held(&run.rx),
        Some(checkpoint.analysis().revision())
    );
    assert!(run.owner.cache.get(&run.target, axis()).is_some());
    assert_eq!(run.owner.entries[0].stage(), Stage::Idle);

    let _again = run
        .owner
        .subscribe(run.queue.clone(), run.track_id, run.source.clone(), axis());
    assert_eq!(run.owner.entries[0].stage(), Stage::Queued);
    run.owner.drive().await;
    assert_eq!(
        running_track(&run.owner),
        Some(run.track_id),
        "the checkpoint is resumed once its commit lands"
    );
    cancel.cancel();
}

/// A checkpoint the runner rejects is no seed to resume from: the track gets
/// a fresh pass, which finishes it.
#[kithara::test(native, tokio, flash(false))]
async fn a_rejected_checkpoint_opens_a_fresh_pass() {
    let directory = tempfile::tempdir().expect("temporary track dir");
    let url = wav_track(directory.path(), 12);
    let checkpoint = resumable_progress(&url).await;
    let cancel = CancelToken::root();
    let mut config = app_config(&cancel, memory_store());
    config.analysis_chunk_seconds = NonZeroU32::new(7).expect("fixture chunk is non-zero");
    let (service, _handle) =
        AnalysisService::new(&config, persistence(&cancel, test_pools()), cancel.child());
    let mut owner = service.owner;
    let (_host, queue) = queue();
    let (track_id, source) = track(&queue, 1, &url);
    let target = target_of(&owner, &source);
    assert!(
        owner
            .runner
            .resume(
                resource_config_from_source(source.clone(), &owner.config).expect("resource"),
                checkpoint.clone(),
                |_| {}
            )
            .is_err(),
        "the fixture checkpoint is rejected on another chunk size"
    );
    owner.runner.clear();
    owner.cache.put(target, checkpoint.clone());

    let rx = owner.subscribe(queue, track_id, source, axis());

    assert_eq!(
        revision_held(&rx),
        Some(checkpoint.analysis().revision()),
        "the checkpoint is served as far as it goes"
    );
    assert_eq!(
        running_track(&owner),
        Some(track_id),
        "and a fresh pass opens"
    );
    settle(&mut owner).await;
    let held = rx.borrow().clone().expect("the deck holds the final value");
    assert!(held.analysis().is_complete(), "which finishes the track");
    assert!(held.analysis().revision() > checkpoint.analysis().revision());
    cancel.cancel();
}

/// `Queued` means in line: an entry the runner passed over without a run
/// does not keep the stage.
#[kithara::test(native, tokio)]
async fn an_entry_is_queued_only_while_it_is_in_line() {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host, queue) = queue();
    let (track_id, source) = track(&queue, 1, "file:///tmp/track-1.mp3");
    let complete = snapshot(
        "test-track".into(),
        5,
        1_000,
        owner.runner.fingerprint().clone(),
        Some(grid()),
    );
    owner
        .cache
        .put(target_of(&owner, &source), progress(complete));

    owner.warm(&queue, &[track_id], axis());

    assert!(owner.active.is_none(), "nothing is left to analyse");
    assert!(owner.pending.is_empty());
    assert_ne!(owner.entries[0].stage(), Stage::Queued);
    cancel.cancel();
}

/// The cache tiers are the store. An entry no deck holds keeps no value of
/// its own once its run ends; the next subscribe seeds it from the cache.
#[kithara::test(native, tokio)]
async fn a_finished_background_entry_holds_its_value_only_in_the_cache() {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host, queue) = queue();
    let (track_id, source) = track(&queue, 1, "file:///tmp/track-1.mp3");
    let target = target_of(&owner, &source);
    owner.warm(&queue, &[track_id], axis());
    let tx = take_over_run(&mut owner, Some(progress(analysis())));

    drop(tx);
    owner.drive().await;

    assert!(owner.cache.get(&target, axis()).is_some());
    assert!(
        owner.entries[0].value_for(axis()).is_none(),
        "the entry itself holds nothing"
    );
    let rx = owner.subscribe(queue, track_id, source, axis());
    assert_eq!(
        revision_held(&rx),
        Some(1),
        "a deck is served from the cache"
    );
    cancel.cancel();
}

/// A warm request creates entries without reading the cache; the seed is
/// read when the run is about to open.
#[kithara::test(native, tokio)]
async fn warm_seeds_nothing_before_the_run_opens() {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host, queue) = queue();
    let (track_id, source) = track(&queue, 1, "file:///tmp/track-1.mp3");
    let complete = snapshot(
        "test-track".into(),
        5,
        1_000,
        owner.runner.fingerprint().clone(),
        Some(grid()),
    );
    owner
        .cache
        .put(target_of(&owner, &source), progress(complete));
    let _busy_rx = {
        let (busy, busy_source) = track(&queue, 2, "file:///tmp/track-2.mp3");
        owner.subscribe(queue.clone(), busy, busy_source, axis())
    };
    take_over_run(&mut owner, None);

    owner.warm(&queue, &[track_id], axis());

    assert!(
        owner.entries[1].value_for(axis()).is_none(),
        "the warm entry waits in line without a value"
    );
    assert_eq!(owner.entries[1].stage(), Stage::Queued);
    cancel.cancel();
}

/// A held entry keeps the requester that holds it; a background warm from
/// another deck does not move where the producer is handed.
#[kithara::test(native, tokio)]
async fn a_background_warm_keeps_the_holder_of_a_held_entry() {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host_a, queue_a) = queue();
    let (_host_b, queue_b) = queue();
    let (track_a, source_a) = track(&queue_a, 1, "file:///tmp/shared.mp3");
    let (track_b, _) = track(&queue_b, 2, "file:///tmp/shared.mp3");
    let _rx = owner.subscribe(queue_a, track_a, source_a, axis());

    owner.warm(&queue_b, &[track_b], axis());

    assert_eq!(owner.entries.len(), 1, "one resource, one entry");
    assert_eq!(owner.entries[0].track_id(), track_a);
    cancel.cancel();
}

/// Drive the owner until the runner is idle again.
async fn settle(owner: &mut Owner) {
    while owner.active.is_some() {
        time::timeout(Duration::from_secs(2), owner.drive())
            .await
            .expect("the pass progresses");
    }
}

/// A fresh pass finishing a partial snapshot publishes above the revision
/// the deck already holds, so its final revision is never mistaken for
/// the partial one.
#[kithara::test(native, tokio, flash(false))]
async fn a_fresh_pass_publishes_above_the_seeded_revision() {
    let directory = tempfile::tempdir().expect("temporary track dir");
    let url = wav_track(directory.path(), 2);
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host, queue) = queue();
    let (track_id, source) = track(&queue, 1, &url);
    let target = target_of(&owner, &source);
    let partial = snapshot(
        token_for(target.key()),
        3,
        400,
        owner.runner.fingerprint().clone(),
        Some(grid()),
    );
    owner.cache.put(target, progress(partial));

    let rx = owner.subscribe(queue, track_id, source, axis());
    assert_eq!(revision_held(&rx), Some(3));
    settle(&mut owner).await;

    let held = rx.borrow().clone().expect("the deck holds the final value");
    assert!(held.analysis().is_complete(), "the pass finished the track");
    assert!(
        held.analysis().revision() > 3,
        "the final revision outranks the seeded one: {}",
        held.analysis().revision()
    );
    cancel.cancel();
}

#[derive(Debug)]
struct InvalidLayout;

impl AssetLayout for InvalidLayout {
    fn path(&self, _resource: &AssetResource) -> String {
        "../escape".to_string()
    }

    fn root(&self, _source: &AssetSource) -> String {
        "root".to_string()
    }
}

#[kithara::test(native, tokio)]
async fn an_invalid_layout_yields_no_analysis() {
    let layouts = AssetLayoutRegistry::default().with::<File<AppPools>>(Arc::new(InvalidLayout));
    let store = AppStore::builder(test_pools())
        .backend(StorageBackend::Memory)
        .layouts(layouts)
        .build();
    let cancel = CancelToken::root();
    let mut owner = owner_in(&cancel, store);
    let (_host, queue) = queue();
    let (track_id, source) = track(&queue, 1, "file:///tmp/invalid.mp3");

    let mut rx = owner.subscribe(queue, track_id, source, axis());

    assert!(revision_held(&rx).is_none(), "the deck shows nothing");
    assert!(rx.changed().await.is_err(), "and nothing will come");
    assert!(owner.entries.is_empty());
    cancel.cancel();
}

/// An mp3 claims its frame count, encoder padding included, and decodes to
/// less. The track ends where its source ends: a pass that read all of it
/// completes the track, and nothing asks for it again.
#[kithara::test(native, tokio, flash(false))]
async fn a_track_shorter_than_its_header_claims_is_completed() {
    let directory = tempfile::tempdir().expect("temporary track dir");
    let url = mp3_track(directory.path());
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (_host, queue) = queue();
    let (track_id, source) = track(&queue, 1, &url);

    let rx = owner.subscribe(queue, track_id, source, axis());
    settle(&mut owner).await;

    let held = rx.borrow().clone().expect("the deck holds the final value");
    let analysis = held.analysis();
    assert_eq!(
        analysis.extent(),
        Some(analysis.coverage().frontier()),
        "the extent is where the source ended, whatever its header claimed"
    );
    assert!(
        analysis.is_complete(),
        "the track is covered: {:?}",
        analysis.missing()
    );
    assert!(
        complete_for(&held, owner.runner.fingerprint()),
        "nothing is left for another pass"
    );
    cancel.cancel();
}
