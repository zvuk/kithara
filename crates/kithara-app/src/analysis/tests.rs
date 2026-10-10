use std::num::NonZeroU32;

use ::kithara::{
    analysis::{AnalysisFile, AnalysisProgress, FrameCoverage, FrameSpan, TrackAnalysis, Waveform},
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
use kithara_test_utils::{TestTempDir, kithara, off_thread::OffThread, temp_dir};

use super::{
    AnalysisService, TrackArtifacts,
    entry::Stage,
    fixtures::{
        analysis, app_config, axis, beats_only, document, fingerprint, grid, long_wav,
        memory_store, other_axis, persistence, progress, queue_off, queue_off_named, revision_held,
        revision_of, rhythm_a_mp3, rhythm_b_mp3, served_grid, served_waveform, short_wav, snapshot,
        test_pools, tone_mp3, track, track_prepared, track_sourced,
    },
    run::{Activity, Run},
    service::{Owner, resource_config_from_source},
    supply::Prepared,
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

fn take_over_run(
    owner: &mut Owner,
    value: Option<AnalysisProgress>,
) -> watch::Sender<Option<AnalysisProgress>> {
    let index = running_entry(owner).expect("a pass is in flight");
    owner.runner.clear();
    let (tx, rx) = watch::channel(value);
    owner.active = Some(Activity::Running(Run {
        rx,
        entry: index,
        axis: axis(),
        requeue: false,
    }));
    tx
}

#[kithara::test(native, tokio)]
async fn a_settled_hit_with_a_gap_is_served_without_a_pass(tone_mp3: String) {
    let cancel = CancelToken::root();
    let config = app_config(&cancel, memory_store());
    let (mut service, handle) =
        AnalysisService::new(&config, persistence(&cancel, test_pools()), cancel.child());
    let owner = &mut service.owner;
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, &tone_mp3).await;
    let settled = snapshot(
        "test-track".into(),
        3,
        400,
        owner.runner.fingerprint().clone(),
        Some(grid()),
    );
    assert!(!settled.is_complete(), "a gap is left in the track");
    owner
        .cache
        .put(target_of(owner, &source), progress(settled));

    let rx = owner.subscribe(queue.clone(), track_id, source, axis());

    assert_eq!(revision_held(&rx), Some(3), "the hit is served as final");
    assert_eq!(
        owner
            .bpms()
            .get(&crate::catalog::canonical_source(&tone_mp3)),
        Some(&128.0)
    );
    drop(rx);
    owner.entries[0].release();
    assert_eq!(
        owner
            .bpms()
            .get(&crate::catalog::canonical_source(&tone_mp3)),
        Some(&128.0)
    );
    assert!(owner.active.is_none(), "nothing is left to analyse");
    assert!(owner.pending.is_empty());
    ::kithara::platform::tokio::task::spawn(service.run());
    handle.warm(queue, vec![track_id], axis()).await;
    for _ in 0..2_000 {
        if handle
            .bpms()
            .contains_key(&crate::catalog::canonical_source(&tone_mp3))
        {
            break;
        }
        time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(
        handle
            .bpms()
            .get(&crate::catalog::canonical_source(&tone_mp3)),
        Some(&128.0)
    );
    cancel.cancel();
    host.close().await;
}

#[kithara::test(native, tokio)]
async fn a_hit_missing_an_artifact_is_served_and_refilled(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let fingerprint = owner.runner.fingerprint().clone();
    assert!(
        fingerprint.beat().is_some(),
        "fixture needs an artifact to omit"
    );
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, &tone_mp3).await;
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
    host.close().await;
}

#[kithara::test(native, tokio)]
async fn an_entry_is_held_only_while_a_deck_keeps_its_receiver(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, &tone_mp3).await;

    let rx = owner.subscribe(queue, track_id, source, axis());
    assert!(owner.entries[0].is_held());

    drop(rx);
    assert!(
        !owner.entries[0].is_held(),
        "the owner's own handle is no receiver"
    );
    cancel.cancel();
    host.close().await;
}

#[kithara::test(native, tokio)]
async fn a_complete_hit_is_served_without_a_pass(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, &tone_mp3).await;
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
    host.close().await;
}

