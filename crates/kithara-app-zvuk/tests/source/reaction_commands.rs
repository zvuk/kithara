use super::*;

#[kithara::test]
async fn row_reactions_are_in_flight_per_track_and_deduplicate_pending_commands() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if
            serde_json::from_slice::<serde_json::Value>(body)
                .expect("the reaction must encode a GraphQL request")["query"]
                .as_str().expect("the encoded query is text")
                .contains(r#"addItem(id: "1000""#)))
            .returns(Ok(Bytes::from_static(include_bytes!(
                "../fixtures/like.json"
            )))),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if
            serde_json::from_slice::<serde_json::Value>(body)
                .expect("the reaction must encode a GraphQL request")["query"]
                .as_str().expect("the encoded query is text")
                .contains(r#"addItem(id: "1003""#)))
            .returns(Ok(Bytes::from_static(include_bytes!(
                "../fixtures/like.json"
            )))),
    )));
    let mut source = loaded_search(net.clone()).await;
    source.write("like_track", &WriteValue::Text("unknown".into()));
    source.write("like_track", &WriteValue::Text("1000".into()));
    source.write("like_track", &WriteValue::Text("1000".into()));
    source.write("like_track", &WriteValue::Text("1003".into()));
    assert_eq!(reactions(&*source, "1000"), [false]);
    assert_eq!(reactions(&*source, "1003"), [false]);
    wait_until(
        Duration::from_secs(2),
        "both row reactions are confirmed",
        || {
            source.tick();
            reactions(&*source, "1000") == [true] && reactions(&*source, "1003") == [true]
        },
    )
    .await
    .expect("a reaction pending on one track must not drop another track's reaction");
    assert_eq!(reactions(&*source, "1006"), [false]);
    release(source, &net).await;
    assert_eq!(
        net.events(),
        [
            "start:needle",
            "end:needle",
            "start:node",
            "end:node",
            "start:node",
            "end:node",
            "start:node",
            "end:node"
        ]
    );
}

/// A reaction confirmed while another page loads drops that load's snapshot,
/// which may predate the reaction, and loads the page again.
#[kithara::test]
async fn a_reaction_confirmed_during_a_page_load_reloads_the_page() {
    let mut reloaded: serde_json::Value =
        serde_json::from_slice(&playlist_reply()).expect("the playlist reply parses");
    reloaded["data"]["playlists"][0]["tracks"][0]["collectionItemData"]["itemStatus"] =
        "liked".into();
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(Bytes::from_static(include_bytes!(
                "../fixtures/like.json"
            )))),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(playlist_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(Bytes::from(
                serde_json::to_vec(&reloaded).expect("the reloaded playlist encodes"),
            ))),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
    )));
    let mut source = loaded_search(net.clone()).await;
    net.hold_reactions(true);
    source.write("like_track", &WriteValue::Text("1000".into()));
    wait_until(
        Duration::from_secs(2),
        "the reaction receives its reply",
        || net.reaction_waiting(),
    )
    .await
    .expect("the ordered reaction reply must arrive before the playlist request");
    source.select("playlist:1023");
    source.tick();
    wait_until(
        Duration::from_secs(2),
        "the playlist request starts",
        || net.events().len() >= 6,
    )
    .await
    .expect("the playlist must be running before the reaction completes");
    net.hold_reactions(false);
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(reactions(&*source, "1000"), [true]);
    release(source, &net).await;
    assert_eq!(net.events().len(), 14);
}
