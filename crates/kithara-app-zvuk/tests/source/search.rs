use kithara_app_library::LibrarySource;
use kithara_net::NetError;
use serde_json::{Value, json};

use super::*;

fn track(id: &str) -> Value {
    json!({
        "id": id,
        "title": format!("Track {id}"),
        "duration": 120,
        "artists": [{"title": "Artist"}],
        "release": null,
        "collectionItemData": null,
    })
}

fn track_page(ids: &[&str], cursor: Option<&str>) -> Bytes {
    let items = ids.iter().map(|id| track(id)).collect::<Vec<_>>();
    Bytes::from(
        json!({"data":{"search":{"tracks":{
            "items": items,
            "page": {"total": 3, "cursor": cursor},
        }}}})
        .to_string(),
    )
}

fn entity_page(field: &str, items: &Value) -> Bytes {
    let mut search = serde_json::Map::new();
    search.insert(
        field.to_owned(),
        json!({"items": items, "page": {"total": 2, "cursor": null}}),
    );
    Bytes::from(json!({"data": {"search": search}}).to_string())
}

fn entity_tracks(field: &str, id: &str, tracks_field: &str, ids: &[&str]) -> Bytes {
    let tracks = ids.iter().map(|id| track(id)).collect::<Vec<_>>();
    let mut entity = json!({"id": id});
    entity[tracks_field] = json!(tracks);
    let mut data = serde_json::Map::new();
    data.insert(field.to_owned(), json!([entity]));
    Bytes::from(json!({"data": data}).to_string())
}

fn streams(ids: &[&str]) -> Bytes {
    let media = ids
        .iter()
        .map(|id| {
            json!({
                "id": id,
                "streamV3": {
                    "hls": format!("https://media.example.invalid/{id}/master.m3u8"),
                    "expire": null,
                },
                "stream": null,
            })
        })
        .collect::<Vec<_>>();
    Bytes::from(json!({"data": {"mediaContents": media}}).to_string())
}

fn keys(source: &dyn LibrarySource) -> Vec<String> {
    (0..source.rows(None).len())
        .map(|row| {
            source
                .row_key(row)
                .expect("a visible row has a key")
                .to_owned()
        })
        .collect()
}

#[kithara::test]
async fn catalogue_rows_are_visible_before_their_streams_resolve() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
    )));
    let mut source = source(net.clone()).await;
    net.hold_streams(true);
    source.write("query", &WriteValue::Text("needle".into()));
    time::sleep(Duration::from_millis(300)).await;
    source.tick();

    wait_until(
        Duration::from_secs(2),
        "metadata arrives before streams",
        || {
            source.tick();
            source.rows(None).len() == 5
        },
    )
    .await
    .expect("catalogue metadata must not wait for stream resolution");
    wait_until(Duration::from_secs(2), "the stream request is held", || {
        net.stream_waiting()
    })
    .await
    .expect("the stream must be genuinely in flight");
    assert_eq!(source.status(), PageStatus::Loading);
    assert!(
        source
            .rows(None)
            .iter()
            .all(|row| row.muted() && row.drag().is_none())
    );
    assert_eq!(source.analysis_key(0), None);

    net.hold_streams(false);
    until(&mut *source, PageStatus::Ready).await;
    assert!(source.rows(None).iter().all(|row| row.drag().is_some()));
    release(source, &net).await;
}

