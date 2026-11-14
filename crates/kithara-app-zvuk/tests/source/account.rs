use kithara_app_library::LibrarySource;
use kithara_net::Headers;

use super::{
    support::{
        AUTH_HEADER, LABEL, Linked, PAGE, Row, Shown, TOKEN, blocking, faults, linked, memory,
        paths, poll, queue_completed, refusing, session, status,
    },
    *,
};

const NEWER: &str = "synthetic-newer-token";
const LATEST: &str = "synthetic-latest-token";
const NOT_CONNECTED: PageStatus = PageStatus::Unreadable(Some("No Zvuk account is connected"));

fn carries(headers: Option<&Headers>, token: &str) -> bool {
    headers.and_then(|headers| headers.get(AUTH_HEADER)) == Some(token)
}

fn playlists(source: &dyn LibrarySource) -> (bool, usize) {
    let node = &source.branch().children[2];
    (node.unlisted, node.children.len())
}

/// Ticks the source until the Playlists node is listed.
async fn listed(source: &mut dyn LibrarySource) {
    wait_until(Duration::from_secs(2), "the playlists are listed", || {
        source.tick();
        !playlists(source).0
    })
    .await
    .expect("the playlist listing must reach the branch");
}

fn signed_out(fault: Option<&str>) -> impl Fn(&Row) -> bool {
    move |row| row.state == Shown::SignedOut && row.fault.as_deref() == fault
}

/// After a refused request, Connect polls until Zvuk confirms the code.
#[kithara::test(timeout(Long))]
async fn signing_in_grants_the_token_and_reloads_the_catalogue() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!((_, _, headers) if carries(headers.as_ref(), TOKEN)))
            .returns(Ok(Bytes::from_static(include_bytes!(
                "../fixtures/playlists.json"
            )))),
        NetMock::post_bytes
            .next_call(matching!((_, _, headers) if carries(headers.as_ref(), TOKEN)))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!((_, _, headers) if carries(headers.as_ref(), TOKEN)))
            .returns(Ok(stream_reply())),
    )));
    net.sessions([Err(500), Ok(session(300))]);
    net.polls([poll(None), poll(Some(TOKEN))]);
    net.park(true);
    let mut linked = linked(net, None).await;
    linked.source.select("search");
    linked
        .source
        .write("query", &WriteValue::Text("needle".into()));
    linked.source.expand("playlists");
    time::sleep(Duration::from_millis(300)).await;
    linked.shows(signed_out(None)).await;
    assert_eq!(linked.source.status(), NOT_CONNECTED);

    linked.press("account_connect");
    linked.shows(signed_out(Some(faults::SESSION))).await;
    linked.press("account_connect");
    linked.shows(|row| row.state == Shown::Waiting).await;
    linked
        .shows(|row| row.state == Shown::Connected(Some(LABEL.to_owned())))
        .await;
    listed(&mut *linked.source).await;
    linked.net.park(false);
    until(&mut *linked.source, PageStatus::Ready).await;

    assert_eq!(linked.row().fault, None);
    assert_eq!(linked.token().as_deref(), Some(TOKEN));
    assert_eq!(linked.stored().as_deref(), Some(TOKEN));
    assert_eq!(linked.opened(), [PAGE]);
    assert_eq!(linked.source.read("query"), Some(ReadValue::Text("needle")));
    assert_eq!(linked.source.rows(None).len(), 5);
    assert_eq!(playlists(&*linked.source), (false, 2));
    let net = linked.release().await;
    let requests = net.requests();
    assert_eq!(
        net.paths(),
        [
            paths::SESSION,
            paths::SESSION,
            paths::TOKEN,
            paths::TOKEN,
            paths::PROFILE
        ]
    );
    assert!(requests[0].device_id.is_some());
    assert_ne!(requests[0].device_id, requests[1].device_id);
    assert!(
        requests[3].at - requests[2].at >= Duration::from_secs(2),
        "polls are at least two seconds apart: {requests:?}"
    );
    assert_eq!(requests[4].token.as_deref(), Some(TOKEN));
    assert_eq!(
        net.events(),
        [
            "start:node",
            "end:node",
            "start:needle",
            "end:needle",
            "start:node",
            "end:node"
        ]
    );
}

