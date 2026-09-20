use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

use hls_m3u8::{MasterPlaylist, MediaPlaylist, tags::VariantStream, types::EncryptionMethod};
use kithara_platform::time::{Duration, Instant};
use rayon::prelude::*;
use reqwest::{StatusCode, blocking::Client, header::HeaderMap};
use thiserror::Error;
use url::Url;

use crate::{
    context::BuildContext,
    hls_manifest::{Manifest, Resource},
    store,
};

const DOWNLOADS: usize = 4;

type KeyProcessor = Box<dyn Fn(Vec<u8>) -> Result<Vec<u8>, String> + Send + Sync>;

pub(crate) struct KeyPolicy {
    pub(crate) headers: HeaderMap,
    pub(crate) processor: KeyProcessor,
}

pub(crate) struct Options {
    pub(crate) refresh: &'static [&'static str],
    pub(crate) timeout: Duration,
    pub(crate) headers: HeaderMap,
    pub(crate) key: Option<KeyPolicy>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Key,
    Media,
    Playlist,
}

impl Kind {
    fn content_type(self, url: &Url) -> &'static str {
        match (self, self.extension(url)) {
            (Self::Playlist, _) => "application/vnd.apple.mpegurl",
            (Self::Media, "aac") => "audio/aac",
            (Self::Media, "m4s" | "mp4" | "m4a") => "audio/mp4",
            (Self::Media, "ts") => "video/mp2t",
            (Self::Key | Self::Media, _) => "application/octet-stream",
        }
    }

    fn extension(self, url: &Url) -> &str {
        match self {
            Self::Key => "key",
            Self::Playlist => "m3u8",
            Self::Media => url
                .path_segments()
                .and_then(Iterator::last)
                .and_then(|name| name.rsplit_once('.').map(|(_, ext)| ext))
                .filter(|ext| !ext.is_empty())
                .unwrap_or("bin"),
        }
    }
}

#[derive(Clone, Debug, derive_more::Display)]
#[display("{_0}")]
pub(crate) struct RedactedUrl(String);

impl RedactedUrl {
    pub(crate) fn new(url: &Url) -> Self {
        Self(format!(
            "{}{}",
            url.origin().ascii_serialization(),
            url.path()
        ))
    }
}