#[kithara::test(native, tokio)]
async fn every_revision_reaches_the_deck_that_holds_the_track(
    tone_mp3: String,
    rhythm_a_mp3: String,
) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (_other, _) = track(&host, 8, &rhythm_a_mp3).await;
    let (held, source) = track(&host, 7, &tone_mp3).await;
    assert_ne!(
        queue.current_index(),
        Some(1),
        "the deck holds the track without playing it"
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
    host.close().await;
}

#[kithara::test(native, tokio)]
async fn two_decks_holding_one_track_share_one_pass(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host_a, queue_a) = queue_off_named("app-host-a").await;
    let (host_b, queue_b) = queue_off_named("app-host-b").await;
    let (track_a, source_a) = track(&host_a, 1, &tone_mp3).await;
    let (track_b, source_b) = track(&host_b, 2, &tone_mp3).await;

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
    host_a.close().await;
    host_b.close().await;
}

#[kithara::test(native, tokio)]
async fn a_held_track_preempts_a_background_run(tone_mp3: String, rhythm_a_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (background, _) = track(&host, 1, &tone_mp3).await;
    let (held, source) = track(&host, 2, &rhythm_a_mp3).await;
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
    host.close().await;
}

#[kithara::test(native, tokio)]
async fn a_background_track_waits_for_a_held_one(
    tone_mp3: String,
    rhythm_a_mp3: String,
    rhythm_b_mp3: String,
) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (held, source) = track(&host, 1, &tone_mp3).await;
    let (background, _) = track(&host, 2, &rhythm_a_mp3).await;
    let (later, later_source) = track(&host, 3, &rhythm_b_mp3).await;
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
    host.close().await;
}

#[kithara::test(native, tokio)]
async fn a_pass_restarts_on_the_axis_the_next_request_names(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, &tone_mp3).await;
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
    host.close().await;
}

#[kithara::test(native, tokio)]
async fn preemption_commits_a_checkpoint_before_starting_the_next_track(
    tone_mp3: String,
    rhythm_a_mp3: String,
) {
    let directory = tempfile::tempdir().expect("temporary analysis store");
    let pools = test_pools();
    let store = AppStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: directory.path().into(),
        })
        .build();
    let cancel = CancelToken::root();
    let mut owner = owner_in(&cancel, store.clone());
    let (host, queue) = queue_off().await;
    let (track_a, source_a) = track(&host, 1, &tone_mp3).await;
    let (track_b, source_b) = track(&host, 2, &rhythm_a_mp3).await;
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
    host.close().await;
}

struct HeldRun {
    target: AnalysisTarget,
    queue: AppQueueControl,
    source: AppTrackSource,
    host: OffThread<(AppHost, AppQueueControl)>,
    owner: Owner,
    rx: watch::Receiver<Option<TrackArtifacts>>,
    track_id: TrackId,
}

impl HeldRun {
    async fn close(self) {
        self.host.close().await;
    }
}

async fn close_run(cancel: &CancelToken, url: &str, value: Option<AnalysisProgress>) -> HeldRun {
    let mut owner = owner(cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, url).await;
    let target = target_of(&owner, &source);
    let rx = owner.subscribe(queue.clone(), track_id, source.clone(), axis());
    let tx = take_over_run(&mut owner, value);

    drop(tx);
    owner.drive().await;
    HeldRun {
        target,
        queue,
        source,
        host,
        owner,
        rx,
        track_id,
    }
}

#[kithara::test(native, tokio)]
async fn a_close_carrying_a_complete_value_publishes_and_caches_it(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut run = close_run(&cancel, &tone_mp3, Some(progress(analysis()))).await;

    assert_eq!(revision_held(&run.rx), Some(1));
    assert!(run.owner.cache.get(&run.target, axis()).is_some());
    assert_eq!(
        run.owner.entries[0].stage(),
        Stage::Ended(axis()),
        "the pass ran its course"
    );
    cancel.cancel();
    run.close().await;
}

