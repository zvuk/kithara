use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Method, Request, Response},
};
use bytes::Bytes;
use futures::{StreamExt, future::poll_fn, stream};
use kithara::net::{HttpBody, HttpTransport, NetError};
use tower_service::Service;
use url::{Origin, Url};

pub(super) struct Transport {
    router: Router,
    origin: Origin,
}

impl Transport {
    pub(super) const fn new(router: Router, origin: Origin) -> Self {
        Self { router, origin }
    }
}

#[async_trait]
impl HttpTransport for Transport {
    async fn open(&self, request: Request<Bytes>) -> Result<Response<HttpBody>, NetError> {
        let url = Url::parse(&request.uri().to_string())
            .map_err(|error| NetError::Refused(error.to_string()))?;
        if url.origin() != self.origin {
            return Err(NetError::Refused(format!("fixture does not own {url}")));
        }
        let method = request.method().clone();
        let mut router = self.router.clone();
        poll_fn(|cx| Service::<Request<Body>>::poll_ready(&mut router, cx))
            .await
            .expect("Axum router is infallible");
        let response = router
            .call(request.map(Body::from))
            .await
            .expect("Axum router is infallible");
        Ok(frame_body(response, &method))
    }
}

fn frame_body(response: Response<Body>, method: &Method) -> Response<HttpBody> {
    let (parts, body) = response.into_parts();
    let expected = (method != Method::HEAD)
        .then(|| parts.headers.get("content-length"))
        .flatten()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    let body = stream::try_unfold(
        (body.into_data_stream(), 0_usize),
        move |(mut body, received)| async move {
            match body.next().await {
                Some(chunk) => {
                    let chunk = chunk.map_err(|error| NetError::Network(error.to_string()))?;
                    let received = received.saturating_add(chunk.len());
                    Ok(Some((chunk, (body, received))))
                }
                None if expected.is_some_and(|expected| received != expected) => {
                    Err(NetError::Network(
                        "fixture HTTP body ended before its declared length".to_owned(),
                    ))
                }
                None => Ok(None),
            }
        },
    );
    Response::from_parts(parts, Box::pin(body))
}

#[cfg(test)]
mod tests {
    use futures::TryStreamExt;
    #[cfg(feature = "flash")]
    use kithara::platform::time::{Instant, WallInstant};
    #[cfg(all(feature = "all", feature = "flash"))]
    use kithara::{
        assets::{AssetStore, StorageBackend},
        audio::AudioConfig,
        download::{Downloader, DownloaderConfig},
        hls::{Hls, HlsConfig},
        play::{PlayWorker, PlayWorkerConfig, TrackConfig},
    };
    use kithara::{
        net::{Headers, HttpClient, NetOptions, RangeSpec, RetryPolicy},
        platform::{CancelToken, sync::Arc, time::Duration},
    };
    use kithara_test_utils::{bufpool::pools, cancel_token};

    use super::*;
    #[cfg(feature = "flash")]
    use crate::fixture_protocol::DelayRule;
    use crate::{
        fixture_protocol::EncryptionRequest,
        kithara,
        test_server::{
            Content, Delivery, FixtureBehavior, HlsFixtureBuilder, PrivateTestServer,
            TestServerHelper,
        },
        test_server_state::TestServerState,
    };

    fn options(inactivity: Duration) -> NetOptions {
        NetOptions::builder()
            .inactivity_timeout(inactivity)
            .retry_policy(RetryPolicy::builder().max_retries(0).build())
            .build()
    }

    fn assert_metadata(real: &Headers, modeled: &Headers) {
        for name in [
            "content-length",
            "content-type",
            "content-range",
            "accept-ranges",
            "content-encoding",
        ] {
            assert_eq!(real.get(name), modeled.get(name), "{name}");
        }
    }

