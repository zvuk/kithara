use std::rc::Rc;

use kithara_platform::{
    CancelToken,
    sync::{Arc, ThreadGate, WaitGate},
    time::Duration,
    tokio::sync::oneshot,
};
use kithara_test_utils::{cancel_token, kithara};
use kithara_worker::{Worker, WorkerConfig};

use super::*;
use crate::LoadRefusal;

#[kithara::test(tokio)]
async fn blocked_preparation_keeps_the_requesting_executor_live() {
    let worker = Worker::new(WorkerConfig::new());
    let preparation = new(&worker, 0, None);
    let release = Arc::new(ThreadGate::default());
    let worker_release = Arc::clone(&release);
    let (started, received) = oneshot::channel();
    let result = execute(&preparation, None, move || async move {
        let state = Rc::new(7);
        let since = worker_release.current();
        let _ = started.send(());
        {
            let _io = kithara_platform::flash::real_io();
            assert!(
                worker_release.wait_timeout(since, Duration::from_secs(1)),
                "the requesting executor must release cold preparation"
            );
        }
        kithara_platform::tokio::task::yield_now().await;
        Ok(*state)
    });
    let (result, ()) = kithara_platform::tokio::join!(result, async {
        received.await.expect("preparation starts");
        release.signal();
    });
    assert_eq!(result.expect("preparation completes"), 7);
}

#[kithara::test(tokio)]
async fn cancelled_preparation_releases_its_waiting_request(cancel_token: CancelToken) {
    let worker = Worker::new(WorkerConfig::new());
    let preparation = new(&worker, 0, None);
    let (started, received) = oneshot::channel();
    let result = execute(
        &preparation,
        Some(cancel_token.clone()),
        move || async move {
            let _ = started.send(());
            std::future::pending::<Result<(), LoadRefusal>>().await
        },
    );
    let (result, ()) = kithara_platform::tokio::join!(result, async {
        received.await.expect("preparation starts");
        cancel_token.cancel();
    });
    assert!(matches!(result, Err(LoadRefusal::Cancelled)));
}

#[kithara::test(tokio)]
async fn pending_and_dropped_preparation_preserve_other_requests() {
    let worker = Worker::new(WorkerConfig::new());
    let preparation = new(&worker, 0, None);
    let (started, received) = oneshot::channel();
    let (retained, dropped) = oneshot::channel::<()>();
    {
        let mut request = Box::pin(execute(&preparation, None, move || async move {
            let _ = started.send(());
            let result = std::future::pending::<Result<(), LoadRefusal>>().await;
            drop(retained);
            result
        }));
        kithara_platform::tokio::select! {
            _ = &mut request => panic!("the first preparation remains pending"),
            result = received => result.expect("the first preparation starts"),
        }
        assert_eq!(
            execute(&preparation, None, || async { Ok(1) })
                .await
                .expect("another request keeps its own admission"),
            1
        );
    }
    assert!(
        dropped.await.is_err(),
        "dropping the request releases its pending source"
    );
    assert_eq!(
        execute(&preparation, None, || async { Ok(2) })
            .await
            .expect("the owner remains live"),
        2
    );
}

#[kithara::test(tokio)]
async fn a_panicked_preparation_does_not_stop_the_next_request() {
    let worker = Worker::new(WorkerConfig::new());
    let preparation = new(&worker, 0, None);
    let failed = execute(&preparation, None, || {
        std::future::poll_fn::<Result<(), LoadRefusal>, _>(|_| panic!("cold preparation panic"))
    })
    .await;
    assert!(matches!(
        failed,
        Err(LoadRefusal::Source(kithara_audio::TrackFailureKind::Render))
    ));
    assert_eq!(
        execute(&preparation, None, || async { Ok(3) })
            .await
            .expect("owner remains live"),
        3
    );
}