#[kithara::test(native, tokio)]
async fn a_close_without_a_value_is_retried_on_the_next_subscribe(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut run = close_run(&cancel, &tone_mp3, None).await;

    assert_eq!(revision_held(&run.rx), None);
    assert!(
        run.owner.cache.get(&run.target, axis()).is_none(),
        "a run that closes with no value caches nothing"
    );
    assert_eq!(run.owner.entries[0].stage(), Stage::Failed(axis()));

    let _again = run
        .owner
        .subscribe(run.queue.clone(), run.track_id, run.source.clone(), axis());
    assert_eq!(
        running_track(&run.owner),
        Some(run.track_id),
        "the track is retried on the next subscribe"
    );
    cancel.cancel();
    run.close().await;
}

#[kithara::test(native, tokio, flash(false))]
async fn a_failed_pass_is_not_reopened_by_queue_warm(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut run = close_run(&cancel, &tone_mp3, None).await;
    run.owner.warm(&run.queue, &[run.track_id], axis());
    assert!(
        run.owner.active.is_none(),
        "an unrelated queue edit must not reopen the failed source"
    );
    assert!(run.owner.pending.is_empty());
    cancel.cancel();
    run.close().await;
}

#[kithara::test(native, tokio, flash(false))]
#[case::one_track(1)]
#[case::beyond_cache_capacity(128)]
async fn obsolete_unheld_entries_are_reclaimed(
    #[case] cycles: u64,
    tone_mp3: String,
    temp_dir: TestTempDir,
) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let original = url::Url::parse(&tone_mp3)
        .expect("fixture URL")
        .to_file_path()
        .expect("local fixture");
    for cycle in 0..cycles {
        let path = temp_dir.path().join(format!("track-{cycle}.mp3"));
        std::fs::copy(&original, &path).expect("distinct local track identity");
        let url: String = url::Url::from_file_path(path)
            .expect("absolute track path")
            .into();
        let (track_id, source) = track(&host, cycle + 1, &url).await;
        let rx = owner.subscribe(queue.clone(), track_id, source, axis());
        drop(take_over_run(&mut owner, None));
        owner.drive().await;
        drop(rx);
        host.call(move |(_, queue)| queue.remove(track_id).expect("remove old track"))
            .await;
        owner.warm(&queue, &[], axis());
        assert!(
            owner.entries.is_empty(),
            "removed tracks must release their resource configuration and artifacts at cycle {cycle}"
        );
    }
    cancel.cancel();
    host.close().await;
}

#[kithara::test(native, tokio, flash(false))]
async fn removed_background_work_is_not_requeued_or_started(
    rhythm_a_mp3: String,
    rhythm_b_mp3: String,
) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (first, _) = track(&host, 1, &rhythm_a_mp3).await;
    let (second, _) = track(&host, 2, &rhythm_b_mp3).await;
    owner.warm(&queue, &[first, second], axis());
    let tx = take_over_run(&mut owner, None);
    if let Some(Activity::Running(run)) = &mut owner.active {
        run.requeue = true;
    }
    host.call(move |(_, queue)| {
        queue.remove(first).expect("remove running track");
        queue.remove(second).expect("remove queued track");
    })
    .await;
    owner.warm(&queue, &[], axis());
    assert!(
        owner.pending.is_empty(),
        "removed queued work must not start"
    );
    assert!(!requeued(&owner), "removed running work must not resume");
    assert_eq!(owner.entries.len(), 1, "keep only the closing run");
    drop(tx);
    owner.drive().await;
    assert!(owner.active.is_none());
    assert!(
        owner.entries.is_empty(),
        "closed obsolete work must release its owner"
    );
    cancel.cancel();
    host.close().await;
}

#[kithara::test(native, tokio, flash(false))]
async fn a_completed_pass_releases_its_publication_after_the_last_subscriber_leaves(
    tone_mp3: String,
) {
    let cancel = CancelToken::root();
    let mut run = close_run(&cancel, &tone_mp3, Some(progress(analysis()))).await;
    run.owner.drive().await;
    assert!(run.owner.active.is_none(), "the checkpoint commit finished");
    assert!(run.owner.entries[0].value_for(axis()).is_some());
    let old = std::mem::replace(&mut run.rx, watch::channel(None).1);
    drop(old);
    time::timeout(Duration::from_millis(100), run.owner.drive())
        .await
        .expect("unsubscribe must wake the owner even after completion");
    assert!(
        run.owner.entries[0].value_for(axis()).is_none(),
        "only the bounded cache may retain the completed analysis"
    );
    cancel.cancel();
    run.close().await;
}