/// Expiry, Cancel and a failing browser each end the sign-in.
#[kithara::test(timeout(::std::time::Duration::from_millis(14198)))]
async fn a_sign_in_without_a_confirmed_code_keeps_no_token() {
    let expiring = DelayedNet::new(Unimock::new(()));
    expiring.sessions([Ok(session(3))]);
    let cancelled = DelayedNet::new(Unimock::new(()));
    cancelled.polls([poll(None), poll(Some(TOKEN))]);
    let mut expiring = Linked::new(expiring, memory(), false);
    let mut cancelled = Linked::new(cancelled, memory(), false);
    let mut failing = Linked::new(DelayedNet::new(Unimock::new(())), memory(), true);
    for linked in [&mut expiring, &mut cancelled, &mut failing] {
        linked.shows(signed_out(None)).await;
        linked.press("account_connect");
    }

    cancelled.shows(|row| row.state == Shown::Waiting).await;
    cancelled.net.reaches(2).await;
    cancelled.press("account_cancel");
    expiring.shows(|row| row.state == Shown::Waiting).await;
    for (linked, fault) in [
        (&mut cancelled, None),
        (&mut expiring, None),
        (&mut failing, Some(faults::BROWSER)),
    ] {
        linked.shows(signed_out(fault)).await;
    }
    time::sleep(Duration::from_secs(3)).await;

    for (linked, sent) in [
        (cancelled, &[paths::SESSION, paths::TOKEN][..]),
        (expiring, &[paths::SESSION, paths::TOKEN][..]),
        (failing, &[paths::SESSION][..]),
    ] {
        assert_eq!(linked.token(), None);
        assert_eq!(linked.stored(), None);
        assert_eq!(linked.opened(), [PAGE]);
        assert_eq!(linked.release().await.paths(), sent);
    }
}

#[kithara::test(timeout(Medium))]
async fn signing_out_completes_before_zvuk_answers_and_clears_the_catalogue() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(liked_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
        NetMock::post_bytes
            .next_call(matching!((_, _, headers) if carries(headers.as_ref(), NEWER)))
            .returns(Ok(Bytes::from_static(include_bytes!(
                "../fixtures/playlists.json"
            )))),
    )));
    net.hold_logout();
    let mut linked = linked(net, Some(TOKEN)).await;
    linked.source.select("liked");
    until(&mut *linked.source, PageStatus::Ready).await;
    linked.net.hold(true);
    linked.source.expand("playlists");
    linked.net.reaches_events(5).await;

    linked.sign_out().await;
    assert!(matches!(linked.row().state, Shown::Connected(_)));
    assert_eq!(
        linked.source.status(),
        PageStatus::Ready,
        "the page follows the account on the tick the row does"
    );
    linked.shows(signed_out(None)).await;
    linked.net.reaches_events(6).await;
    linked.source.select("liked");
    linked.source.tick();

    assert_eq!(linked.source.status(), NOT_CONNECTED);
    assert!(linked.source.rows(None).is_empty());
    assert_eq!(playlists(&*linked.source), (true, 0));
    assert_eq!(linked.stored(), None);

    linked.net.hold(false);
    linked.net.park(true);
    linked.sign_in(NEWER).await;
    linked.source.tick();
    linked.net.reaches_events(8).await;
    linked.sign_out().await;
    linked.source.tick();

    assert_eq!(playlists(&*linked.source), (true, 0));
    let net = linked.release().await;
    let logouts: Vec<_> = net
        .requests()
        .into_iter()
        .filter(|request| request.path == paths::LOGOUT)
        .map(|request| request.token)
        .collect();
    assert_eq!(net.paths()[..2], [paths::PROFILE, paths::LOGOUT]);
    assert_eq!(logouts, [Some(TOKEN.to_owned()), Some(NEWER.to_owned())]);
    assert_eq!(
        net.events(),
        [
            "start:node",
            "end:node",
            "start:node",
            "end:node",
            "start:node",
            "end:node",
            "start:node",
            "end:node"
        ]
    );
}

