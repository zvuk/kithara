use std::{fmt::Display, fs::File, io::Read, path::PathBuf};

use bytes::Bytes;
use futures::StreamExt;
use kithara_abr::Abr;
use kithara_assets::{
    AcquisitionResult, AssetResource, AssetStore, AssetWriter, ReadSide, ResourceKey, WriteSide,
};
use kithara_bufpool::HasPool;
use kithara_download::{FetchCmd, Peer};
use kithara_net::{Headers, NetError, NetResult};
use kithara_platform::{
    CancelScope, CancelToken,
    sync::Arc,
    tokio::{self, task},
};
use thiserror::Error;
use url::Url;

use super::{ArtifactDocument, ArtifactRequest};
use crate::resource::{PlaybackResamplerBackend, ResourceConfig, ResourceSrc};

/// Largest document any artifact may arrive as. An analysis document or a
/// cover is a summary of a track, not a second copy of it: anything past this
/// is a wrong URL, not a big artifact, and it is refused as soon as its
/// declared length or its received bytes pass the cap.
pub const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;

/// Why an artifact document never reached the track it was read for.
///
/// Every variant is terminal for that read: nothing is read in its place.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ArtifactLoadError {
    /// The read was cancelled with the token it ran under.
    #[error("the {kind} artifact load was cancelled")]
    Cancelled { kind: &'static str },
    /// The bytes arrived but are not a document of this kind.
    #[error("the {kind} artifact is not a document this build reads: {reason}")]
    Decode { kind: &'static str, reason: String },
    /// The bytes never arrived, or the store could not hold them.
    #[error("the {kind} artifact at {src} could not be read: {reason}")]
    Fetch {
        kind: &'static str,
        src: String,
        reason: String,
    },
    /// A remote artifact needs the shared downloader the resource is wired
    /// with; without one there is no I/O path to read it over.
    #[error("no downloader is configured to read the {kind} artifact at {url}")]
    NoDownloader { kind: &'static str, url: Url },
    /// The document is past [`MAX_ARTIFACT_BYTES`].
    #[error("the {kind} artifact is {bytes} bytes, past the {limit} a document may hold")]
    TooLarge {
        kind: &'static str,
        bytes: u64,
        limit: usize,
    },
    /// The scheme is not one an artifact document is read over.
    #[error("{scheme} is not a scheme a {kind} artifact is read from")]
    Scheme { kind: &'static str, scheme: String },
}

/// The I/O a track lends its artifact documents: its shared downloader, its
/// asset store, the headers its audio is read with, and its own cancel token.
///
/// Built from a [`ResourceConfig`], so a document rides the same transport and
/// lands in the same store scope as the audio it belongs to.
pub struct ArtifactFetch<'a, S, B: Default = PlaybackResamplerBackend>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    config: &'a ResourceConfig<S, B>,
}

impl<'a, S, B: Default> ArtifactFetch<'a, S, B>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    #[must_use]
    pub const fn new(config: &'a ResourceConfig<S, B>) -> Self {
        Self { config }
    }

    /// Headers a track artifact sends to `url`. The audio source's credentials
    /// are the audio host's: an artifact served from anywhere else is fetched
    /// plain, so configuring an artifact URL can never leak a token to a third
    /// host.
    pub(super) fn headers_for(&self, url: &Url) -> Option<Headers> {
        let ResourceSrc::Url(audio) = self.config.source() else {
            return None;
        };
        (audio.host_str() == url.host_str() && audio.scheme() == url.scheme())
            .then(|| self.config.headers.clone())
            .flatten()
    }

    /// Read one artifact document of the track from `src`, under the track's
    /// own token, so it ends with the load it was read beside.
    ///
    /// # Errors
    ///
    /// Returns why the document could not be read or did not parse.
    pub async fn load<T: ArtifactDocument>(
        &self,
        src: &ResourceSrc,
    ) -> Result<T, ArtifactLoadError> {
        let cancel = self.config.cancel();
        if cancel.is_some_and(CancelToken::is_cancelled) {
            return Err(ArtifactLoadError::Cancelled { kind: T::KIND });
        }
        let bytes = match src {
            ResourceSrc::Path(path) => read_file::<T>(path.clone()).await?,
            ResourceSrc::Url(url) => {
                check_scheme::<T>(url)?;
                self.download::<T>(url, self.headers_for(url), cancel)
                    .await?
            }
        };
        decode(bytes)
    }

    /// Read the document `request` names, under `cancel`.
    ///
    /// A URL document goes through the track's asset store, keyed by the
    /// request's URL inside the track's own scope. A stored document is read
    /// without the network. A missing one is downloaded with exactly the
    /// request's headers and stored only once it reads. A read that finds the
    /// same document downloading waits for that download, and acquires again
    /// when it ends without storing. A path is read from its file.
    ///
    /// # Errors
    ///
    /// Returns why the document could not be read, stored, or parsed.
    pub async fn read<T>(
        &self,
        request: &ArtifactRequest,
        cancel: &CancelToken,
    ) -> Result<T, ArtifactLoadError>
    where
        T: ArtifactDocument + Send + 'static,
    {
        if cancel.is_cancelled() {
            return Err(ArtifactLoadError::Cancelled { kind: T::KIND });
        }
        match &request.src {
            ResourceSrc::Path(path) => decode(read_file::<T>(path.clone()).await?),
            ResourceSrc::Url(url) => {
                self.read_stored(url, request.headers.as_ref(), cancel)
                    .await
            }
        }
    }

    async fn read_stored<T>(
        &self,
        url: &Url,
        headers: Option<&Headers>,
        cancel: &CancelToken,
    ) -> Result<T, ArtifactLoadError>
    where
        T: ArtifactDocument + Send + 'static,
    {
        check_scheme::<T>(url)?;
        let key = self
            .config
            .asset_key(&AssetResource::Url(url.clone()))
            .map_err(|error| fetch_failure::<T>(url, error))?;
        loop {
            let store = self.config.store().clone();
            let (held_key, wait) = (key.clone(), cancel.clone());
            let held = task::spawn_blocking(move || hold(&store, &held_key, &wait))
                .await
                .map_err(|_| ArtifactLoadError::Cancelled { kind: T::KIND })?
                .map_err(|reason| fetch_failure::<T>(url, reason))?;
            match held {
                Held::Stored(bytes) => return decode(bytes),
                Held::Writer(writer) => return self.fill(writer, url, headers, cancel).await,
                Held::Ended if cancel.is_cancelled() => {
                    return Err(ArtifactLoadError::Cancelled { kind: T::KIND });
                }
                Held::Ended => {}
            }
        }
    }

    /// Download the document into `writer` and commit it only once it reads;
    /// any other end fails the writer, which wakes every read waiting on it.
    async fn fill<T>(
        &self,
        writer: AssetWriter<S>,
        url: &Url,
        headers: Option<&Headers>,
        cancel: &CancelToken,
    ) -> Result<T, ArtifactLoadError>
    where
        T: ArtifactDocument + Send + 'static,
    {
        let downloaded = self
            .download::<T>(url, headers.cloned(), Some(cancel))
            .await;
        let url = url.clone();
        task::spawn_blocking(move || settle(writer, downloaded, &url))
            .await
            .map_err(|_| ArtifactLoadError::Cancelled { kind: T::KIND })?
    }

    async fn download<T: ArtifactDocument>(
        &self,
        url: &Url,
        headers: Option<Headers>,
        cancel: Option<&CancelToken>,
    ) -> Result<Vec<u8>, ArtifactLoadError> {
        let downloader =
            self.config
                .downloader
                .as_ref()
                .ok_or_else(|| ArtifactLoadError::NoDownloader {
                    kind: T::KIND,
                    url: url.clone(),
                })?;
        let scope = CancelScope::new(cancel.cloned());
        let token = scope.token();
        // The downloader races an imperative fetch only against its peer, so
        // the fetch races the token here: ending it drops the peer, which
        // aborts the transfer in flight.
        let fetch = async {
            let handle = downloader.register(
                Arc::new(ArtifactPeer {
                    cancel: scope.token(),
                }),
                None,
            );
            let cmd = FetchCmd::get(url.clone())
                .cancel(scope.token())
                .maybe_headers(headers)
                .validator(reject_oversized_artifact)
                .build();
            let mut body = handle
                .execute(cmd)
                .await
                .map_err(|error| net_failure::<T>(url, error))?
                .body;
            let mut chunks: Vec<Bytes> = Vec::new();
            let mut received = 0;
            while let Some(chunk) = body.next().await {
                let chunk = chunk.map_err(|error| net_failure::<T>(url, error))?;
                received += chunk.len();
                if received > MAX_ARTIFACT_BYTES {
                    return Err(ArtifactLoadError::TooLarge {
                        bytes: received as u64,
                        kind: T::KIND,
                        limit: MAX_ARTIFACT_BYTES,
                    });
                }
                chunks.push(chunk);
            }
            Ok(chunks.concat())
        };
        tokio::select! {
            biased;
            () = token.cancelled() => Err(ArtifactLoadError::Cancelled { kind: T::KIND }),
            downloaded = fetch => downloaded,
        }
    }
}

/// What a store holds for one document key, as a read finds it.
enum Held<W> {
    /// The committed document's bytes.
    Stored(Vec<u8>),
    /// The writer this read now owns: the document is its to download.
    Writer(W),
    /// Another read's download ended, committed or not; acquire again.
    Ended,
}

/// Take `key` from `store`: its stored bytes, its writer, or, when another
/// read is downloading it, the end of that download.
fn hold<S>(
    store: &AssetStore<S>,
    key: &ResourceKey,
    cancel: &CancelToken,
) -> Result<Held<AssetWriter<S>>, String>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    match store
        .acquire_resource(key, None)
        .map_err(|error| error.to_string())?
    {
        AcquisitionResult::Ready(reader) => {
            let len = reader
                .len()
                .ok_or_else(|| "the stored document has no length".to_owned())?;
            let mut bytes = vec![0; usize::try_from(len).map_err(|error| error.to_string())?];
            let read = reader
                .read_at(0, &mut bytes)
                .map_err(|error| error.to_string())?;
            bytes.truncate(read);
            Ok(Held::Stored(bytes))
        }
        AcquisitionResult::Pending(writer) => Ok(Held::Writer(writer)),
        AcquisitionResult::Joined(reader) => {
            // The whole range is never written before a commit, so the wait
            // ends only with the writer: committed, failed, or dropped, or
            // with `cancel`. Each answer means the same here: acquire again.
            let _ = reader.wait_range_with_cancel(0..u64::MAX, cancel);
            Ok(Held::Ended)
        }
        _ => Err("the store answered with an acquisition this build does not read".to_owned()),
    }
}

