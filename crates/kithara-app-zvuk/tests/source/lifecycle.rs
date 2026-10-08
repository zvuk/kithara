use kithara_app_library::{Context, Environment, RegisterError};
use kithara_app_zvuk::Source;
use kithara_net::{HttpClient, NetError, NetOptions};
use kithara_platform::tokio::runtime::Handle;
use kithara_test_utils::bufpool::pools;
use serde_yaml_ng::Value as Section;

use super::{support::queue_completed, *};

#[kithara::test(tokio)]
async fn a_mismatched_entry_names_the_plugin_and_not_its_value() {
    let cancel = kithara_test_utils::cancel_token();
    let net = HttpClient::new(NetOptions::default(), pools(), cancel.clone());
    let environment = Environment::new(Handle::current(), net);
    let entry = Section::String("secret-token-hunter2".to_owned());

    let Err(cause) = (Source::FACTORY.register)(&environment, Context::new(cancel, entry)) else {
        panic!("a token in place of the entry must not register");
    };

    let message = RegisterError::new(Source::FACTORY.id, cause).to_string();
    assert!(message.contains("sources.zvuk"), "{message}");
    assert!(!message.contains("hunter2"), "{message}");
}

/// Selecting the node already shown neither restarts nor drops its request,
/// and keeps its rows, query and selected row.
#[kithara::test]
async fn reselecting_the_current_node_keeps_its_request_rows_and_query() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(liked_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
    )));
    let mut source = source(net.clone());
    source.select("liked");
    source.select("liked");
    source.tick();
    wait_until(
        Duration::from_secs(2),
        "the collection result awaits a tick",
        || net.events().len() == 4,
    )
    .await
    .expect("the original collection request must complete before repeated selection");
    assert_eq!(source.status(), PageStatus::Loading);
    source.select("liked");
    source.select("liked");
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(source.rows(None).len(), 5);
    source.write("query", &WriteValue::Text("amber".into()));
    source.select("liked");
    source.tick();
    assert_eq!(source.read("query"), Some(ReadValue::Text("amber")));
    assert_eq!(source.row_key(0), Some("1000"));
    assert!(source.rows(Some("1000"))[0].selected());
    release(source, &net).await;
    assert_eq!(
        net.events(),
        ["start:node", "end:node", "start:node", "end:node"]
    );
}

/// A failed reaction is no page fault, so reselecting the page keeps its rows
/// and the reaction's fault instead of retrying the catalogue.
#[kithara::test]
async fn reselecting_after_a_side_operation_fault_keeps_the_page_without_a_retry() {
    let net = Unimock::new((
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Err(NetError::Timeout)),
    ));
    let mut source = loaded_search(net).await;
    source.write("like_track", &WriteValue::Text("1000".into()));
    wait_until(Duration::from_secs(2), "the reaction fails", || {
        source.tick();
        matches!(source.read("fault"), Some(ReadValue::Text(text)) if !text.is_empty())
    })
    .await
    .expect("the reaction must fail before repeated selection");
    let Some(ReadValue::Text(fault)) = source.read("fault") else {
        panic!("a failed operation must expose its status text");
    };
    let fault = fault.to_owned();
    source.select("search");
    source.tick();
    assert_eq!(source.status(), PageStatus::Ready);
    assert_eq!(source.read("fault"), Some(ReadValue::Text(&fault)));
    assert_eq!(source.rows(Some("1000")).len(), 5);
    assert!(source.rows(None).iter().all(|row| row.drag().is_some()));
    assert_eq!(reactions(&*source, "1000"), [false]);
}

#[kithara::test]
async fn a_side_operation_fault_keeps_the_page_fault_and_its_retry() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Err(NetError::Timeout)),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if String::from_utf8_lossy(body).contains("KitharaPlaylists")))
            .returns(Ok(Bytes::from_static(include_bytes!(
                "../fixtures/graphql_error.json"
            )))),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
    )));
    let mut source = loaded_search(net.clone()).await;
    assert!(
        source
            .rows(None)
            .iter()
            .all(|row| row.muted() && row.drag().is_none())
    );
    assert!(source.analysis_key(0).is_none());
    let Some(ReadValue::Text(stream_fault)) = source.read("fault") else {
        panic!("the stream failure must expose its text");
    };
    let stream_fault = stream_fault.to_owned();
    assert!(!stream_fault.is_empty());
    source.expand("playlists");
    wait_until(
        Duration::from_secs(2),
        "playlist failure is reported",
        || {
            source.tick();
            source.read("fault") != Some(ReadValue::Text(&stream_fault))
        },
    )
    .await
    .expect("the playlist listing must fail after the stream failure");
    let Some(ReadValue::Text(fault)) = source.read("fault") else {
        panic!("both failures must expose their text");
    };
    assert!(
        fault.contains(&stream_fault) && fault.contains("Synthetic service error"),
        "the page fault stays beside the playlist fault: {fault}"
    );
    source.select("search");
    assert_eq!(source.status(), PageStatus::Loading);
    assert_eq!(
        source.read("fault"),
        Some(ReadValue::Text("Synthetic service error"))
    );
    wait_until(Duration::from_secs(2), "stream retry is admitted", || {
        source.tick();
        source.status() == PageStatus::Ready
            && source
                .rows(None)
                .iter()
                .all(|row| !row.muted() && row.drag().is_some())
    })
    .await
    .expect("reselecting the page must retry its stream failure");
    release(source, &net).await;
}

