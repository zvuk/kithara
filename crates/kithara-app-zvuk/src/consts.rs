/// Key requests to this domain carry the account token.
pub(crate) const KEY_DOMAIN: &str = "zvuk.com";
pub(crate) const AUTH_HEADER: &str = "X-Auth-Token";
pub(crate) const ENDPOINT: &str = "https://zvuk.com/api/v1/graphql/";
pub(crate) const TRACK_FIELDS: &str = "id title duration artists { title } release { title image { src } } collectionItemData { itemStatus }";
pub(crate) const MEDIA_FIELDS: &str =
    "... on Track { id streamV3 { hls expire } stream { mid high flac } }";
pub(crate) const SESSION_ENDPOINT: &str = "https://zvuk.com/api/tiny/login/qr/session";
pub(crate) const TOKEN_ENDPOINT: &str = "https://zvuk.com/api/tiny/login/qr/token";
pub(crate) const PROFILE_ENDPOINT: &str = "https://zvuk.com/api/v2/tiny/profile";
pub(crate) const LOGOUT_ENDPOINT: &str = "https://zvuk.com/api/tiny/logout";
/// The secret store key of the account token.
pub(crate) const STORE_KEY: &str = "zvuk";
/// The shortest pause between two confirmation polls.
pub(crate) const POLL_INTERVAL_MS: u64 = 2_000;