#[kithara::test]
async fn scopes_open_entities_and_back_restores_the_retained_search() {
    let artists = entity_page(
        "artists",
        &json!([{"id":"20","title":"Ana Vale"},{"id":"21","title":"Ben Ore"}]),
    );
    let albums = entity_page(
        "releases",
        &json!([{"id":"30","title":"First Light","artists":[{"title":"Ana Vale"}]}]),
    );
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if String::from_utf8_lossy(body).contains("tracks(limit: $limit")))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if String::from_utf8_lossy(body).contains("KitharaStreams")))
            .returns(Ok(stream_reply())),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if String::from_utf8_lossy(body).contains("artists(limit: $limit")))
            .returns(Ok(artists)),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if String::from_utf8_lossy(body).contains("KitharaArtistTracks")))
            .returns(Ok(entity_tracks("getArtists", "20", "popularTracks", &["1000"]))),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(streams(&["1000"]))),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if String::from_utf8_lossy(body).contains("releases(limit: $limit")))
            .returns(Ok(albums)),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if String::from_utf8_lossy(body).contains("KitharaReleaseTracks")))
            .returns(Ok(entity_tracks("getReleases", "30", "tracks", &["1003", "1000"]))),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(streams(&["1003", "1000"]))),
    )));
    let mut source = loaded_search(net.clone()).await;

    source.write("scope_artists", &WriteValue::Trigger);
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(source.read("query"), Some(ReadValue::Text("needle")));
    assert_eq!(source.read("scope_artists"), Some(ReadValue::Bool(true)));
    assert_eq!(keys(&*source), ["artist:20", "artist:21"]);
    assert!(source.rows(None).iter().all(|row| row.drag().is_none()));

    source.write("open_entity", &WriteValue::Text("artist:20".into()));
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(keys(&*source), ["1000"]);
    assert_eq!(source.read("query"), Some(ReadValue::Text("needle")));
    assert_eq!(source.read("back_hidden"), Some(ReadValue::Bool(false)));
    let events = net.events().len();
    source.write("back", &WriteValue::Trigger);
    assert_eq!(keys(&*source), ["artist:20", "artist:21"]);
    assert_eq!(source.read("scope_artists"), Some(ReadValue::Bool(true)));
    assert_eq!(net.events().len(), events);

    source.write("scope_albums", &WriteValue::Trigger);
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(keys(&*source), ["album:30"]);
    source.write("open_entity", &WriteValue::Text("album:30".into()));
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(keys(&*source), ["1003", "1000"]);
    source.write("back", &WriteValue::Trigger);
    assert_eq!(keys(&*source), ["album:30"]);
    assert_eq!(source.read("scope_albums"), Some(ReadValue::Bool(true)));
    release(source, &net).await;
}

#[kithara::test]
async fn continuation_appends_distinct_rows_and_retries_the_same_cursor() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if {
                let body: Value = serde_json::from_slice(body).unwrap();
                body["query"].as_str().unwrap().contains("KitharaSearch")
                    && body["variables"]["cursor"].is_null()
            }))
            .returns(Ok(track_page(&["1000"], Some("one")))),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(streams(&["1000"]))),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if serde_json::from_slice::<Value>(body).unwrap()["variables"]["cursor"] == "one"))
            .returns(Ok(track_page(&["1000", "1003"], Some("two")))),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if {
                let body = String::from_utf8_lossy(body);
                body.contains("KitharaStreams") && body.contains("1003") && !body.contains("1000")
            }))
            .returns(Ok(streams(&["1003"]))),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if serde_json::from_slice::<Value>(body).unwrap()["variables"]["cursor"] == "two"))
            .returns(Err(NetError::Timeout)),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if serde_json::from_slice::<Value>(body).unwrap()["variables"]["cursor"] == "two"))
            .returns(Ok(track_page(&["1006"], Some("two")))),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(streams(&["1006"]))),
    )));
    let mut source = source(net.clone()).await;
    source.write("query", &WriteValue::Text("needle".into()));
    time::sleep(Duration::from_millis(300)).await;
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(
        source.read("load_more_hidden"),
        Some(ReadValue::Bool(false))
    );

    source.write("load_more", &WriteValue::Trigger);
    source.write("load_more", &WriteValue::Trigger);
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(keys(&*source), ["1000", "1003"]);
    assert_eq!(source.read("count"), Some(ReadValue::Text("2 / 3")));

    source.write("load_more", &WriteValue::Trigger);
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(keys(&*source), ["1000", "1003"]);
    assert_eq!(
        source.read("load_more_hidden"),
        Some(ReadValue::Bool(false))
    );
    assert!(matches!(source.read("fault"), Some(ReadValue::Text(text)) if !text.is_empty()));

    source.write("load_more", &WriteValue::Trigger);
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(keys(&*source), ["1000", "1003", "1006"]);
    assert_eq!(source.read("load_more_hidden"), Some(ReadValue::Bool(true)));
    assert_eq!(source.read("fault"), Some(ReadValue::Text("")));
    release(source, &net).await;
}