#[derive(Debug, Error)]
pub(crate) enum HydrateError {
    #[error("request for {url} failed: {source}")]
    Request {
        url: RedactedUrl,
        #[source]
        source: reqwest::Error,
    },
    #[error("request for {url} returned HTTP {status}; refresh repository variable(s) {refresh}")]
    AuthStatus {
        url: RedactedUrl,
        status: StatusCode,
        refresh: String,
    },
    #[error("request for {url} returned HTTP {status}")]
    Status {
        url: RedactedUrl,
        status: StatusCode,
    },
    #[error("fixture hydration budget expired before {url}")]
    Timeout { url: RedactedUrl },
    #[error("invalid master playlist {url}")]
    Master {
        url: RedactedUrl,
        #[source]
        source: hls_m3u8::Error,
    },
    #[error("invalid media playlist {url}")]
    MediaPlaylist {
        url: RedactedUrl,
        #[source]
        source: hls_m3u8::Error,
    },
    #[error("playlist {url} is not UTF-8: {source}")]
    Utf8 {
        url: RedactedUrl,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("HLS fixture is not VOD: {url}")]
    NotVod { url: RedactedUrl },
    #[error("unsupported SAMPLE-AES encryption in {url}")]
    SampleAes { url: RedactedUrl },
    #[error("cannot resolve URI from {base}: {source}")]
    Resolve {
        base: RedactedUrl,
        #[source]
        source: url::ParseError,
    },
    #[error("one HLS URI was used as both {first:?} and {second:?}: {url}")]
    ConflictingKind {
        url: RedactedUrl,
        first: Kind,
        second: Kind,
    },
    #[error("key processing failed for {url}: {reason}")]
    Key { url: RedactedUrl, reason: String },
    #[error("AES-128 key from {url} has {actual} bytes, expected 16")]
    KeyLength { url: RedactedUrl, actual: usize },
    #[error("cannot build bounded HLS downloader: {0}")]
    Pool(#[from] rayon::ThreadPoolBuildError),
    #[error("cannot store HLS resource: {0}")]
    Store(#[from] std::io::Error),
    #[error("cannot encode HLS bundle manifest: {0}")]
    Manifest(#[from] toml::ser::Error),
}

#[derive(Clone, Copy)]
pub(crate) struct Deadline {
    end: Instant,
}

impl Deadline {
    pub(crate) fn new(timeout: Duration) -> Self {
        Self {
            end: Instant::now() + timeout,
        }
    }

    fn remaining(self, url: &Url) -> Result<Duration, HydrateError> {
        let remaining = self.end.saturating_duration_since(Instant::now());
        (!remaining.is_zero())
            .then_some(remaining)
            .ok_or_else(|| HydrateError::Timeout {
                url: RedactedUrl::new(url),
            })
    }
}

fn refresh_names(names: &[&str]) -> String {
    names.join(", ")
}

pub(crate) fn fetch(
    client: &Client,
    url: &Url,
    headers: &HeaderMap,
    deadline: Deadline,
    refresh: &[&str],
) -> Result<Vec<u8>, HydrateError> {
    let response = client
        .get(url.clone())
        .headers(headers.clone())
        .timeout(deadline.remaining(url)?)
        .send()
        .map_err(|source| HydrateError::Request {
            url: RedactedUrl::new(url),
            source: source.without_url(),
        })?;
    let status = response.status();
    if !status.is_success() {
        return if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            Err(HydrateError::AuthStatus {
                status,
                url: RedactedUrl::new(url),
                refresh: refresh_names(refresh),
            })
        } else {
            Err(HydrateError::Status {
                status,
                url: RedactedUrl::new(url),
            })
        };
    }
    response
        .bytes()
        .map(Vec::from)
        .map_err(|source| HydrateError::Request {
            url: RedactedUrl::new(url),
            source: source.without_url(),
        })
}

fn resolve(base: &Url, reference: &str) -> Result<Url, HydrateError> {
    base.join(reference)
        .map_err(|source| HydrateError::Resolve {
            source,
            base: RedactedUrl::new(base),
        })
}

fn insert_kind(
    resources: &mut BTreeMap<Url, Kind>,
    url: &Url,
    kind: Kind,
) -> Result<(), HydrateError> {
    if let Some(first) = resources.insert(url.clone(), kind)
        && first != kind
    {
        return Err(HydrateError::ConflictingKind {
            first,
            url: RedactedUrl::new(url),
            second: kind,
        });
    }
    Ok(())
}

fn route(url: &Url, kind: Kind) -> String {
    format!(
        "/hls/{}.{}",
        store::asset_id("route", url.as_str()),
        kind.extension(url)
    )
}

fn playlist_urls(master: &MasterPlaylist<'_>, base: &Url) -> Result<Vec<Url>, HydrateError> {
    let mut urls = BTreeSet::new();
    for stream in &master.variant_streams {
        let uri = match stream {
            VariantStream::ExtXIFrame { uri, .. } | VariantStream::ExtXStreamInf { uri, .. } => uri,
        };
        urls.insert(resolve(base, uri)?);
    }
    for media in &master.media {
        if let Some(uri) = media.uri() {
            urls.insert(resolve(base, uri)?);
        }
    }
    Ok(urls.into_iter().collect())
}

fn collect_media_resources(
    playlist: &MediaPlaylist<'_>,
    base: &Url,
    resources: &mut BTreeMap<Url, Kind>,
) -> Result<(), HydrateError> {
    if !playlist.has_end_list {
        return Err(HydrateError::NotVod {
            url: RedactedUrl::new(base),
        });
    }
    for segment in playlist.segments.values() {
        insert_kind(resources, &resolve(base, segment.uri())?, Kind::Media)?;
        if let Some(map) = &segment.map {
            insert_kind(resources, &resolve(base, map.uri())?, Kind::Media)?;
        }
        for key in segment.keys.iter().filter_map(|key| key.as_ref()) {
            if key.method == EncryptionMethod::SampleAes {
                return Err(HydrateError::SampleAes {
                    url: RedactedUrl::new(base),
                });
            }
            if key.method == EncryptionMethod::Aes128 {
                insert_kind(resources, &resolve(base, key.uri())?, Kind::Key)?;
            }
        }
    }
    Ok(())
}

fn rewrite_media(playlist: &mut MediaPlaylist<'static>, base: &Url) -> Result<(), HydrateError> {
    for segment in playlist.segments.values_mut() {
        let media_url = resolve(base, segment.uri())?;
        segment.set_uri(route(&media_url, Kind::Media));
        if let Some(map) = &mut segment.map {
            let map_url = resolve(base, map.uri())?;
            map.set_uri(route(&map_url, Kind::Media));
        }
        for key in segment.keys.iter_mut().filter_map(|key| key.0.as_mut()) {
            if key.method == EncryptionMethod::Aes128 {
                let key_url = resolve(base, key.uri())?;
                key.set_uri(route(&key_url, Kind::Key));
            }
        }
    }
    Ok(())
}

fn rewrite_master(master: &mut MasterPlaylist<'static>, base: &Url) -> Result<(), HydrateError> {
    for stream in &mut master.variant_streams {
        let uri = match stream {
            VariantStream::ExtXIFrame { uri, .. } | VariantStream::ExtXStreamInf { uri, .. } => uri,
        };
        let source = resolve(base, uri)?;
        *uri = Cow::Owned(route(&source, Kind::Playlist));
    }
    for media in &mut master.media {
        if let Some(uri) = media.uri() {
            let source = resolve(base, uri)?;
            media.set_uri(Some(route(&source, Kind::Playlist)));
        }
    }
    for session_key in &mut master.session_keys {
        if session_key.0.method == EncryptionMethod::SampleAes {
            return Err(HydrateError::SampleAes {
                url: RedactedUrl::new(base),
            });
        }
        if session_key.0.method == EncryptionMethod::Aes128 {
            let source = resolve(base, session_key.0.uri())?;
            session_key.0.set_uri(route(&source, Kind::Key));
        }
    }
    Ok(())
}

fn download_resource(
    client: &Client,
    url: &Url,
    kind: Kind,
    options: &Options,
    deadline: Deadline,
) -> Result<Vec<u8>, HydrateError> {
    if kind != Kind::Key {
        return fetch(client, url, &options.headers, deadline, options.refresh);
    }
    let Some(policy) = &options.key else {
        let bytes = fetch(client, url, &options.headers, deadline, options.refresh)?;
        return validate_key(url, bytes);
    };
    let bytes = fetch(client, url, &policy.headers, deadline, options.refresh)?;
    let processed = (policy.processor)(bytes).map_err(|reason| HydrateError::Key {
        reason,
        url: RedactedUrl::new(url),
    })?;
    validate_key(url, processed)
}

fn validate_key(url: &Url, bytes: Vec<u8>) -> Result<Vec<u8>, HydrateError> {
    if bytes.len() != 16 {
        return Err(HydrateError::KeyLength {
            url: RedactedUrl::new(url),
            actual: bytes.len(),
        });
    }
    Ok(bytes)
}

pub(crate) fn hydrate(
    context: &BuildContext<'_>,
    master_url: &Url,
    options: &Options,
) -> Result<Vec<u8>, HydrateError> {
    let deadline = Deadline::new(options.timeout);
    let client = Client::builder()
        .build()
        .map_err(|source| HydrateError::Request {
            url: RedactedUrl::new(master_url),
            source: source.without_url(),
        })?;
    let master_bytes = fetch(
        &client,
        master_url,
        &options.headers,
        deadline,
        options.refresh,
    )?;
    let master_text = std::str::from_utf8(&master_bytes).map_err(|source| HydrateError::Utf8 {
        source,
        url: RedactedUrl::new(master_url),
    })?;
    let mut master = MasterPlaylist::try_from(master_text)
        .map_err(|source| HydrateError::Master {
            source,
            url: RedactedUrl::new(master_url),
        })?
        .into_owned();
    let playlist_urls = playlist_urls(&master, master_url)?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(DOWNLOADS)
        .build()?;
    let playlist_bytes = pool.install(|| {
        playlist_urls
            .par_iter()
            .map(|url| {
                fetch(&client, url, &options.headers, deadline, options.refresh)
                    .map(|bytes| (url.clone(), bytes))
            })
            .collect::<Result<Vec<_>, _>>()
    })?;
    let mut playlists = Vec::with_capacity(playlist_bytes.len());
    let mut resources = BTreeMap::new();
    for (url, bytes) in playlist_bytes {
        let text = std::str::from_utf8(&bytes).map_err(|source| HydrateError::Utf8 {
            source,
            url: RedactedUrl::new(&url),
        })?;
        let playlist = text
            .parse::<MediaPlaylist>()
            .map_err(|source| HydrateError::MediaPlaylist {
                source,
                url: RedactedUrl::new(&url),
            })?
            .into_owned();
        collect_media_resources(&playlist, &url, &mut resources)?;
        insert_kind(&mut resources, &url, Kind::Playlist)?;
        playlists.push((url, playlist));
    }
    for session_key in &master.session_keys {
        if session_key.0.method == EncryptionMethod::SampleAes {
            return Err(HydrateError::SampleAes {
                url: RedactedUrl::new(master_url),
            });
        }
        if session_key.0.method == EncryptionMethod::Aes128 {
            insert_kind(
                &mut resources,
                &resolve(master_url, session_key.0.uri())?,
                Kind::Key,
            )?;
        }
    }

    let downloadable: Vec<_> = resources
        .iter()
        .filter(|(_, kind)| **kind != Kind::Playlist)
        .map(|(url, kind)| (url.clone(), *kind))
        .collect();
    let downloaded = pool.install(|| {
        downloadable
            .par_iter()
            .map(|(url, kind)| {
                download_resource(&client, url, *kind, options, deadline)
                    .map(|bytes| (url.clone(), *kind, bytes))
            })
            .collect::<Result<Vec<_>, _>>()
    })?;

    let mut manifest_resources = Vec::with_capacity(resources.len() + 1);
    for (url, kind, bytes) in downloaded {
        let ext = kind.extension(&url);
        manifest_resources.push(Resource {
            content_type: kind.content_type(&url).to_owned(),
            file: context.store(url.as_str(), ext, &bytes)?,
            route: route(&url, kind),
        });
    }
    for (url, mut playlist) in playlists {
        rewrite_media(&mut playlist, &url)?;
        let body = playlist.to_string();
        manifest_resources.push(Resource {
            content_type: Kind::Playlist.content_type(&url).to_owned(),
            file: context.store(url.as_str(), "m3u8", body.as_bytes())?,
            route: route(&url, Kind::Playlist),
        });
    }
    rewrite_master(&mut master, master_url)?;
    let master_body = master.to_string();
    let master_route = "/hls/master.m3u8".to_owned();
    manifest_resources.push(Resource {
        content_type: Kind::Playlist.content_type(master_url).to_owned(),
        file: context.store(master_url.as_str(), "m3u8", master_body.as_bytes())?,
        route: master_route.clone(),
    });
    manifest_resources.sort_by(|left, right| left.route.cmp(&right.route));
    Ok(toml::to_string(&Manifest {
        master: master_route,
        resources: manifest_resources,
    })?
    .into_bytes())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        thread::{self, JoinHandle},
    };

    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use reqwest::header::HeaderMap;
    use tempfile::TempDir;
    use tiny_http::{Response, Server, StatusCode as TinyStatus};
    use url::Url;

    use super::{HydrateError, Options, hydrate};
    use crate::{
        context::BuildContext,
        hls_manifest::Manifest,
        remote_file::{RemoteFileError, fetch_verified},
    };

    #[kithara::test(native, flash(false))]
    fn remote_file_rejects_truncation_corruption_and_missing_content() {
        let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let server = TestServer::new(
            HashMap::from([
                ("/valid", (200, b"abc".as_slice())),
                ("/short", (200, b"ab".as_slice())),
                ("/corrupt", (200, b"bad".as_slice())),
                ("/missing", (404, b"missing".as_slice())),
            ]),
            4,
        );
        let fetch = |path| {
            let url = server.url.join(path).expect("fixture URL");
            fetch_verified(&url, digest, 3, Duration::from_secs(2))
        };
        assert_eq!(fetch("valid").expect("verified bytes"), b"abc");
        assert!(matches!(
            fetch("short"),
            Err(RemoteFileError::Length {
                expected: 3,
                received: 2,
                ..
            })
        ));
        assert!(matches!(
            fetch("corrupt"),
            Err(RemoteFileError::Digest { .. })
        ));
        assert!(matches!(fetch("missing"), Err(RemoteFileError::Fetch(_))));
        assert_eq!(server.finish().values().sum::<usize>(), 4);
    }

    struct TestServer {
        handle: JoinHandle<HashMap<String, usize>>,
        url: Url,
    }

    impl TestServer {
        fn new(routes: HashMap<&'static str, (u16, &'static [u8])>, requests: usize) -> Self {
            let server = Server::http("127.0.0.1:0").expect("bind test server");
            let url =
                Url::parse(&format!("http://{}/", server.server_addr())).expect("test server URL");
            let handle = thread::spawn(move || {
                let mut counts = HashMap::new();
                for _ in 0..requests {
                    let Some(request) = server
                        .recv_timeout(Duration::from_secs(2))
                        .expect("receive request")
                    else {
                        break;
                    };
                    let path = request
                        .url()
                        .split('?')
                        .next()
                        .unwrap_or_else(|| request.url());
                    *counts.entry(path.to_owned()).or_insert(0) += 1;
                    let (status, body) = routes.get(path).copied().unwrap_or((404, b"missing"));
                    request
                        .respond(Response::from_data(body).with_status_code(TinyStatus(status)))
                        .expect("respond");
                }
                counts
            });
            Self { handle, url }
        }

        fn finish(self) -> HashMap<String, usize> {
            self.handle.join().expect("server thread")
        }
    }

