use std::borrow::Cow;

use kithara_assets::{AssetLayout, AssetResource, AssetSource, DefaultLayout};
use sha2::{Digest, Sha256};
use url::Url;

use super::domain::DomainPattern;
use crate::consts;

/// Domain rule selecting the case-sensitive query keys that identify content.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct QueryIdentityRule {
    domains: Vec<DomainPattern>,
    keys: Vec<String>,
}

impl QueryIdentityRule {
    /// Create a rule for case-insensitive exact, `"*.domain"`, or `"*"` domains.
    /// Query key names remain case-sensitive.
    #[must_use]
    pub fn new<D, Q, Domains, Keys>(domains: Domains, keys: Keys) -> Self
    where
        D: AsRef<str>,
        Q: AsRef<str>,
        Domains: IntoIterator<Item = D>,
        Keys: IntoIterator<Item = Q>,
    {
        let domains = domains
            .into_iter()
            .map(|domain| DomainPattern::parse(domain.as_ref()))
            .collect();
        let mut unique_keys: Vec<String> = Vec::new();
        for key in keys {
            let key = key.as_ref();
            if !unique_keys.iter().any(|existing| existing == key) {
                unique_keys.push(key.to_string());
            }
        }
        Self {
            domains,
            keys: unique_keys,
        }
    }

    fn filtered_url(&self, url: &Url) -> Url {
        let pairs: Vec<(Cow<'_, str>, Cow<'_, str>)> = url.query_pairs().collect();
        let mut filtered = url.clone();
        filtered.set_query(None);
        filtered.set_fragment(None);

        let selected = pairs
            .iter()
            .any(|(name, _)| self.keys.iter().any(|key| key == name.as_ref()));
        if !selected {
            return filtered;
        }

        {
            let mut output = filtered.query_pairs_mut();
            for key in &self.keys {
                for (_, value) in pairs.iter().filter(|(name, _)| name.as_ref() == key) {
                    output.append_pair(key, value);
                }
            }
        }
        filtered
    }

    fn identity(&self, url: &Url) -> Option<String> {
        let pairs: Vec<(Cow<'_, str>, Cow<'_, str>)> = url.query_pairs().collect();
        let selected = pairs
            .iter()
            .any(|(name, _)| self.keys.iter().any(|key| key == name.as_ref()));
        if !selected {
            return None;
        }

        let mut hasher = Sha256::new();
        hasher.update(consts::IDENTITY_DOMAIN);
        for key in &self.keys {
            hash_field(&mut hasher, key.as_bytes());
            let values = pairs
                .iter()
                .filter(|(name, _)| name.as_ref() == key)
                .map(|(_, value)| value.as_bytes());
            hash_len(&mut hasher, values.clone().count());
            for value in values {
                hash_field(&mut hasher, value);
            }
        }
        Some(finish_hash(hasher))
    }

    fn matches(&self, url: &Url) -> bool {
        url.host_str()
            .is_some_and(|host| self.domains.iter().any(|domain| domain.matches(host)))
    }
}

/// Asset layout that adds configured query identity to source roots and URL paths.
/// The first matching rule wins; unmatched resources use [`DefaultLayout`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct QueryIdentityLayout {
    rules: Vec<QueryIdentityRule>,
}

impl QueryIdentityLayout {
    /// Create a layout whose first matching domain rule wins.
    #[must_use]
    pub fn new<I>(rules: I) -> Self
    where
        I: IntoIterator<Item = QueryIdentityRule>,
    {
        Self {
            rules: rules.into_iter().collect(),
        }
    }

    fn rule(&self, url: &Url) -> Option<&QueryIdentityRule> {
        self.rules.iter().find(|rule| rule.matches(url))
    }
}

impl AssetLayout for QueryIdentityLayout {
    fn path(&self, resource: &AssetResource) -> String {
        let AssetResource::Url(url) = resource else {
            return DefaultLayout.path(resource);
        };
        let Some(rule) = self.rule(url) else {
            return DefaultLayout.path(resource);
        };
        DefaultLayout.path(&AssetResource::Url(rule.filtered_url(url)))
    }

    fn root(&self, source: &AssetSource) -> String {
        let AssetSource::Remote { url, discriminator } = source else {
            return DefaultLayout.root(source);
        };
        let Some(identity) = self.rule(url).and_then(|rule| rule.identity(url)) else {
            return DefaultLayout.root(source);
        };
        let source = AssetSource::Remote {
            url: url.clone(),
            discriminator: Some(combined_discriminator(
                discriminator.as_deref(),
                identity.as_bytes(),
            )),
        };
        DefaultLayout.root(&source)
    }
}

fn combined_discriminator(discriminator: Option<&str>, identity: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(consts::DISCRIMINATOR_DOMAIN);
    if let Some(discriminator) = discriminator {
        hasher.update([1]);
        hash_field(&mut hasher, discriminator.as_bytes());
    } else {
        hasher.update([0]);
    }
    hash_field(&mut hasher, identity);
    finish_hash(hasher)
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hash_len(hasher, value.len());
    hasher.update(value);
}

fn hash_len(hasher: &mut Sha256, value: usize) {
    hasher.update(u64::try_from(value).unwrap_or(u64::MAX).to_be_bytes());
}

fn finish_hash(hasher: Sha256) -> String {
    let hash = hasher.finalize();
    hex::encode(&hash[..consts::HASH_BYTES])
}