async fn resumable_progress(url: &str) -> AnalysisProgress {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, url).await;
    let target = target_of(&owner, &source);
    let _rx = owner.subscribe(queue, track_id, source, axis());
    loop {
        time::timeout(Duration::from_secs(2), owner.drive())
            .await
            .expect("the pass progresses");
        let held = owner.cache.get(&target, axis());
        if let Some(progress) = held.filter(AnalysisProgress::is_resumable) {
            cancel.cancel();
            host.close().await;
            return progress;
        }
        assert!(owner.active.is_some(), "the pass published no checkpoint");
    }
}

#[kithara::test(native, tokio, flash(false))]
async fn a_close_on_an_unsettled_value_is_resumed_on_the_next_subscribe(long_wav: String) {
    let url = long_wav;
    let checkpoint = resumable_progress(&url).await;
    let cancel = CancelToken::root();
    let mut run = close_run(&cancel, &url, Some(checkpoint.clone())).await;

    assert_eq!(
        revision_held(&run.rx),
        Some(checkpoint.analysis().revision())
    );
    assert!(run.owner.cache.get(&run.target, axis()).is_some());
    assert_eq!(run.owner.entries[0].stage(), Stage::Failed(axis()));

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
    run.close().await;
}

#[kithara::test(native, tokio, flash(false))]
async fn a_rejected_checkpoint_opens_a_fresh_pass(long_wav: String) {
    let url = long_wav;
    let checkpoint = resumable_progress(&url).await;
    let cancel = CancelToken::root();
    let mut config = app_config(&cancel, memory_store());
    config.analysis_chunk_seconds = NonZeroU32::new(7).expect("fixture chunk is non-zero");
    let (service, _handle) =
        AnalysisService::new(&config, persistence(&cancel, test_pools()), cancel.child());
    let mut owner = service.owner;
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, &url).await;
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
    assert!(
        held.analysis().expect("a pass published").is_complete(),
        "which finishes the track"
    );
    assert!(held.analysis().map(TrackAnalysis::revision) > Some(checkpoint.analysis().revision()));
    cancel.cancel();
    host.close().await;
}

#[kithara::test(native, tokio)]
async fn an_entry_is_queued_only_while_it_is_in_line(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, &tone_mp3).await;
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
    host.close().await;
}

#[kithara::test(native, tokio)]
async fn a_finished_background_entry_holds_its_value_only_in_the_cache(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, &tone_mp3).await;
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
    host.close().await;
}

#[kithara::test(native, tokio)]
async fn warm_seeds_nothing_before_the_run_opens(tone_mp3: String, rhythm_a_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, &tone_mp3).await;
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
        let (busy, busy_source) = track(&host, 2, &rhythm_a_mp3).await;
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
    host.close().await;
}

#[kithara::test(native, tokio)]
async fn a_background_warm_keeps_the_holder_of_a_held_entry(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host_a, queue_a) = queue_off_named("app-host-a").await;
    let (host_b, queue_b) = queue_off_named("app-host-b").await;
    let (track_a, source_a) = track(&host_a, 1, &tone_mp3).await;
    let (track_b, _) = track(&host_b, 2, &tone_mp3).await;
    let _rx = owner.subscribe(queue_a, track_a, source_a, axis());

    owner.warm(&queue_b, &[track_b], axis());

    assert_eq!(owner.entries.len(), 1, "one resource, one entry");
    assert_eq!(owner.entries[0].track_id(), track_a);
    cancel.cancel();
    host_a.close().await;
    host_b.close().await;
}

#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn settle(owner: &mut Owner) {
    while owner.active.is_some() {
        let progress = time::timeout(Duration::from_secs(2), owner.drive());
        ::kithara::platform::tokio::pin!(progress);
        loop {
            hang_tick!();
            ::kithara::platform::tokio::select! {
                result = &mut progress => {
                    result.expect("the pass progresses");
                    hang_reset!();
                    break;
                }
                () = time::sleep(Duration::from_millis(100)) => {}
            }
        }
    }
}