/// Store a downloaded document only once it reads; fail the writer otherwise.
fn settle<T, S>(
    writer: AssetWriter<S>,
    downloaded: Result<Vec<u8>, ArtifactLoadError>,
    url: &Url,
) -> Result<T, ArtifactLoadError>
where
    T: ArtifactDocument,
    S: HasPool<u8> + Send + Sync + 'static,
{
    let bytes = match downloaded {
        Ok(bytes) => bytes,
        Err(error) => {
            writer.fail(error.to_string());
            return Err(error);
        }
    };
    let len = bytes.len() as u64;
    if let Err(error) = writer.write_at(0, &bytes) {
        writer.fail(error.to_string());
        return Err(fetch_failure::<T>(url, error));
    }
    match decode::<T>(bytes) {
        Ok(document) => {
            writer
                .commit(Some(len))
                .map_err(|error| fetch_failure::<T>(url, error))?;
            Ok(document)
        }
        Err(error) => {
            writer.fail(error.to_string());
            Err(error)
        }
    }
}

fn decode<T: ArtifactDocument>(bytes: Vec<u8>) -> Result<T, ArtifactLoadError> {
    T::decode(bytes).map_err(|reason| ArtifactLoadError::Decode {
        reason,
        kind: T::KIND,
    })
}

fn check_scheme<T: ArtifactDocument>(url: &Url) -> Result<(), ArtifactLoadError> {
    if matches!(url.scheme(), "http" | "https") {
        return Ok(());
    }
    Err(ArtifactLoadError::Scheme {
        kind: T::KIND,
        scheme: url.scheme().to_owned(),
    })
}

