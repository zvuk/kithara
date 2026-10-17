use kithara::{
    host::wasm,
    platform::{
        sync::mpsc,
        thread::{assert_not_main_thread, keep_worker_alive},
        tokio::{self, sync::broadcast::error::RecvError, task::spawn as task_spawn},
    },
    play::{CrossfadeSettings, PlayError},
    queue::{QueueEvent, QueueSettings},
};

use crate::{
    pools::{FfiPools, FfiQueue, Pools},
    web::analysis::{AnalysisRequest, AnalysisRuns},
};

pub(super) fn queue_settings() -> QueueSettings {
    QueueSettings::builder()
        .crossfade(CrossfadeSettings {
            duration: 5.0,
            ..Default::default()
        })
        .build()
}

/// Registers the unique Queue and hosts source analysis off the browser main thread.
/// Playback commands go directly to the Queue's own postbox.
pub(super) fn worker_main(
    queue: FfiQueue,
    host_sender: wasm::HostSender<FfiPools>,
    pools: Pools,
    analysis_rx: mpsc::Receiver<AnalysisRequest>,
) {
    assert_not_main_thread(concat!(module_path!(), "::worker_main"));
    keep_worker_alive();
    task_spawn(async move {
        let control = queue.control();
        let mut membership = control.subscribe::<QueueEvent>();
        crate::web::observer::source::spawn(&control);
        let mut host = wasm::remote_host(host_sender);
        let owner = match host.insert(queue) {
            Ok(owner) => owner,
            Err(error) => {
                tracing::error!(%error, "host rejected the Web queue");
                return;
            }
        };
        let mut analysis = AnalysisRuns::new(pools);
        loop {
            tokio::select! {
                request = analysis_rx.recv_async() => {
                    let Ok(request) = request else { break };
                    let AnalysisRequest { id, request_id, resources } = request;
                    if let Err(error) = analysis.start_queued(
                        &control, id, request_id, |url| resources.config(url).ok(),
                    ) {
                        crate::web::interop::send_reply(request_id, Err(error));
                    }
                }
                event = membership.recv() => {
                    if matches!(event, Err(RecvError::Closed)) { break; }
                }
            }
            analysis.reconcile(&control);
        }
        analysis.clear();
        match host.remove(&owner) {
            Ok(()) | Err(PlayError::SessionGone { .. }) => {}
            Err(error) => tracing::error!(%error, "host refused Web queue removal"),
        }
    });
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use crate::{
        pools,
        web::resources::{ResourceBuilder, consts},
    };

    #[kithara::test(wasm, flash(false))]
    fn shared_store_keeps_two_hls_working_sets() {
        let state = ResourceBuilder::new(pools::build().expect("resource pools"));
        assert_eq!(
            state.store.ephemeral_cache_capacity(),
            Some(consts::ASSET_CACHE_CAPACITY)
        );
        assert_eq!(consts::ASSET_CACHE_MAX_BYTES, 128 * 1024 * 1024);
    }
}