    #[kithara::test(tokio)]
    async fn real_and_modeled_routes_preserve_media_keys_ranges_and_errors() {
        let server = PrivateTestServer::start().await;
        let helper = server.helper();
        let options = options(Duration::from_secs(3));
        let modeled = helper.modeled_client(options.clone(), CancelToken::never());
        let real = HttpClient::new(options, pools(), CancelToken::never());
        let fixture = helper
            .create_hls(
                HlsFixtureBuilder::new()
                    .variant_count(2)
                    .segments_per_variant(2)
                    .segment_size(2048)
                    .encryption(EncryptionRequest {
                        iv_hex: None,
                        key_hex: "00112233445566778899aabbccddeeff".to_owned(),
                    }),
            )
            .await
            .unwrap();
        for url in [
            fixture.master_url(),
            fixture.media_url(0),
            fixture.segment_url(1, 1),
            fixture.key_url(),
        ] {
            let expected = real.get_bytes(url.clone(), None).await.unwrap();
            let actual = modeled.get_bytes(url.clone(), None).await.unwrap();
            assert!(!expected.is_empty());
            assert_eq!(expected, actual, "{url}");
            assert_metadata(
                &real.head(url.clone(), None).await.unwrap(),
                &modeled.head(url, None).await.unwrap(),
            );
        }
        let range = RangeSpec::new(3, Some(63));
        let real_range = real
            .get_range(fixture.segment_url(0, 0), range.clone(), None)
            .await
            .unwrap();
        let modeled_range = modeled
            .get_range(fixture.segment_url(0, 0), range, None)
            .await
            .unwrap();
        assert!(real_range.is_partial() && modeled_range.is_partial());
        assert_metadata(&real_range.headers, &modeled_range.headers);
        assert_eq!(
            real_range.try_collect::<Vec<_>>().await.unwrap().concat(),
            modeled_range
                .try_collect::<Vec<_>>()
                .await
                .unwrap()
                .concat(),
        );
        for client in [&real, &modeled] {
            assert!(
                matches!(client.get_bytes(helper.base_url().join("/missing").unwrap(), None).await,
                Err(NetError::Status { status, body: Some(body), .. }) if status.get() == 404 && body.is_empty())
            );
            assert!(
                matches!(client.post_bytes(helper.base_url().join("/health").unwrap(), Bytes::from_static(b"request"), None).await,
                Err(NetError::Status { status, .. }) if status.get() == 405)
            );
        }
        let truncated = helper.register_behavior(FixtureBehavior {
            content: Content::StaticBytes {
                bytes: Arc::new(b"0123456789".to_vec()),
                content_type: None,
            },
            delivery: Delivery::EarlyClose { after_bytes: 2 },
        });
        for client in [&real, &modeled] {
            assert!(matches!(
                client.get_bytes(truncated.url(), None).await,
                Err(NetError::Network(_))
            ));
        }
    }

    #[cfg(feature = "flash")]
    #[kithara::test(tokio, flash(true))]
    #[case::completes(Duration::from_secs(3), false)]
    #[case::times_out(Duration::from_millis(500), true)]
    async fn router_delay_obeys_virtual_inactivity(
        #[case] inactivity: Duration,
        #[case] times_out: bool,
    ) {
        let helper = TestServerHelper::for_server(
            TestServerState::new(),
            Url::parse("http://fixture.invalid").unwrap(),
        );
        let fixture =
            helper
                .create_hls(HlsFixtureBuilder::new().segment_size(2048).push_delay_rule(
                    DelayRule {
                        delay_ms: 2000,
                        ..DelayRule::default()
                    },
                ))
                .await
                .unwrap();
        let client = helper.modeled_client(options(inactivity), CancelToken::never());
        let wall = WallInstant::now();
        let start = Instant::now();
        let result = client.get_bytes(fixture.segment_url(0, 0), None).await;
        if times_out {
            assert!(matches!(result, Err(NetError::Timeout)));
            assert!(start.elapsed() >= inactivity);
        } else {
            assert_eq!(result.unwrap().len(), 2048);
            assert!(start.elapsed() >= Duration::from_secs(2));
        }
        assert!(wall.elapsed() < Duration::from_secs(1));
    }

