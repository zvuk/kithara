use std::{collections::HashMap, fmt};

use bytes::Bytes;
use kithara::{
    drm::{KeyProcessor, KeyRequest, KeyRequestFactory, UniqueBinaryCipher},
    platform::sync::Arc,
    play::policy::{DomainKeyPolicy, DomainKeyRule},
};
use rand::{Rng as _, RngExt as _, distr::Alphanumeric};

use super::schema::{Drm, DrmProvider, SeedAlphabet, SeedSpec};

/// Header the key request generates per fetch; a document must not set it.
const GENERATED_HEADER: &str = "X-Encrypted-Key";

/// A provider a document declared in a way no policy can honour.
#[derive(Debug)]
#[non_exhaustive]
pub enum PolicyError {
    /// The document set the header the request factory generates.
    ReservedHeader { provider: String },
    /// A salt of no length leaves the cipher key unsalted.
    EmptySeed { provider: String },
    /// A hex salt of odd length cannot come from whole bytes.
    OddHexSeed { provider: String, length: usize },
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReservedHeader { provider } => write!(
                f,
                "provider `{provider}` must not set `{GENERATED_HEADER}` -- it is generated per request from the cipher key"
            ),
            Self::EmptySeed { provider } => write!(
                f,
                "provider `{provider}` seed.length must be greater than zero -- an empty salt sends `{GENERATED_HEADER}` blank and ciphers on the bare key"
            ),
            Self::OddHexSeed { provider, length } => write!(
                f,
                "provider `{provider}` seed.length must be even for alphabet=hex (got {length})"
            ),
        }
    }
}

impl std::error::Error for PolicyError {}

/// Build the ordered domain policy the DRM registry resolves through.
///
/// # Errors
/// Returns the first provider that declares a policy no rule can honour.
pub(crate) fn drm_policy(drm: &Drm) -> Result<DomainKeyPolicy, PolicyError> {
    let rules = drm
        .providers
        .iter()
        .map(rule)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DomainKeyPolicy::new(rules))
}

fn rule(provider: &DrmProvider) -> Result<DomainKeyRule, PolicyError> {
    if provider.headers.contains_key(GENERATED_HEADER) {
        return Err(PolicyError::ReservedHeader {
            provider: provider.name.clone(),
        });
    }
    if provider.seed.length == 0 {
        return Err(PolicyError::EmptySeed {
            provider: provider.name.clone(),
        });
    }
    if provider.seed.alphabet == SeedAlphabet::Hex && !provider.seed.length.is_multiple_of(2) {
        return Err(PolicyError::OddHexSeed {
            provider: provider.name.clone(),
            length: provider.seed.length,
        });
    }

    let cipher_key: Arc<str> = Arc::from(provider.cipher_key.as_str());
    let seed = provider.seed.clone();
    let factory: KeyRequestFactory = Arc::new(move || {
        let salt = salt(&seed);
        let cipher = UniqueBinaryCipher::new(&format!("{cipher_key}{salt}"));
        let processor: KeyProcessor = Arc::new(move |key: Bytes| Ok(cipher.decrypt(&key)));
        KeyRequest::new(
            HashMap::from([(GENERATED_HEADER.to_string(), salt)]),
            processor,
        )
    });

    Ok(DomainKeyRule::for_domains(&provider.domains, factory)
        .headers(
            provider
                .headers
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
        )
        .build())
}

