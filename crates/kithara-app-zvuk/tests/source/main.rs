#![cfg(not(target_arch = "wasm32"))]

use bytes::Bytes;
use kithara_app_library::PageStatus;
use kithara_net::mock::NetMock;
use kithara_platform::time::{self, Duration};
use kithara_test_utils::{kithara, wait_until};
use kithara_ui::render::{ReadValue, WriteValue};
use unimock::{MockFn, Unimock, matching};

mod account;
mod counts;
mod lifecycle;
mod reaction_commands;
mod reactions;
mod support;

use support::{
    DelayedNet, liked_reply, loaded_search, playlist_reply, reactions, registered, release,
    search_reply, source, stream_reply, until,
};

#[kithara::test]
async fn a_query_is_debounced_and_resolves_one_batch() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes.next_call(matching!((_, body, _) if serde_json::from_slice::<serde_json::Value>(body).unwrap()["variables"]["query"] == "needle")).returns(Ok(search_reply())),
        NetMock::post_bytes.next_call(matching!((_, body, _) if String::from_utf8_lossy(body).contains("mediaContents"))).returns(Ok(stream_reply())),
    )));
    let mut source = source(net.clone()).await;
    source.select("search");
    source.write("query", &WriteValue::Text("old".to_owned()));
    source.tick();
    time::sleep(Duration::from_millis(100)).await;
    source.write("query", &WriteValue::Text("needle".to_owned()));
    assert_eq!(source.read("query"), Some(ReadValue::Text("needle")));
    source.tick();
    time::sleep(Duration::from_millis(299)).await;
    source.tick();
    assert!(source.rows(None).is_empty());
    time::sleep(Duration::from_millis(1)).await;
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(source.rows(None).len(), 5);
    assert!(source.rows(None).iter().all(|row| row.drag().is_some()));
    assert_eq!(
        net.events(),
        ["start:needle", "end:needle", "start:node", "end:node"]
    );
    release(source, &net).await;
}

#[kithara::test(timeout(Short))]
async fn a_trailing_space_keeps_search_rows_without_another_request() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
    )));
    let mut source = loaded_search(net.clone()).await;
    source.write("query", &WriteValue::Text("needle ".to_owned()));
    assert_eq!(source.read("query"), Some(ReadValue::Text("needle ")));
    assert_eq!(source.rows(None).len(), 5);
    time::sleep(Duration::from_millis(300)).await;
    source.tick();
    assert_eq!(source.status(), PageStatus::Ready);
    assert_eq!(source.read("count"), Some(ReadValue::Text("5 / 120")));
    source.write("query", &WriteValue::Text("new query".into()));
    assert_eq!(source.read("count"), Some(ReadValue::Text("0 / \u{221e}")));
    release(source, &net).await;
    assert_eq!(
        net.events(),
        ["start:needle", "end:needle", "start:node", "end:node"]
    );
}

#[kithara::test]
async fn playlist_navigation_is_loaded_once_by_the_source() {
    let net = DelayedNet::new(Unimock::new(
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(Bytes::from_static(include_bytes!(
                "../fixtures/playlists.json"
            )))),
    ));
    let mut source = source(net.clone()).await;
    assert!(source.branch().children[2].unlisted);
    source.expand("playlists");
    wait_until(Duration::from_secs(2), "the playlists are listed", || {
        source.tick();
        !source.branch().children[2].unlisted
    })
    .await
    .expect("the playlist listing must reach the branch");
    let playlists = &source.branch().children[2];
    assert_eq!(playlists.children.len(), 2);
    assert!(
        playlists
            .children
            .iter()
            .all(|node| node.key.starts_with("playlist:"))
    );
    source.expand("playlists");
    time::sleep(Duration::from_millis(20)).await;
    source.tick();
    assert_eq!(source.branch().children[2].children.len(), 2);
    assert_eq!(net.events(), ["start:node", "end:node"]);
    release(source, &net).await;
}