/// Re-selecting the current search node after a failed continuation resumes
/// that continuation instead of restarting the search, so the accepted pages
/// stay.
#[kithara::test]
async fn reselecting_search_after_a_failed_continuation_keeps_accumulated_pages() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if {
                let body: Value = serde_json::from_slice(body).unwrap();
                body["query"].as_str().unwrap().contains("KitharaSearch")
                    && body["variables"]["cursor"].is_null()
            }))
            .returns(Ok(track_page(&["1000"], Some("one")))),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(streams(&["1000"]))),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if serde_json::from_slice::<Value>(body).unwrap()["variables"]["cursor"] == "one"))
            .returns(Ok(track_page(&["1000", "1003"], Some("two")))),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if {
                let body = String::from_utf8_lossy(body);
                body.contains("KitharaStreams") && body.contains("1003") && !body.contains("1000")
            }))
            .returns(Ok(streams(&["1003"]))),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if serde_json::from_slice::<Value>(body).unwrap()["variables"]["cursor"] == "two"))
            .returns(Err(NetError::Timeout)),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if serde_json::from_slice::<Value>(body).unwrap()["variables"]["cursor"] == "two"))
            .returns(Ok(track_page(&["1006"], Some("two")))),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(streams(&["1006"]))),
    )));
    let mut source = source(net.clone()).await;
    source.write("query", &WriteValue::Text("needle".into()));
    time::sleep(Duration::from_millis(300)).await;
    until(&mut *source, PageStatus::Ready).await;

    source.write("load_more", &WriteValue::Trigger);
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(keys(&*source), ["1000", "1003"]);

    source.write("load_more", &WriteValue::Trigger);
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(keys(&*source), ["1000", "1003"]);
    assert!(matches!(source.read("fault"), Some(ReadValue::Text(text)) if !text.is_empty()));

    source.select("search");
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(keys(&*source), ["1000", "1003", "1006"]);
    assert_eq!(source.read("load_more_hidden"), Some(ReadValue::Bool(true)));
    assert_eq!(source.read("fault"), Some(ReadValue::Text("")));
    release(source, &net).await;
}

#[kithara::test]
async fn continuation_without_a_new_identity_ends_pagination() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(track_page(&["1000"], Some("one")))),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(streams(&["1000"]))),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if serde_json::from_slice::<Value>(body).unwrap()["variables"]["cursor"] == "one"))
            .returns(Ok(track_page(&["1000"], Some("two")))),
    )));
    let mut source = source(net.clone()).await;
    source.write("query", &WriteValue::Text("needle".into()));
    time::sleep(Duration::from_millis(300)).await;
    until(&mut *source, PageStatus::Ready).await;
    source.write("load_more", &WriteValue::Trigger);
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(keys(&*source), ["1000"]);
    assert_eq!(source.read("load_more_hidden"), Some(ReadValue::Bool(true)));
    release(source, &net).await;
}

#[kithara::test]
async fn suggestions_are_capped_and_choosing_one_searches_its_text() {
    let empty = Bytes::from(
        json!({"data":{"search":{"tracks":{"items":[],"page":{"total":0,"cursor":null}}}}})
            .to_string(),
    );
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if serde_json::from_slice::<Value>(body).unwrap()["variables"]["query"] == "mo"))
            .returns(Ok(empty)),
        NetMock::post_bytes
            .next_call(matching!((_, body, _) if serde_json::from_slice::<Value>(body).unwrap()["variables"]["query"] == "Mozart"))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
    )));
    net.autocomplete([
        Ok(Bytes::from_static(
            br#"{"data":{"searchAutocomplete":{"content":["Mozart","Most","Mosaic","Fourth"]}}}"#,
        )),
        Ok(Bytes::from_static(
            br#"{"data":{"searchAutocomplete":{"content":["Mozart"]}}}"#,
        )),
    ]);
    let mut source = source(net.clone()).await;
    source.write("query", &WriteValue::Text("mo".into()));
    time::sleep(Duration::from_millis(300)).await;
    wait_until(Duration::from_secs(2), "three suggestions arrive", || {
        source.tick();
        source.read("suggestion_2") == Some(ReadValue::Text("Mosaic"))
            && source.status() == PageStatus::Empty
    })
    .await
    .expect("the first three service suggestions must be visible");
    assert_eq!(source.read("suggestion_0"), Some(ReadValue::Text("Mozart")));
    assert_eq!(source.read("suggestion_1"), Some(ReadValue::Text("Most")));

    source.write("suggestion_0", &WriteValue::Trigger);
    assert_eq!(source.read("query"), Some(ReadValue::Text("Mozart")));
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(source.rows(None).len(), 5);
    assert_eq!(
        source.read("suggestion_1_hidden"),
        Some(ReadValue::Bool(true))
    );
    release(source, &net).await;
}