#[kithara::test(native, tokio, flash(false))]
async fn a_fresh_pass_publishes_above_the_seeded_revision(short_wav: String) {
    let url = short_wav;
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let fingerprint = owner.runner.fingerprint().clone();
    assert!(
        fingerprint.beat().is_some(),
        "fixture needs an artifact to omit"
    );
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, &url).await;
    let target = target_of(&owner, &source);
    let wanting = snapshot(token_for(target.key()), 3, 1_000, fingerprint, None);
    owner.cache.put(target, progress(wanting));

    let rx = owner.subscribe(queue, track_id, source, axis());
    assert_eq!(revision_held(&rx), Some(3));
    settle(&mut owner).await;

    let held = rx.borrow().clone().expect("the deck holds the final value");
    assert!(
        held.analysis().expect("a pass published").is_complete(),
        "the pass finished the track"
    );
    let revision = held.analysis().expect("a pass published").revision();
    assert!(
        revision > 3,
        "the final revision outranks the seeded one: {revision}"
    );
    cancel.cancel();
    host.close().await;
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
async fn an_invalid_layout_yields_no_analysis(tone_mp3: String) {
    let layouts = AssetLayoutRegistry::default().with::<File<AppPools>>(Arc::new(InvalidLayout));
    let store = AppStore::builder(test_pools())
        .backend(StorageBackend::Memory)
        .layouts(layouts)
        .build();
    let cancel = CancelToken::root();
    let mut owner = owner_in(&cancel, store);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, &tone_mp3).await;

    let mut rx = owner.subscribe(queue, track_id, source, axis());

    assert!(revision_held(&rx).is_none(), "the deck shows nothing");
    assert!(rx.changed().await.is_err(), "and nothing will come");
    assert!(owner.entries.is_empty());
    cancel.cancel();
    host.close().await;
}

#[kithara::test(native, tokio, flash(false))]
async fn a_track_shorter_than_its_header_claims_is_done(tone_mp3: String) {
    the_source_gave_everything_it_can(&tone_mp3).await;
}

#[kithara::test(native, tokio, flash(false))]
async fn a_resampled_track_is_covered_from_its_first_frame(rhythm_a_mp3: String) {
    the_source_gave_everything_it_can(&rhythm_a_mp3).await;
}

async fn the_source_gave_everything_it_can(url: &str) {
    const HEAD_TOLERANCE_FRAMES: u64 = 2 * MPEG_FRAME_SAMPLES;

    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, url).await;

    let rx = owner.subscribe(queue.clone(), track_id, source.clone(), axis());
    settle(&mut owner).await;

    let held = rx.borrow().clone().expect("the deck holds the final value");
    let analysis = held.analysis().expect("a pass published");
    assert_eq!(
        analysis.extent(),
        Some(analysis.coverage().frontier()),
        "the extent is where the source ended, whatever its header claimed"
    );
    assert!(
        analysis.is_settled(),
        "the pass took everything the source gives"
    );
    let missing = analysis.missing();
    let only_the_head = match missing.as_slice() {
        [] => true,
        [head] => head.start == 0 && head.frames() <= HEAD_TOLERANCE_FRAMES,
        _ => false,
    };
    assert!(
        only_the_head,
        "only the priming the decoder cannot deliver is missing: {missing:?}"
    );
    assert!(
        Prepared::default().settled_for(
            &owner
                .cache
                .get(&target_of(&owner, &source), axis())
                .expect("the pass cached its final value"),
            owner.runner.fingerprint(),
        ),
        "nothing is left for another pass"
    );
    let _again = owner.subscribe(queue, track_id, source, axis());
    assert!(
        owner.active.is_none(),
        "a track the source gave in full is not analysed again"
    );
    cancel.cancel();
    host.close().await;
}