    #[kithara::test(tokio)]
    #[case::drop(false)]
    #[case::cancel(true)]
    async fn retiring_modeled_body_releases_its_gate(#[case] cancel_request: bool) {
        let helper = TestServerHelper::for_server(
            TestServerState::new(),
            Url::parse("http://fixture.invalid").unwrap(),
        );
        let handle = helper.register_behavior(FixtureBehavior {
            content: Content::StaticBytes {
                bytes: Arc::new(b"0123456789".to_vec()),
                content_type: None,
            },
            delivery: Delivery::Gated { after_bytes: 2 },
        });
        let cancel = cancel_token();
        let client = helper.modeled_client(options(Duration::from_secs(3)), cancel.clone());
        let mut body = client.stream(handle.url(), None).await.unwrap();
        assert_eq!(body.next().await.unwrap().unwrap(), "01");
        assert!(futures::poll!(body.next()).is_pending());
        assert_eq!(handle.body_pending(), 1);
        if cancel_request {
            cancel.cancel();
            assert!(matches!(body.next().await, Some(Err(NetError::Cancelled))));
            assert!(body.next().await.is_none());
        } else {
            drop(body);
        }
        assert_eq!(handle.body_pending(), 0);
    }

    #[kithara::test(tokio)]
    async fn modeled_transport_refuses_other_origins() {
        let helper = TestServerHelper::for_server(
            TestServerState::new(),
            Url::parse("http://fixture.invalid").unwrap(),
        );
        let client = helper.modeled_client(options(Duration::from_secs(3)), CancelToken::never());
        assert!(matches!(
            client
                .get_bytes(Url::parse("https://external.invalid/media").unwrap(), None)
                .await,
            Err(NetError::Refused(_))
        ));
    }

    #[kithara::test(tokio)]
    async fn cancellation_of_derived_handle_releases_pending_headers() {
        let state = TestServerState::new();
        let helper = TestServerHelper::for_server(
            Arc::clone(&state),
            Url::parse("http://fixture.invalid").unwrap(),
        );
        let fixture = helper.create_hls(HlsFixtureBuilder::new()).await.unwrap();
        let gate = state.register_segment_gate(fixture.token(), 0, 0);
        let cancel = cancel_token();
        let client = helper.modeled_client(options(Duration::from_secs(3)), cancel.clone());
        let observed = client.with_observer(None);
        let mut request = Box::pin(observed.get_bytes(fixture.segment_url(0, 0), None));
        assert!(futures::poll!(request.as_mut()).is_pending());
        assert_eq!(gate.requested(), 1);
        assert_eq!(gate.pending(), 1);
        cancel.cancel();
        assert!(matches!(request.await, Err(NetError::Cancelled)));
        assert_eq!(gate.pending(), 0);
        assert!(matches!(
            client.get_bytes(fixture.segment_url(0, 0), None).await,
            Err(NetError::Cancelled)
        ));
        assert_eq!(gate.requested(), 1);
    }

    #[cfg(feature = "flash")]
    #[kithara::test(tokio, flash(true))]
    #[case::real_first(true)]
    #[case::modeled_first(false)]
    async fn pending_real_body_keeps_shared_clock_pacing(#[case] real_first: bool) {
        let server = PrivateTestServer::start().await;
        let helper = server.helper();
        let handle = helper.register_behavior(FixtureBehavior {
            content: Content::StaticBytes {
                bytes: Arc::new(b"0123456789".to_vec()),
                content_type: None,
            },
            delivery: Delivery::Gated { after_bytes: 2 },
        });
        let net_options = options(Duration::from_secs(3));
        let real = HttpClient::new(net_options.clone(), pools(), CancelToken::never());
        let modeled = helper.modeled_client(net_options, CancelToken::never());
        let fixture =
            helper
                .create_hls(HlsFixtureBuilder::new().segment_size(2048).push_delay_rule(
                    DelayRule {
                        delay_ms: 2000,
                        ..DelayRule::default()
                    },
                ))
                .await
                .unwrap();
        let mut real_body = real.stream(handle.url(), None).await.unwrap();
        assert_eq!(real_body.next().await.unwrap().unwrap(), "01");
        let mut delayed = Box::pin(modeled.get_bytes(fixture.segment_url(0, 0), None));
        let start = Instant::now();
        let wall = WallInstant::now();
        if real_first {
            assert!(futures::poll!(real_body.next()).is_pending());
            assert!(futures::poll!(delayed.as_mut()).is_pending());
        } else {
            assert!(futures::poll!(delayed.as_mut()).is_pending());
            assert!(futures::poll!(real_body.next()).is_pending());
        }
        real_pause().await;
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(futures::poll!(delayed.as_mut()).is_pending());
        drop(real_body);
        assert_eq!(delayed.await.unwrap().len(), 2048);
        assert!(start.elapsed() >= Duration::from_secs(2));
        assert!(wall.elapsed() < Duration::from_secs(1));
    }