fn fetch_failure<T: ArtifactDocument>(url: &Url, reason: impl Display) -> ArtifactLoadError {
    ArtifactLoadError::Fetch {
        kind: T::KIND,
        src: url.to_string(),
        reason: reason.to_string(),
    }
}

fn net_failure<T: ArtifactDocument>(url: &Url, error: NetError) -> ArtifactLoadError {
    match error {
        NetError::Cancelled => ArtifactLoadError::Cancelled { kind: T::KIND },
        error => fetch_failure::<T>(url, error),
    }
}

async fn read_file<T: ArtifactDocument>(path: PathBuf) -> Result<Vec<u8>, ArtifactLoadError> {
    let display = path.display().to_string();
    let read = task::spawn_blocking(move || read_capped(&path))
        .await
        .map_err(|_| ArtifactLoadError::Cancelled { kind: T::KIND })?;
    match read {
        Ok(Capped::Bytes(bytes)) => Ok(bytes),
        Ok(Capped::TooLarge { bytes }) => Err(ArtifactLoadError::TooLarge {
            bytes,
            kind: T::KIND,
            limit: MAX_ARTIFACT_BYTES,
        }),
        Err(error) => Err(ArtifactLoadError::Fetch {
            kind: T::KIND,
            src: display,
            reason: error.to_string(),
        }),
    }
}

