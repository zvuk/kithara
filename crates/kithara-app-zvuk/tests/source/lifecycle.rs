use kithara_app_library::{Context, Environment, RegisterError, Secrets};
use kithara_app_zvuk::Source;
use kithara_net::{HttpClient, NetError, NetOptions};
use kithara_platform::{CancelToken, tokio::runtime::Handle};
use kithara_test_utils::{TestTempDir, bufpool::pools};
use kithara_ui::{error::UiDocError, ids::SourceUri, text::parse_text};
use serde_yaml_ng::Value as Section;

use super::{
    support::{memory, queue_completed, refusing},
    *,
};

/// Plugin services over `secrets`.
fn environment(cancel: &CancelToken, secrets: Secrets) -> Environment {
    let net = HttpClient::new(NetOptions::default(), pools(), cancel.clone());
    Environment::new(Handle::current(), net, secrets)
}

#[kithara::test(tokio)]
async fn a_mismatched_entry_names_the_plugin_and_not_its_value() {
    let cancel = kithara_test_utils::cancel_token();
    let environment = environment(&cancel, memory());
    let entry = Section::String("secret-token-hunter2".to_owned());

    let Err(cause) = (Source::FACTORY.register)(&environment, Context::new(cancel, entry)) else {
        panic!("a token in place of the entry must not register");
    };

    let message = RegisterError::new(Source::FACTORY.id, cause).to_string();
    assert!(message.contains("sources.zvuk"), "{message}");
    assert!(!message.contains("hunter2"), "{message}");
}

/// The page states a missing account in the words of the catalog it is
/// built with.
#[kithara::test(tokio)]
async fn a_signed_out_page_reads_its_status_from_the_catalog() {
    let cancel = kithara_test_utils::cancel_token();
    let environment = environment(&cancel, memory());
    let entry: Section = serde_yaml_ng::from_str("{user_agent: agent}").expect("the entry parses");
    let registration = (Source::FACTORY.register)(&environment, Context::new(cancel, entry))
        .expect("the entry registers");
    let shipped = registration.page().texts[0];
    let mut words = parse_text(shipped.text, &SourceUri(shipped.path.to_owned()))
        .expect("the source's captions parse");
    words.entries.insert(
        "zvuk.status.not_connected".to_owned(),
        "synthetic locale wording".to_owned(),
    );

    let source = registration
        .build(&words)
        .expect("the catalog words every label");

    assert_eq!(
        source.status(),
        PageStatus::Unreadable(Some("synthetic locale wording"))
    );
}

/// The account row words its fault in the catalog the source is built with;
/// a catalog without a fault's words fails the build naming the key.
#[kithara::test(tokio)]
async fn the_account_row_words_its_fault_from_the_catalog() {
    const STORE: &str = "zvuk.account.fault.store";
    let cancel = kithara_test_utils::cancel_token();
    let environment = environment(&cancel, refusing());
    let register = || {
        let entry: Section =
            serde_yaml_ng::from_str("{user_agent: agent}").expect("the entry parses");
        (Source::FACTORY.register)(&environment, Context::new(cancel.clone(), entry))
            .expect("the entry registers")
    };
    let registration = register();
    let shipped = registration.page().texts[0];
    let mut words = parse_text(shipped.text, &SourceUri(shipped.path.to_owned()))
        .expect("the source's captions parse");
    let mut missing = words.clone();
    missing.entries.remove(STORE);
    words
        .entries
        .insert(STORE.to_owned(), "synthetic store wording".to_owned());

    let Err(error) = register().build(&missing) else {
        panic!("a catalog without the store fault's words must not build");
    };
    assert!(
        matches!(&error, UiDocError::UnknownTextKey { key, .. } if key == STORE),
        "{error}"
    );
    let mut source = registration
        .build(&words)
        .expect("the catalog words every label");
    wait_until(
        Duration::from_secs(2),
        "the row shows the store fault",
        || {
            source.tick();
            source.read("account_fault") == Some(ReadValue::Text("synthetic store wording"))
        },
    )
    .await
    .expect("the row words the store fault from the catalog");
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
    let mut source = source(net.clone()).await;
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
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Err(NetError::Timeout)),
    )));
    let mut source = loaded_search(net.clone()).await;
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
    release(source, &net).await;
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
    let mut source = source(net.clone()).await;
    queue_completed(&mut *source, &net, "old", 4).await;
    source.select("playlist:empty");
    source.tick();
    assert_eq!(source.read("query"), Some(ReadValue::Text("")));
    assert!(source.rows(None).is_empty());
    until(&mut *source, PageStatus::Empty).await;
    release(source, &net).await;
}