    #[cfg(all(feature = "all", feature = "flash"))]
    #[kithara::test(tokio, flash(true))]
    #[case::real_first_complete(true, false)]
    #[case::modeled_first_complete(false, false)]
    #[case::real_first_cancel(true, true)]
    #[case::modeled_first_cancel(false, true)]
    async fn pending_real_loader_keeps_shared_clock_pacing(
        #[case] real_first: bool,
        #[case] cancel_load: bool,
    ) {
        let server = PrivateTestServer::start().await;
        let helper = server.helper();
        let fixture = helper
            .create_hls(
                HlsFixtureBuilder::new()
                    .variant_count(1)
                    .segments_per_variant(4)
                    .segment_duration_secs(1.0)
                    .packaged_audio_aac_lc(44_100, 2),
            )
            .await
            .unwrap();
        let gate = helper.register_init_gate(fixture.token(), 0);
        let delayed_fixture =
            helper
                .create_hls(HlsFixtureBuilder::new().segment_size(2048).push_delay_rule(
                    DelayRule {
                        delay_ms: 2000,
                        ..DelayRule::default()
                    },
                ))
                .await
                .unwrap();
        let region = pools();
        let cancel = cancel_token();
        let worker = PlayWorker::new(PlayWorkerConfig::builder(region.clone()).build());
        let downloader = Downloader::new(
            DownloaderConfig::for_client(HttpClient::new(
                options(Duration::from_secs(3)),
                region.clone(),
                cancel.child(),
            ))
            .cancel(cancel.child())
            .build(),
        );
        let hls = HlsConfig::for_url(fixture.master_url())
            .downloader(downloader)
            .store(
                AssetStore::builder(region.clone())
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .pools(region)
            .cancel(cancel.child())
            .initial_abr_mode(kithara::abr::AbrMode::manual(0))
            .build();
        let config = TrackConfig::for_audio(
            AudioConfig::<Hls<_>>::for_stream(hls)
                .cancel(cancel.clone())
                .build(),
        )
        .block_on_underrun(true)
        .build();
        let modeled = helper.modeled_client(options(Duration::from_secs(3)), cancel_token());
        let mut loading = Box::pin(crate::mock::load_audio(&worker, config));
        let mut delayed = Box::pin(modeled.get_bytes(delayed_fixture.segment_url(0, 0), None));
        let start = Instant::now();
        let wall = WallInstant::now();
        if real_first {
            assert!(futures::poll!(loading.as_mut()).is_pending());
            assert!(futures::poll!(delayed.as_mut()).is_pending());
        } else {
            assert!(futures::poll!(delayed.as_mut()).is_pending());
            assert!(futures::poll!(loading.as_mut()).is_pending());
        }
        while gate.requested() == 0 {
            assert!(
                wall.elapsed() < Duration::from_secs(1),
                "real init GET never arrived"
            );
            assert!(futures::poll!(loading.as_mut()).is_pending());
            real_pause().await;
        }
        real_pause().await;
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(futures::poll!(loading.as_mut()).is_pending());
        assert!(futures::poll!(delayed.as_mut()).is_pending());
        if cancel_load {
            cancel.cancel();
            let refusal = loading.await.err().expect("cancelled load must refuse");
            assert!(
                matches!(&refusal, kithara_render::LoadRefusal::Cancelled),
                "{refusal:?}"
            );
            gate.release();
        } else {
            gate.release();
            let audio = loading.await.expect("real gated load completes");
            let (audio, samples) =
                crate::reads::blocking_audio(audio, crate::reads::read_to_eof).await;
            assert!(
                samples > 0,
                "real load must produce finite PCM through natural EOF"
            );
            drop(audio);
            cancel.cancel();
        }
        assert_eq!(delayed.await.unwrap().len(), 2048);
        assert!(start.elapsed() >= Duration::from_secs(2));
        assert!(wall.elapsed() < Duration::from_secs(1));
    }

    #[cfg(feature = "flash")]
    #[kithara::flash(false)]
    async fn real_pause() {
        kithara::platform::time::sleep(Duration::from_millis(30)).await;
    }
}