/// Refuse a response whose declared length is past the artifact cap, before
/// any of its body is buffered.
fn reject_oversized_artifact(headers: &Headers) -> NetResult<()> {
    let declared = headers
        .get("content-length")
        .and_then(|value| value.parse::<u64>().ok());
    match declared {
        Some(bytes) if bytes > MAX_ARTIFACT_BYTES as u64 => Err(NetError::InvalidContentType(
            format!("artifact document of {bytes} bytes"),
        )),
        _ => Ok(()),
    }
}

/// Outcome of reading a local document under the artifact cap.
enum Capped {
    Bytes(Vec<u8>),
    TooLarge { bytes: u64 },
}

/// Read at most [`MAX_ARTIFACT_BYTES`] from `path`, reporting a larger file
/// rather than buffering it.
fn read_capped(path: &PathBuf) -> std::io::Result<Capped> {
    let file = File::open(path)?;
    let declared = file.metadata()?.len();
    if declared > MAX_ARTIFACT_BYTES as u64 {
        return Ok(Capped::TooLarge { bytes: declared });
    }
    let mut bytes = Vec::with_capacity(usize::try_from(declared).unwrap_or(0));
    file.take(MAX_ARTIFACT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Ok(Capped::TooLarge {
            bytes: bytes.len() as u64,
        });
    }
    Ok(Capped::Bytes(bytes))
}

/// The downloader peer one artifact document is read over. It has no variants
/// and no buffer to report: it exists so the fetch rides the shared client and
/// dies with the token it was read under.
struct ArtifactPeer {
    cancel: CancelToken,
}

impl Abr for ArtifactPeer {
    fn cancel(&self) -> CancelToken {
        self.cancel.clone()
    }
}

impl Peer for ArtifactPeer {}