const MPEG_FRAME_SAMPLES: u64 = 1152;
#[kithara::test(native, tokio)]
async fn a_track_opened_with_every_artifact_is_not_analysed(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track_prepared(
        &host,
        1,
        &tone_mp3,
        &owner.config.clone(),
        Some(served_grid()),
        Some(served_waveform()),
    )
    .await;

    let rx = owner.subscribe(queue, track_id, source, axis());

    assert!(
        owner.active.is_none() && owner.pending.is_empty(),
        "nothing the caller already handed over is analysed again"
    );
    let held = rx.borrow().clone().expect("the deck is served at once");
    assert!(held.grid().is_some(), "the served grid reaches the deck");
    assert!(held.waveform().is_some(), "and so does the served waveform");
    assert!(
        held.analysis().is_none(),
        "with no local pass invented behind them"
    );
    cancel.cancel();
    host.close().await;
}

#[kithara::test(native, tokio, flash(false))]
async fn a_served_waveform_leaves_only_the_beats_to_analyse(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    assert!(
        owner.runner.fingerprint().waveform().is_some(),
        "fixture runtime analyses waveforms at all"
    );
    let (host, queue) = queue_off().await;
    let (track_id, source) = track_prepared(
        &host,
        1,
        &tone_mp3,
        &owner.config.clone(),
        None,
        Some(served_waveform()),
    )
    .await;

    let rx = owner.subscribe(queue, track_id, source, axis());
    assert_eq!(
        running_track(&owner),
        Some(track_id),
        "the beats are still missing, so a pass opens"
    );
    settle(&mut owner).await;

    let held = rx.borrow().clone().expect("the deck holds the publication");
    assert!(
        held.waveform().is_some(),
        "the served waveform is published beside the pass"
    );
    let analysis = held.analysis().expect("the pass published");
    assert!(
        analysis.waveform().is_none(),
        "and the pass itself analysed no waveform"
    );
    assert!(analysis.beat().is_some(), "only the beats were analysed");
    cancel.cancel();
    host.close().await;
}

/// Two origins cover the two artifacts between them: the grid the caller
/// handed over and a waveform this track was analysed for once before. There
/// is nothing left for a pass to do, so none opens.
#[kithara::test(native, tokio)]
async fn a_supplied_grid_over_a_cached_waveform_opens_no_pass(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track_prepared(
        &host,
        1,
        &tone_mp3,
        &owner.config.clone(),
        Some(served_grid()),
        None,
    )
    .await;
    let cached = snapshot(
        "test-track".into(),
        6,
        1_000,
        owner.runner.fingerprint().clone(),
        None,
    );
    owner
        .cache
        .put(target_of(&owner, &source), progress(cached));

    let rx = owner.subscribe(queue, track_id, source, axis());

    assert!(
        owner.active.is_none() && owner.pending.is_empty(),
        "between the caller and the cache both artifacts are covered"
    );
    let held = rx.borrow().clone().expect("the deck is served at once");
    assert_eq!(
        held.grid().expect("a grid is published").as_raw().model_id,
        served_grid().as_raw().model_id,
        "the grid is the caller's"
    );
    assert!(
        held.waveform().is_some(),
        "and the waveform is the cached pass's"
    );
    cancel.cancel();
    host.close().await;
}

/// The same the other way round: the waveform was handed over and the beats
/// are in the cache from an earlier pass.
#[kithara::test(native, tokio)]
async fn a_supplied_waveform_over_cached_beats_opens_no_pass(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track_prepared(
        &host,
        1,
        &tone_mp3,
        &owner.config.clone(),
        None,
        Some(served_waveform()),
    )
    .await;
    let cached = beats_only(owner.runner.fingerprint().clone());
    owner
        .cache
        .put(target_of(&owner, &source), progress(cached));

    let rx = owner.subscribe(queue, track_id, source, axis());

    assert!(
        owner.active.is_none() && owner.pending.is_empty(),
        "the beats are cached and the waveform was handed over"
    );
    let held = rx.borrow().clone().expect("the deck is served at once");
    assert!(held.grid().is_some(), "the cached beats are published");
    assert_eq!(
        held.waveform().map(Waveform::buckets),
        Some(served_waveform().buckets()),
        "and the waveform is the one the caller handed over, bucket for bucket"
    );
    cancel.cancel();
    host.close().await;
}