/// Fresh salt for one key request. The upstream WAF validates alphabet and
/// length -- iOS ships an 8-char lowercase-hex salt for zvuk.com while zvq.me
/// staging takes a 16-char alphanumeric one -- so both come from the provider's
/// own declaration.
fn salt(seed: &SeedSpec) -> String {
    match seed.alphabet {
        SeedAlphabet::Hex => {
            let mut bytes = vec![0u8; seed.length / 2];
            rand::rng().fill_bytes(&mut bytes);
            bytes.iter().map(|byte| format!("{byte:02x}")).collect()
        }
        SeedAlphabet::Alphanumeric => rand::rng()
            .sample_iter(Alphanumeric)
            .take(seed.length)
            .map(char::from)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use kithara::{drm::KeyRequestResolver as _, play::policy::DomainKeyPolicy};
    use url::Url;

    use super::{PolicyError, drm_policy};
    use crate::{
        baked::BAKED_DOCUMENT,
        document::schema::{Document, Drm},
    };

    const PROVIDER: &str = concat!(
        "drm:\n  providers:\n    - name: example\n",
        "      domains: [example.com, \"*.example.com\"]\n",
        "      cipher_key: secret\n",
        "      headers:\n        X-Auth-Token: token\n",
    );

    fn policy(source: &str) -> DomainKeyPolicy {
        let document: Document = serde_yaml_ng::from_str(source).expect("valid document");
        drm_policy(&document.drm).expect("valid policy")
    }

    fn url(text: &str) -> Url {
        Url::parse(text).expect("valid url")
    }

    #[kithara::test(native, flash(false))]
    fn a_declared_header_reaches_the_matching_resource() {
        let policy = policy(PROVIDER);

        let headers = policy
            .resource_headers(&url("https://cdn.example.com/master.m3u8"))
            .expect("the wildcard rule matches");

        assert!(
            headers
                .iter()
                .any(|(name, value)| name == "X-Auth-Token" && value == "token"),
            "declared header missing from {headers:?}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_host_no_rule_names_gets_no_headers() {
        let policy = policy(PROVIDER);

        assert!(
            policy
                .resource_headers(&url("https://other.test/master.m3u8"))
                .is_none()
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_hex_salt_is_lowercase_hex_of_the_declared_length() {
        let policy = policy(PROVIDER);

        let prepared = policy
            .prepare(&url("https://example.com/keyserver/key"))
            .expect("the exact rule matches");

        let salt = prepared.headers["X-Encrypted-Key"].clone();
        assert_eq!(salt.len(), 8);
        assert!(
            salt.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "not lowercase hex: {salt}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn an_alphanumeric_salt_takes_the_declared_alphabet_and_length() {
        let policy = policy(&format!(
            "{PROVIDER}      seed:\n        length: 16\n        alphabet: alphanumeric\n"
        ));

        let prepared = policy
            .prepare(&url("https://example.com/keyserver/key"))
            .expect("the exact rule matches");

        let salt = prepared.headers["X-Encrypted-Key"].clone();
        assert_eq!(salt.len(), 16);
        assert!(salt.chars().all(|c| c.is_ascii_alphanumeric()), "{salt}");
    }

    #[kithara::test(native, flash(false))]
    fn each_key_request_carries_a_fresh_salt() {
        let policy = policy(PROVIDER);
        let key_url = url("https://example.com/keyserver/key");

        let first = policy.prepare(&key_url).expect("the exact rule matches");
        let second = policy.prepare(&key_url).expect("the exact rule matches");

        assert_ne!(
            first.headers["X-Encrypted-Key"], second.headers["X-Encrypted-Key"],
            "a salt reused across requests is not a salt"
        );
    }

    #[kithara::test(native, flash(false))]
    fn declaring_the_generated_header_is_refused() {
        let document: Document = serde_yaml_ng::from_str(concat!(
            "drm:\n  providers:\n    - name: example\n      domains: [example.com]\n",
            "      cipher_key: secret\n      headers:\n        X-Encrypted-Key: mine\n",
        ))
        .expect("valid document");

        let error = drm_policy(&document.drm).expect_err("the header is generated per request");

        assert!(matches!(error, PolicyError::ReservedHeader { .. }));
    }

    /// One provider whose only distinguishing feature is its declared seed.
    fn seed(length: usize, alphabet: &str) -> Drm {
        let document: Document = serde_yaml_ng::from_str(&format!(
            concat!(
                "drm:\n  providers:\n    - name: example\n      domains: [example.com]\n",
                "      cipher_key: secret\n",
                "      seed:\n        length: {length}\n        alphabet: {alphabet}\n",
            ),
            length = length,
            alphabet = alphabet
        ))
        .expect("valid document");
        document.drm
    }

    #[kithara::test(native, flash(false))]
    fn a_hex_salt_of_no_length_is_refused() {
        let error = drm_policy(&seed(0, "hex")).expect_err("an empty salt is not a salt");

        assert!(matches!(error, PolicyError::EmptySeed { .. }), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn an_alphanumeric_salt_of_no_length_is_refused() {
        let error = drm_policy(&seed(0, "alphanumeric")).expect_err("an empty salt is not a salt");

        assert!(matches!(error, PolicyError::EmptySeed { .. }), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn an_odd_hex_salt_length_is_refused() {
        let error = drm_policy(&seed(7, "hex")).expect_err("hex needs whole bytes");

        assert!(matches!(error, PolicyError::OddHexSeed { length: 7, .. }));
    }

    fn shipped_salt(key_url: &str) -> String {
        let document: Document =
            serde_yaml_ng::from_str(BAKED_DOCUMENT).expect("the baked document parses");
        let policy = drm_policy(&document.drm).expect("the shipped providers are valid");
        policy
            .prepare(&url(key_url))
            .expect("a shipped rule matches")
            .headers["X-Encrypted-Key"]
            .clone()
    }

    /// `app.yaml` warns that the prod WAF validates alphabet and length and
    /// answers 418 on deviation. Nothing else pins the shipped shape.
    #[kithara::test(native, flash(false))]
    fn the_shipped_prod_provider_salts_with_eight_lowercase_hex_characters() {
        let salt = shipped_salt("https://cdn-hls-slicer.zvuk.com/drm/track/0/key.bin");

        assert_eq!(salt.len(), 8);
        assert!(
            salt.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "not lowercase hex: {salt}"
        );
    }

    /// The 8-char hex prod format on stage silently corrupts decrypt output, so
    /// the stage shape is pinned separately from prod's.
    #[kithara::test(native, flash(false))]
    fn the_shipped_stage_provider_salts_with_sixteen_alphanumeric_characters() {
        let salt = shipped_salt("https://ecs-stage-slicer-01.zvq.me/drm/track/0/key.bin");

        assert_eq!(salt.len(), 16);
        assert!(salt.chars().all(|c| c.is_ascii_alphanumeric()), "{salt}");
    }

    #[kithara::test(native, flash(false))]
    fn every_shipped_provider_domain_reaches_its_rule() {
        let document: Document =
            serde_yaml_ng::from_str(BAKED_DOCUMENT).expect("the baked document parses");

        let policy = drm_policy(&document.drm).expect("the shipped providers are valid");

        for provider in &document.drm.providers {
            for domain in &provider.domains {
                // A wildcard is probed through a subdomain; a bare domain is
                // probed as itself. Prefixing every host would let the
                // wildcard entry answer for the bare one and hide a provider
                // whose only domain never matches.
                let host = domain
                    .strip_prefix("*.")
                    .map_or_else(|| domain.clone(), |bare| format!("cdn.{bare}"));
                let probe = url(&format!("https://{host}/master.m3u8"));
                assert!(
                    policy.prepare(&probe).is_some(),
                    "no rule matched {domain} from provider {}",
                    provider.name
                );
            }
        }
    }
}
