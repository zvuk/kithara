use bytes::Bytes;
use url::Url;

use super::{Client, ClientBuilder, RequestBuilder};
use crate::types::Compression;

pub(crate) fn head_request(client: &Client, url: Url) -> RequestBuilder {
    client.head(url)
}

pub(crate) fn post_request(client: &Client, url: Url, body: Bytes) -> RequestBuilder {
    client.post(url).body(body)
}

/// A native `ClientBuilder` transform. Used to disable individual
/// compression algorithms (`no_gzip` etc.) so the advertised
/// `Accept-Encoding` stays in lockstep with [`Compression`]. The builder
/// type is the active native backend — `wreq` or `reqwest`.
type ClientBuilderMod = fn(ClientBuilder) -> ClientBuilder;

impl From<Compression> for Vec<ClientBuilderMod> {
    fn from(c: Compression) -> Self {
        [
            (
                Compression::GZIP,
                ClientBuilder::no_gzip as ClientBuilderMod,
            ),
            (Compression::DEFLATE, ClientBuilder::no_deflate),
            (Compression::BROTLI, ClientBuilder::no_brotli),
            (Compression::ZSTD, ClientBuilder::no_zstd),
        ]
        .into_iter()
        .filter(|(flag, _)| !c.contains(*flag))
        .map(|(_, disable)| disable)
        .collect()
    }
}

pub(crate) fn apply_compression(builder: ClientBuilder, c: Compression) -> ClientBuilder {
    Vec::<ClientBuilderMod>::from(c)
        .into_iter()
        .fold(builder, |b, disable| disable(b))
}