/// A new query drops the request it supersedes, which ends without a result,
/// and shows its own; an emptied query drops the request in flight without
/// starting another.
#[kithara::test]
async fn a_superseding_query_cancels_the_outstanding_request() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes.next_call(matching!((_, body, _) if serde_json::from_slice::<serde_json::Value>(body).unwrap()["variables"]["query"] == "current"))
            .returns(Ok(search_reply())),
        NetMock::post_bytes.next_call(matching!(_, _, _)).returns(Ok(stream_reply())),
    )));
    let seen = |event: &str| {
        net.events()
            .iter()
            .filter(|logged| *logged == event)
            .count()
    };
    let mut source = source(net.clone()).await;

    source.write("query", &WriteValue::Text("blocked".into()));
    wait_until(Duration::from_secs(2), "the held search starts", || {
        source.tick();
        seen("start:blocked") == 1
    })
    .await
    .expect("the held search must start");
    source.write("query", &WriteValue::Text("current".into()));
    wait_until(Duration::from_secs(2), "the newer search shows", || {
        source.tick();
        source.status() == PageStatus::Ready && seen("end:blocked") == 1
    })
    .await
    .expect("the newer query must replace the held search");
    assert_eq!(source.rows(None).len(), 5);

    source.write("query", &WriteValue::Text("blocked".into()));
    wait_until(
        Duration::from_secs(2),
        "the held search starts again",
        || {
            source.tick();
            seen("start:blocked") == 2
        },
    )
    .await
    .expect("the held search must start again");
    source.write("query", &WriteValue::Text(String::new()));
    wait_until(Duration::from_secs(2), "the held search ends", || {
        source.tick();
        seen("end:blocked") == 2
    })
    .await
    .expect("an emptied query must drop the held search");
    assert!(source.rows(None).is_empty());
    assert_eq!(source.status(), PageStatus::Empty);
    release(source, &net).await;
    let mut events = net.events();
    events.sort();
    assert_eq!(
        events,
        [
            "end:blocked",
            "end:blocked",
            "end:current",
            "end:node",
            "start:blocked",
            "start:blocked",
            "start:current",
            "start:node",
        ]
    );
}

#[kithara::test]
async fn dropping_the_source_cancels_its_request_without_cancelling_the_parent() {
    let net = DelayedNet::new(Unimock::new(()));
    let parent = kithara_test_utils::cancel_token();
    let mut source = registered(net.clone(), &parent).await;
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
    let mut source = registered(net.clone(), &parent).await;
    queue_completed(&mut *source, &net, "needle", 4).await;
    parent.cancel();
    source.tick();
    assert!(source.rows(None).is_empty());
    release(source, &net).await;
}

#[kithara::test(tokio, flash(false))]
async fn the_registered_source_restores_its_native_streaming_preference() {
    let directory = TestTempDir::new();
    let path = directory.path().join("nested").join("zvuk.json");
    let cancel = kithara_test_utils::cancel_token();
    let environment = environment(&cancel, memory());
    let build = || {
        let entry: Section = serde_yaml_ng::from_str("{user_agent: agent}").unwrap();
        let context = Context::new(cancel.child(), entry).with_settings_path(Some(path.clone()));
        let registered = (Source::FACTORY.register)(&environment, context).unwrap();
        let text = registered.page().texts[0];
        let words = parse_text(text.text, &SourceUri(text.path.to_owned())).unwrap();
        registered.build(&words).unwrap()
    };
    let mut first = build();
    wait_until(Duration::from_secs(2), "initial preference is read", || {
        first.tick();
        first.read("streaming_pending") == Some(ReadValue::Bool(false))
    })
    .await
    .unwrap();
    assert_eq!(
        first.read("adaptive_streaming"),
        Some(ReadValue::Bool(true))
    );
    first.write("adaptive_streaming", &WriteValue::Trigger);
    wait_until(
        Duration::from_secs(2),
        "changed preference is saved",
        || {
            first.tick();
            first.read("streaming_pending") == Some(ReadValue::Bool(false))
        },
    )
    .await
    .unwrap();
    assert_eq!(
        first.read("adaptive_streaming"),
        Some(ReadValue::Bool(false))
    );
    assert_eq!(
        first.read("streaming_fault_hidden"),
        Some(ReadValue::Bool(true))
    );
    drop(first);
    let mut restarted = build();
    wait_until(
        Duration::from_secs(2),
        "restarted preference is read",
        || {
            restarted.tick();
            restarted.read("streaming_pending") == Some(ReadValue::Bool(false))
        },
    )
    .await
    .unwrap();
    assert_eq!(
        restarted.read("adaptive_streaming"),
        Some(ReadValue::Bool(false))
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "false");
}