/// A revision the deck already holds is not a new publication. The run may
/// state it again — a checkpoint, a resumed pass, a repeated send — and the
/// entry answers that nothing moved.
#[kithara::test(native, tokio)]
async fn a_repeated_revision_is_published_once(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track(&host, 1, &tone_mp3).await;
    let mut rx = owner.subscribe(queue, track_id, source, axis());
    let tx = take_over_run(&mut owner, None);

    tx.send(Some(progress(revision_of(3))))
        .expect("run publishes");
    owner.publish();
    assert_eq!(revision_held(&rx), Some(3));
    drop(rx.borrow_and_update());

    tx.send(Some(progress(revision_of(3))))
        .expect("run publishes the same revision again");
    owner.publish();

    assert!(
        !rx.has_changed().expect("the sender is alive"),
        "the same revision twice is one publication, not two"
    );
    cancel.cancel();
    host.close().await;
}

/// A cache hit is a pass result like any other, and a pass result never
/// outranks what the caller handed over. The cached grid was analysed for
/// this very track, and it is still the supplied one the deck reads.
#[kithara::test(native, tokio)]
async fn a_cached_pass_never_replaces_the_grid_the_caller_supplied(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track_prepared(
        &host,
        1,
        &tone_mp3,
        &owner.config.clone(),
        Some(served_grid()),
        Some(served_waveform()),
    )
    .await;
    let cached = snapshot(
        "test-track".into(),
        5,
        1_000,
        owner.runner.fingerprint().clone(),
        Some(grid()),
    );
    owner
        .cache
        .put(target_of(&owner, &source), progress(cached));

    let rx = owner.subscribe(queue, track_id, source, axis());

    let held = rx.borrow().clone().expect("the deck is served at once");
    assert_eq!(
        revision_held(&rx),
        Some(5),
        "the cached pass is published, as a pass result"
    );
    assert_eq!(
        held.grid().expect("a grid is published").as_raw().model_id,
        served_grid().as_raw().model_id,
        "but the grid the deck reads is the one the caller handed over"
    );
    assert!(
        owner.active.is_none() && owner.pending.is_empty(),
        "and nothing reopens to reconcile the two"
    );
    cancel.cancel();
    host.close().await;
}

/// The mirror of the served-waveform case: one publication carries a grid the
/// caller handed over and a waveform this build analysed, and neither origin
/// is visible to the consumer that reads them.
#[kithara::test(native, tokio, flash(false))]
async fn a_supplied_grid_is_published_beside_a_locally_analysed_waveform(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track_prepared(
        &host,
        1,
        &tone_mp3,
        &owner.config.clone(),
        Some(served_grid()),
        None,
    )
    .await;

    let rx = owner.subscribe(queue, track_id, source, axis());
    assert_eq!(
        running_track(&owner),
        Some(track_id),
        "the waveform is still missing, so a pass opens"
    );
    settle(&mut owner).await;

    let held = rx.borrow().clone().expect("the deck holds the publication");
    let grid = held.grid().expect("the supplied grid is published");
    assert_eq!(
        grid.as_raw()
            .beats
            .iter()
            .map(|beat| beat.ordinal)
            .collect::<Vec<_>>(),
        served_grid()
            .as_raw()
            .beats
            .iter()
            .map(|beat| beat.ordinal)
            .collect::<Vec<_>>(),
        "the pass that filled in the waveform renamed no beat of the grid it was handed"
    );
    assert!(
        held.waveform().is_some(),
        "and the waveform the pass produced is published beside it"
    );
    let analysis = held.analysis().expect("the pass published");
    assert!(
        analysis.beat().is_none(),
        "the pass analysed no beats: the track already had a grid"
    );
    assert!(
        analysis.waveform().is_some(),
        "only the waveform was analysed"
    );
    cancel.cancel();
    host.close().await;
}

/// A prepared artifact publishes on its own, before any pass has run. What is
/// settled belongs to the pass and to nothing else, so a publication carrying
/// only supplied artifacts states no settled result at all.
#[kithara::test(native, tokio, flash(false))]
async fn a_supplied_artifact_publishes_without_claiming_a_settled_pass(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track_prepared(
        &host,
        1,
        &tone_mp3,
        &owner.config.clone(),
        Some(served_grid()),
        None,
    )
    .await;

    let rx = owner.subscribe(queue, track_id, source, axis());

    let held = rx.borrow().clone().expect("the grid publishes at once");
    assert!(
        held.grid().is_some(),
        "the supplied grid is usable immediately"
    );
    assert!(
        held.analysis().is_none(),
        "no pass has finished, so the publication claims nothing a pass would claim"
    );
    assert!(
        held.waveform().is_none(),
        "and it invents no waveform to go with the grid it has"
    );
    cancel.cancel();
    host.close().await;
}