/// A search started under the stored token is refused only once a newer
/// token is in place.
#[kithara::test(timeout(Long))]
async fn a_refused_token_signs_out_and_a_stale_refusal_spares_the_newer_one() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!((_, _, headers) if carries(headers.as_ref(), TOKEN)))
            .returns(Err(status(401))),
        NetMock::post_bytes
            .next_call(matching!((_, _, headers) if carries(headers.as_ref(), NEWER)))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!((_, _, headers) if carries(headers.as_ref(), NEWER)))
            .returns(Ok(stream_reply())),
        NetMock::post_bytes
            .next_call(matching!((_, _, headers) if carries(headers.as_ref(), NEWER)))
            .returns(Err(status(401))),
        NetMock::post_bytes
            .next_call(matching!((_, _, headers) if carries(headers.as_ref(), LATEST)))
            .returns(Ok(Bytes::from_static(include_bytes!(
                "../fixtures/playlists.json"
            )))),
        NetMock::post_bytes
            .next_call(matching!((_, _, headers) if carries(headers.as_ref(), LATEST)))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!((_, _, headers) if carries(headers.as_ref(), LATEST)))
            .returns(Ok(stream_reply())),
    )));
    let mut linked = linked(net, Some(TOKEN)).await;
    linked.net.park(true);
    linked
        .source
        .write("query", &WriteValue::Text("needle".into()));
    time::sleep(Duration::from_millis(300)).await;
    linked.source.tick();
    linked.sign_out().await;
    linked.sign_in(NEWER).await;
    linked.net.park(false);
    linked.net.reaches_events(2).await;
    until(&mut *linked.source, PageStatus::Ready).await;

    assert_eq!(linked.token().as_deref(), Some(NEWER));
    assert_eq!(linked.row().fault, None);
    assert_eq!(linked.source.rows(None).len(), 5);

    linked
        .source
        .write("query", &WriteValue::Text("blocked".into()));
    time::sleep(Duration::from_millis(300)).await;
    linked.source.tick();
    linked.net.reaches_events(7).await;
    linked.source.expand("playlists");
    linked.shows(signed_out(Some(faults::REJECTED))).await;
    wait_until(Duration::from_secs(2), "the search in flight ends", || {
        linked.source.tick();
        linked.net.events().len() == 10
    })
    .await
    .expect("the refused listing must stop the search in flight");
    assert_eq!(playlists(&*linked.source), (true, 0));
    assert_eq!(linked.token(), None);
    wait_until(
        Duration::from_secs(2),
        "the store forgets the token",
        || linked.stored().is_none(),
    )
    .await
    .expect("the account must delete the refused token from the store");
    assert_eq!(linked.source.status(), NOT_CONNECTED);

    linked
        .source
        .write("query", &WriteValue::Text("needle".into()));
    linked.net.park(true);
    linked.sign_in(LATEST).await;
    listed(&mut *linked.source).await;
    linked.net.park(false);
    until(&mut *linked.source, PageStatus::Ready).await;

    assert_eq!(linked.source.rows(None).len(), 5);
    assert!(!linked.cancel.is_cancelled());
    let net = linked.release().await;
    assert_eq!(
        net.events(),
        [
            "start:needle",
            "end:needle",
            "start:needle",
            "end:needle",
            "start:node",
            "end:node",
            "start:blocked",
            "start:node",
            "end:node",
            "end:blocked",
            "start:node",
            "end:node",
            "start:needle",
            "end:needle",
            "start:node",
            "end:node"
        ]
    );
}

/// The refusal arrives through the stream batch of a search a newer query
/// superseded, and ends the reaction and the listing in flight.
#[kithara::test(timeout(Medium))]
async fn a_refused_token_signs_out_and_ends_the_work_in_flight() {
    let net = DelayedNet::new(Unimock::new((
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(stream_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Ok(search_reply())),
        NetMock::post_bytes
            .next_call(matching!(_, _, _))
            .returns(Err(status(401))),
    )));
    let mut linked = linked(net, Some(TOKEN)).await;
    linked
        .shows(|row| row.state == Shown::Connected(Some(LABEL.to_owned())))
        .await;
    linked
        .source
        .write("query", &WriteValue::Text("needle".into()));
    time::sleep(Duration::from_millis(300)).await;
    until(&mut *linked.source, PageStatus::Ready).await;
    linked.net.hold(true);
    linked
        .source
        .write("like_track", &WriteValue::Text("1000".into()));
    linked.source.expand("playlists");
    linked.net.reaches_events(6).await;
    linked.net.hold(false);
    queue_completed(&mut *linked.source, &linked.net, "old", 10).await;

    linked
        .source
        .write("query", &WriteValue::Text("current".into()));
    linked.shows(signed_out(Some(faults::REJECTED))).await;
    time::sleep(Duration::from_millis(300)).await;
    linked.source.tick();
    linked.net.reaches_events(12).await;
    linked.source.select("liked");
    linked.source.expand("playlists");
    linked.source.tick();

    assert_eq!(linked.token(), None);
    assert_eq!(linked.source.status(), NOT_CONNECTED);
    assert_eq!(playlists(&*linked.source), (true, 0));
    assert_eq!(linked.source.branch().key, "zvuk");
    let net = linked.release().await;
    assert_eq!(net.paths(), [paths::PROFILE]);
    let events = net.events();
    assert_eq!(events.len(), 12, "{events:?}");
    assert_eq!(
        events[..10],
        [
            "start:needle",
            "end:needle",
            "start:node",
            "end:node",
            "start:node",
            "start:node",
            "start:old",
            "end:old",
            "start:node",
            "end:node"
        ]
    );
}

