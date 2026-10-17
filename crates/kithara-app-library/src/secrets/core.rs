use std::{error, path::Path};

use keyring_core::{
    Entry, Error,
    api::{CredentialStore, CredentialStoreApi},
};
use kithara_platform::sync::Arc;

mod consts {
    pub(super) const APPLICATION: &str = "kithara";
}

/// Credentials a plugin keeps across runs.
#[derive(Clone)]
pub struct Secrets {
    store: Result<Arc<CredentialStore>, SecretError>,
}

impl Secrets {
    /// The overlay section that holds the secrets of a build without
    /// `keystore`.
    pub const SECTION: &str = "secrets";

    /// The application's secret store: the [`SECTION`](Self::SECTION) of the
    /// configuration overlay at `overlay`, or the operating system's store
    /// under `keystore`; on a host without one every call returns
    /// [`SecretError::Unsupported`].
    #[must_use]
    pub fn native(overlay: Option<&Path>) -> Self {
        cfg_select! {
            any(feature = "keystore", target_arch = "wasm32") => {
                let _ = overlay;
                cfg_select! {
                    all(feature = "keystore", target_os = "macos") => {
                        Self::new(apple_native_keyring_store::keychain::Store::new())
                    }
                    all(feature = "keystore", target_os = "windows") => {
                        Self::new(windows_native_keyring_store::Store::new())
                    }
                    all(
                        feature = "keystore",
                        unix,
                        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
                    ) => Self::new(dbus_secret_service_keyring_store::Store::new()),
                    _ => Self {
                        store: Err(SecretError::Unsupported),
                    },
                }
            }
            _ => overlay.map_or(
                Self {
                    store: Err(SecretError::Unsupported),
                },
                |path| Self::new(Ok(super::file::Store::new(path))),
            ),
        }
    }

    /// Keeps the store `opened` produced; a failure to open it is the error of
    /// every call.
    #[must_use]
    pub fn new<S>(opened: keyring_core::Result<Arc<S>>) -> Self
    where
        S: CredentialStoreApi + Send + Sync + 'static,
    {
        Self {
            store: opened
                .map(|store| -> Arc<CredentialStore> { store })
                .map_err(SecretError::from),
        }
    }

    /// The value stored under `key`, if any.
    ///
    /// # Errors
    /// Returns [`SecretError`] when the store cannot be read.
    pub fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        match self.entry(key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Stores `value` under `key`, replacing a previous value.
    ///
    /// # Errors
    /// Returns [`SecretError`] when the store refuses the write.
    pub fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        Ok(self.entry(key)?.set_password(value)?)
    }

    /// Removes the value under `key`; a missing value is already removed.
    ///
    /// # Errors
    /// Returns [`SecretError`] when the store refuses the removal.
    pub fn delete(&self, key: &str) -> Result<(), SecretError> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn entry(&self, key: &str) -> Result<Entry, SecretError> {
        let store = self.store.as_ref().map_err(SecretError::clone)?;
        Ok(store.build(consts::APPLICATION, key, None)?)
    }
}

/// A secret store failure.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SecretError {
    #[error("the secret store denied access")]
    Denied(#[source] Arc<dyn error::Error + Send + Sync>),
    #[error("the secret store failed")]
    Platform(#[source] Arc<dyn error::Error + Send + Sync>),
    #[error("this host has no secret store")]
    Unsupported,
}

impl From<Error> for SecretError {
    fn from(error: Error) -> Self {
        match error {
            Error::NoStorageAccess(source) => Self::Denied(source.into()),
            Error::PlatformFailure(source) | Error::BadDataFormat(_, source) => {
                Self::Platform(source.into())
            }
            Error::BadEncoding(_) => Self::Platform(Arc::new(Error::BadEncoding(Vec::new()))),
            other => Self::Platform(Arc::new(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use keyring_core::sample::Store;
    use kithara_test_utils::kithara;

    use super::*;

    /// A store that failed to open, or a host without one, fails every call
    /// with that error.
    #[kithara::test]
    fn a_missing_store_fails_every_call_with_its_error() {
        let refused = Secrets::new(Err::<Arc<Store>, _>(Error::NoStorageAccess(Box::new(
            std::io::Error::other("synthetic keychain refusal"),
        ))));
        let unsupported = Secrets {
            store: Err(SecretError::Unsupported),
        };

        let error = refused.get("probe").expect_err("the store did not open");
        let chain: Vec<String> =
            std::iter::successors(Some(&error as &dyn error::Error), |error| error.source())
                .map(ToString::to_string)
                .collect();
        assert_eq!(
            chain,
            [
                "the secret store denied access",
                "synthetic keychain refusal"
            ]
        );
        for (secrets, expected) in [
            (refused, "the secret store denied access"),
            (unsupported, "this host has no secret store"),
        ] {
            let errors = [
                secrets.get("probe").map(drop),
                secrets.set("probe", "value"),
                secrets.delete("probe"),
            ]
            .map(|call| call.expect_err("no store answers").to_string());
            assert_eq!(errors, [expected; 3]);
        }
    }

    #[kithara::test]
    fn an_undecodable_value_leaves_its_bytes_out_of_the_error() {
        let store = Store::new().expect("the sample store opens");
        let secrets = Secrets::new(Ok(Arc::clone(&store)));
        store
            .build(consts::APPLICATION, "probe", None)
            .and_then(|entry| entry.set_secret(&[0xff, 0xfe, 0xfd]))
            .expect("the store writes raw bytes");

        let error = secrets.get("probe").expect_err("the bytes are not UTF-8");

        let shown = format!("{error:?} {error}");
        assert!(!shown.contains("255"), "{shown}");
    }
}