    fn options() -> Options {
        Options {
            headers: HeaderMap::new(),
            key: None,
            refresh: &["TEST_TOKEN"],
            timeout: Duration::from_secs(2),
        }
    }

    #[kithara::test(native, flash(false))]
    fn downloads_rewrites_and_deduplicates_a_complete_vod() {
        const MASTER: &[u8] = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=64000\nv1.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=128000\nv2.m3u8\n";
        const V1: &[u8] = b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXT-X-BYTERANGE:4@0\n#EXTINF:4,\none.m4s\n#EXT-X-ENDLIST\n";
        const V2: &[u8] = b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4,\ntwo.m4s\n#EXT-X-ENDLIST\n";
        let server = TestServer::new(
            HashMap::from([
                ("/master.m3u8", (200, MASTER)),
                ("/v1.m3u8", (200, V1)),
                ("/v2.m3u8", (200, V2)),
                ("/init.mp4", (200, b"init".as_slice())),
                ("/key.bin", (200, b"0123456789abcdef".as_slice())),
                ("/one.m4s", (200, b"one-full-resource".as_slice())),
                ("/two.m4s", (200, b"two".as_slice())),
            ]),
            7,
        );
        let master_url = server.url.join("master.m3u8").expect("master URL");
        let temp = TempDir::new().expect("temporary store");
        let context = BuildContext::new(temp.path(), "bundle");

        let bytes = hydrate(&context, &master_url, &options()).expect("hydrate VOD");
        let manifest: Manifest =
            toml::from_str(std::str::from_utf8(&bytes).expect("manifest UTF-8"))
                .expect("manifest TOML");
        let counts = server.finish();

        assert_eq!(manifest.resources.len(), 7);
        assert_eq!(counts.get("/init.mp4"), Some(&1));
        assert_eq!(counts.get("/key.bin"), Some(&1));
        let playlists: Vec<_> = manifest
            .resources
            .iter()
            .filter(|resource| resource.content_type == "application/vnd.apple.mpegurl")
            .map(|resource| {
                std::fs::read_to_string(temp.path().join(&resource.file))
                    .expect("rewritten playlist")
            })
            .collect();
        assert!(
            playlists
                .iter()
                .all(|playlist| !playlist.contains("127.0.0.1"))
        );
        assert!(
            playlists
                .iter()
                .any(|playlist| playlist.contains("#EXT-X-BYTERANGE:4@0"))
        );
        assert!(
            playlists
                .iter()
                .all(|playlist| !playlist.contains("key.bin"))
        );
    }