#[kithara::test(timeout(Medium))]
async fn a_failing_store_signs_out_with_its_error_and_revokes_the_new_session() {
    let net = DelayedNet::new(Unimock::new(()));
    net.polls([poll(Some(TOKEN))]);
    let mut linked = Linked::new(net, refusing(), false);
    linked.shows(signed_out(Some(faults::STORE))).await;
    assert!(linked.net.paths().is_empty());

    linked.press("account_connect");
    linked.shows(|row| row.state == Shown::Waiting).await;
    linked.shows(signed_out(Some(faults::STORE))).await;
    linked.net.reaches(3).await;

    assert_eq!(linked.token(), None);
    let net = linked.release().await;
    assert_eq!(net.paths(), [paths::SESSION, paths::TOKEN, paths::LOGOUT]);
    assert_eq!(net.requests()[2].token.as_deref(), Some(TOKEN));
}

#[kithara::test(flash(false))]
async fn cancelling_the_plugin_ends_the_account_during_a_blocked_store_read() {
    let (secrets, gate) = blocking(None);
    let linked = Linked::new(DelayedNet::new(Unimock::new(())), secrets, false);
    gate.reached().await;

    linked.release().await;

    drop(gate.release);
}

/// Zvuk is asked to revoke the session while the store still deletes the
/// token, so cancelling the plugin then cannot skip the revoke.
#[kithara::test(flash(false))]
async fn signing_out_revokes_the_session_before_the_store_forgets_the_token() {
    let (secrets, gate) = blocking(Some(TOKEN));
    let mut linked = Linked::new(DelayedNet::new(Unimock::new(())), secrets, false);
    gate.reached().await;
    gate.release.send(()).expect("the store read waits");
    linked
        .shows(|row| matches!(row.state, Shown::Connected(_)))
        .await;

    linked.press("account_disconnect");
    gate.reached().await;
    wait_until(
        kithara_test_utils::TestBudget::Quick.timeout(false),
        "the logout is sent",
        || linked.net.paths().contains(&paths::LOGOUT.to_owned()),
    )
    .await
    .expect("the revoke must not wait for the store");

    let net = linked.release().await;
    drop(gate.release);
    let logouts: Vec<_> = net
        .requests()
        .into_iter()
        .filter(|request| request.path == paths::LOGOUT)
        .map(|request| request.token)
        .collect();
    assert_eq!(logouts, [Some(TOKEN.to_owned())]);
}

/// The stored token signs the account in and reaches the `zvuk.com` key
/// requests without a sign-in.
#[kithara::test]
async fn a_stored_token_starts_the_account_and_is_granted_to_zvuk_com_key_requests() {
    let mut linked = linked(DelayedNet::new(Unimock::new(())), Some(TOKEN)).await;
    linked
        .shows(|row| row.state == Shown::Connected(Some(LABEL.to_owned())))
        .await;

    assert_eq!(
        (
            linked.grant.domain(),
            linked.grant.header(),
            linked.token().as_deref()
        ),
        ("zvuk.com", AUTH_HEADER, Some(TOKEN))
    );
    assert!(linked.opened().is_empty());
    let net = linked.release().await;
    assert_eq!(net.paths(), [paths::PROFILE]);
    assert_eq!(net.requests()[0].token.as_deref(), Some(TOKEN));
}