/// Wait until the entry holds the artifact its source answers with. The owner
/// is driven by hand here, the way every other test in this file drives it.
async fn read_artifacts(owner: &mut Owner, reads: usize) {
    for _ in 0..reads {
        time::timeout(Duration::from_secs(5), owner.drive())
            .await
            .expect("the artifact read answers");
    }
}

#[kithara::test(native, tokio, flash(false))]
async fn a_grid_read_from_a_source_reaches_the_deck_unanalysed(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let bytes = serde_json::to_vec(served_grid().as_raw()).expect("the grid serializes");
    let (track_id, source) = track_sourced(
        &host,
        1,
        &tone_mp3,
        &owner.config.clone(),
        Some(document("grid.json", &bytes).into()),
        Some(Arc::new(served_waveform()).into()),
    )
    .await;

    let rx = owner.subscribe(queue, track_id, source, axis());
    assert!(
        owner.active.is_none() && owner.pending.is_empty(),
        "an artifact still being read is not a reason to analyse one"
    );
    read_artifacts(&mut owner, 1).await;

    let held = rx.borrow().clone().expect("the deck is served");
    assert_eq!(
        held.grid().map(|grid| grid.as_raw().model_id.clone()),
        Some("served".to_owned()),
        "the grid the source served reaches the deck"
    );
    assert!(
        held.analysis().is_none(),
        "with no local pass invented behind it"
    );
    cancel.cancel();
    host.close().await;
}

/// A source the caller named is a request for that artifact. Bytes that are
/// not a grid are reported as such and leave the track without one — they
/// never turn into a local pass nobody asked for.
#[kithara::test(native, tokio, flash(false))]
async fn a_grid_source_that_does_not_parse_never_becomes_local_work(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let (track_id, source) = track_sourced(
        &host,
        1,
        &tone_mp3,
        &owner.config.clone(),
        Some(document("broken.json", b"{}").into()),
        Some(Arc::new(served_waveform()).into()),
    )
    .await;

    let rx = owner.subscribe(queue, track_id, source, axis());
    read_artifacts(&mut owner, 1).await;

    let held = rx.borrow().clone().expect("the deck is served");
    assert!(held.grid().is_none(), "no grid was served");
    assert!(
        held.waveform().is_some(),
        "and the waveform beside it is unharmed"
    );
    assert!(
        owner.active.is_none() && owner.pending.is_empty(),
        "a grid the caller asked a source for is not analysed instead"
    );
    cancel.cancel();
    host.close().await;
}

/// An artifact answering for a load that is over belongs to no track: the
/// entry has been re-pointed and must keep what it holds now.
#[kithara::test(native, tokio, flash(false))]
async fn an_artifact_answering_a_closed_load_is_dropped(tone_mp3: String) {
    let cancel = CancelToken::root();
    let mut owner = owner(&cancel);
    let (host, queue) = queue_off().await;
    let bytes = serde_json::to_vec(served_grid().as_raw()).expect("the grid serializes");
    let (first, source) = track_sourced(
        &host,
        1,
        &tone_mp3,
        &owner.config.clone(),
        Some(document("late.json", &bytes).into()),
        None,
    )
    .await;
    let rx = owner.subscribe(queue.clone(), first, source, axis());
    // The same resource re-pointed at another track: one entry, a new load.
    let (second, plain) = track(&host, 2, &tone_mp3).await;
    let _ = owner.subscribe(queue, second, plain, axis());

    read_artifacts(&mut owner, 1).await;

    assert!(
        rx.borrow()
            .as_ref()
            .is_none_or(|held| held.grid().is_none()),
        "the entry moved on, so the late grid is dropped"
    );
    cancel.cancel();
    host.close().await;
}