    #[kithara::test(native, flash(false))]
    fn auth_failure_names_the_variable_without_leaking_the_query() {
        let server = TestServer::new(
            HashMap::from([("/master.m3u8", (401, b"denied".as_slice()))]),
            1,
        );
        let master_url = server
            .url
            .join("master.m3u8?token=must-not-leak")
            .expect("master URL");
        let temp = TempDir::new().expect("temporary store");
        let context = BuildContext::new(temp.path(), "bundle");

        let error = hydrate(&context, &master_url, &options()).expect_err("401 must fail");
        let message = error.to_string();
        drop(server.finish());

        assert!(matches!(error, HydrateError::AuthStatus { .. }));
        assert!(message.contains("TEST_TOKEN"));
        assert!(!message.contains("must-not-leak"));
    }

    #[kithara::test(native, flash(false))]
    fn server_failure_does_not_blame_credentials() {
        let server = TestServer::new(
            HashMap::from([("/master.m3u8", (500, b"failed".as_slice()))]),
            1,
        );
        let master_url = server.url.join("master.m3u8").expect("master URL");
        let temp = TempDir::new().expect("temporary store");
        let context = BuildContext::new(temp.path(), "bundle");

        let error = hydrate(&context, &master_url, &options()).expect_err("500 must fail");
        let message = error.to_string();
        drop(server.finish());

        assert!(matches!(error, HydrateError::Status { .. }));
        assert!(!message.contains("TEST_TOKEN"));
        assert!(!message.contains("refresh"));
    }

    #[kithara::test(native, flash(false))]
    fn rejects_live_and_sample_aes_playlists() {
        for (playlist, expected) in [
            (
                b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\none.m4s\n".as_slice(),
                "not VOD",
            ),
            (
                b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"key.bin\"\n#EXTINF:4,\none.m4s\n#EXT-X-ENDLIST\n".as_slice(),
                "SAMPLE-AES",
            ),
        ] {
            let server = TestServer::new(
                HashMap::from([
                    (
                        "/master.m3u8",
                        (
                            200,
                            b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=64000\nmedia.m3u8\n"
                                .as_slice(),
                        ),
                    ),
                    ("/media.m3u8", (200, playlist)),
                ]),
                2,
            );
            let master_url = server.url.join("master.m3u8").expect("master URL");
            let temp = TempDir::new().expect("temporary store");
            let context = BuildContext::new(temp.path(), "bundle");

            let error = hydrate(&context, &master_url, &options()).expect_err(expected);
            drop(server.finish());

            assert!(error.to_string().contains(expected), "{error}");
        }
    }
}