#[kithara::test]
async fn leaving_search_discards_its_queued_result_and_clears_the_query() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes.next_call(matching!(_, _, _)).returns(Ok(search_reply())),
        NetMock::post_bytes.next_call(matching!(_, _, _)).returns(Ok(stream_reply())),
        NetMock::post_bytes.next_call(matching!((_, body, _) if String::from_utf8_lossy(body).contains("KitharaPlaylist"))).returns(Ok(Bytes::from_static(br#"{"data":{"playlists":[{"id":"empty","tracks":[]}]}}"#))),
    )));
    let mut source = source(net.clone());
    queue_completed(&mut *source, &net, "old", 4).await;
    source.select("playlist:empty");
    source.tick();
    assert_eq!(source.read("query"), Some(ReadValue::Text("")));
    assert!(source.rows(None).is_empty());
    until(&mut *source, PageStatus::Empty).await;
    release(source, &net).await;
}

/// A new query drops the outstanding request before starting its own, and an
/// emptied query drops it without starting another.
#[kithara::test]
async fn a_superseding_query_cancels_the_outstanding_request() {
    let net = DelayedNet::new(Unimock::new(
        NetMock::post_bytes.next_call(matching!((_, body, _) if serde_json::from_slice::<serde_json::Value>(body).unwrap()["variables"]["query"] == "current"))
            .returns(Ok(Bytes::from_static(br#"{"data":{"search":{"tracks":{"items":[]}}}}"#))),
    ));
    let mut source = source(net.clone());
    for (query, events) in [
        ("blocked", &["start:blocked"][..]),
        (
            "current",
            &[
                "start:blocked",
                "end:blocked",
                "start:current",
                "end:current",
            ],
        ),
        (
            "blocked",
            &[
                "start:blocked",
                "end:blocked",
                "start:current",
                "end:current",
                "start:blocked",
            ],
        ),
        (
            "",
            &[
                "start:blocked",
                "end:blocked",
                "start:current",
                "end:current",
                "start:blocked",
                "end:blocked",
            ],
        ),
    ] {
        source.write("query", &WriteValue::Text(query.into()));
        time::sleep(Duration::from_millis(300)).await;
        wait_until(Duration::from_secs(2), "the requests settle", || {
            source.tick();
            net.events() == events
        })
        .await
        .expect("each query must replace the outstanding request");
    }
    assert!(source.rows(None).is_empty());
    assert_eq!(source.status(), PageStatus::Empty);
    release(source, &net).await;
    assert_eq!(net.events().len(), 6);
}

#[kithara::test]
async fn dropping_the_source_cancels_its_request_without_cancelling_the_parent() {
    let net = DelayedNet::new(Unimock::new(()));
    let parent = kithara_test_utils::cancel_token();
    let mut source = registered(net.clone(), &parent);
    source.write("query", &WriteValue::Text("blocked".into()));
    time::sleep(Duration::from_millis(300)).await;
    wait_until(Duration::from_secs(2), "blocked search starts", || {
        source.tick();
        net.events() == ["start:blocked"]
    })
    .await
    .expect("the blocked search must start before cancellation");
    release(source, &net).await;
    assert_eq!(net.events(), ["start:blocked", "end:blocked"]);
    assert!(!parent.is_cancelled());
}

#[kithara::test]
async fn shutdown_discards_a_completion_already_waiting_for_tick() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
    )));
    let parent = kithara_test_utils::cancel_token();
    let mut source = registered(net.clone(), &parent);
    queue_completed(&mut *source, &net, "needle", 4).await;
    parent.cancel();
    source.tick();
    assert!(source.rows(None).is_empty());
    release(source, &net).await;
}

/// A token rejection latches the source whichever request meets it, even a
/// catalogue request a newer query superseded or a playlist listing beside a
/// search still in flight: the page turns unreadable, every request stops and
/// nothing starts another, while the parent and the branch remain.
#[kithara::test]
async fn an_authentication_rejection_latches_the_source() {
    for superseded in [true, false] {
        let net = DelayedNet::new(Unimock::new(
            NetMock::post_bytes
                .next_call(matching!(_, _, _))
                .returns(Err(NetError::Status {
                    status: std::num::NonZeroU16::new(401).unwrap(),
                    url: None,
                    body: None,
                })),
        ));
        let parent = kithara_test_utils::cancel_token();
        let mut source = registered(net.clone(), &parent);
        let expected: &[&str] = if superseded {
            queue_completed(&mut *source, &net, "old", 2).await;
            source.write("query", &WriteValue::Text("current".into()));
            &["start:old", "end:old"]
        } else {
            source.write("query", &WriteValue::Text("blocked".into()));
            time::sleep(Duration::from_millis(300)).await;
            wait_until(Duration::from_secs(2), "blocked search starts", || {
                source.tick();
                net.events() == ["start:blocked"]
            })
            .await
            .expect("the blocked search must be in flight before the rejection");
            source.expand("playlists");
            &["start:blocked", "start:node", "end:node", "end:blocked"]
        };
        until(&mut *source, PageStatus::Unreadable).await;
        source.select("liked");
        source.expand("playlists");
        source.write("query", &WriteValue::Text("another".into()));
        time::sleep(Duration::from_millis(300)).await;
        source.tick();
        assert_eq!(source.status(), PageStatus::Unreadable);
        assert!(
            matches!(source.read("fault"), Some(ReadValue::Text(text)) if text.contains("authentication"))
        );
        assert_eq!(source.branch().key, "zvuk");
        release(source, &net).await;
        assert_eq!(net.events(), expected, "superseded: {superseded}");
        assert!(!parent.is_cancelled());
    }
}