#[kithara::test]
async fn suggestion_failure_keeps_catalogue_rows_and_stale_replies_are_dropped() {
    let empty = Bytes::from(
        json!({"data":{"search":{"tracks":{"items":[],"page":{"total":0,"cursor":null}}}}})
            .to_string(),
    );
    let net = DelayedNet::new(Unimock::new(
        NetMock::post_bytes
            .each_call(
                matching!((_, body, _) if String::from_utf8_lossy(body).contains("KitharaSearch")),
            )
            .returns(Ok(empty)),
    ));
    net.autocomplete([
        Ok(Bytes::from_static(
            br#"{"data":{"searchAutocomplete":{"content":["Old"]}}}"#,
        )),
        Ok(Bytes::from_static(
            br#"{"data":{"searchAutocomplete":{"content":["Fresh"]}}}"#,
        )),
    ]);
    let mut source = source(net.clone()).await;
    source.write("query", &WriteValue::Text("old".into()));
    time::sleep(Duration::from_millis(300)).await;
    source.tick();
    net.reaches_events(4).await;
    source.write("query", &WriteValue::Text("new".into()));
    assert_eq!(
        source.read("suggestion_0_hidden"),
        Some(ReadValue::Bool(true))
    );
    wait_until(
        Duration::from_secs(2),
        "only the current suggestions arrive",
        || {
            source.tick();
            source.read("suggestion_0") == Some(ReadValue::Text("Fresh"))
                && source.status() == PageStatus::Empty
        },
    )
    .await
    .expect("the old query's suggestions must not replace the new query's");
    release(source, &net).await;

    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
    )));
    net.autocomplete([Err(500)]);
    let mut source = loaded_search(net.clone()).await;
    wait_until(
        Duration::from_secs(2),
        "the suggestion fault is visible",
        || {
            source.tick();
            matches!(source.read("fault"), Some(ReadValue::Text(text)) if text.contains("500"))
        },
    )
    .await
    .expect("a suggestion failure must not discard catalogue rows");
    assert_eq!(source.rows(None).len(), 5);
    assert!(source.rows(None).iter().all(|row| row.drag().is_some()));
    release(source, &net).await;
}

#[kithara::test]
async fn a_query_change_cancels_obsolete_stream_enrichment() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .each_call(matching!((_, body, _) if serde_json::from_slice::<Value>(body).unwrap()["variables"]["query"] == "old"))
            .returns(Ok(track_page(&["1000"], None))),
        NetMock::post_bytes
            .each_call(matching!((_, body, _) if serde_json::from_slice::<Value>(body).unwrap()["variables"]["query"] == "current"))
            .returns(Ok(track_page(&["2000"], None))),
        NetMock::post_bytes
            .each_call(matching!((_, body, _) if String::from_utf8_lossy(body).contains("KitharaStreams")))
            .returns(Ok(streams(&["2000"]))),
    )));
    let mut source = source(net.clone()).await;
    net.hold_streams(true);
    source.write("query", &WriteValue::Text("old".into()));
    time::sleep(Duration::from_millis(300)).await;
    wait_until(Duration::from_secs(2), "old metadata is visible", || {
        source.tick();
        keys(&*source) == ["1000"] && net.stream_waiting()
    })
    .await
    .expect("the old catalogue must reach stream enrichment");

    source.write("query", &WriteValue::Text("current".into()));
    net.hold_streams(false);
    until(&mut *source, PageStatus::Ready).await;
    assert_eq!(keys(&*source), ["2000"]);
    assert_eq!(
        source.rows(None)[0].drag().unwrap()["source"],
        "https://media.example.invalid/2000/master.m3u8"
    );
    release(source, &net).await;
}
