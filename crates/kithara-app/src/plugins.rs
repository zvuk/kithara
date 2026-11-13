//! The library plugins this build can mount and their registration.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use kithara::platform::CancelToken;
#[cfg(not(target_arch = "wasm32"))]
use kithara_app_library::Cause;
use kithara_app_library::{Context, Environment, Factory, KeyAccess, RegisterError, Registration};
use serde_yaml_ng::Value;

pub use self::consts::FACTORIES;
use crate::document::Config;

mod consts {
    use kithara_app_library::Factory;

    /// Every library source this build can mount.
    pub const FACTORIES: &[Factory] = &[
        #[cfg(feature = "zvuk")]
        kithara_app_zvuk::Source::FACTORY,
    ];
}

/// Registers the plugins `document` configures over `environment`.
///
/// # Errors
/// Returns the first registration a plugin refuses, named by its id.
pub fn mount(
    document: &Config,
    environment: &Environment,
    shutdown: &CancelToken,
) -> Result<Vec<Registration>, RegisterError> {
    #[cfg(not(target_arch = "wasm32"))]
    let settings_root: Option<PathBuf> = {
        let Some(factory) = FACTORIES.iter().find(|factory| {
            document
                .sources()
                .get(factory.id)
                .is_some_and(|section| !section.is_null())
        }) else {
            return Ok(Vec::new());
        };
        Some(
            dirs_next::config_dir()
                .ok_or_else(|| RegisterError::new(factory.id, Cause::SettingsUnavailable))?
                .join("kithara"),
        )
    };
    #[cfg(target_arch = "wasm32")]
    let settings_root: Option<PathBuf> = None;
    configured(
        FACTORIES,
        document.sources(),
        environment,
        shutdown,
        settings_root.as_deref(),
    )
}

/// Registers each factory whose entry is present and non-null.
///
/// # Errors
/// Returns the first registration a plugin refuses, named by its id.
fn configured(
    factories: &[Factory],
    sections: &BTreeMap<String, Value>,
    environment: &Environment,
    shutdown: &CancelToken,
    settings_root: Option<&Path>,
) -> Result<Vec<Registration>, RegisterError> {
    factories
        .iter()
        .filter_map(|factory| {
            let section = sections
                .get(factory.id)
                .filter(|section| !section.is_null())?;
            Some(
                (factory.register)(
                    environment,
                    Context::new(shutdown.child(), section.clone()).with_settings_path(
                        settings_root.map(|path| path.join(format!("{}.json", factory.id))),
                    ),
                )
                .map_err(|cause| RegisterError::new(factory.id, cause)),
            )
        })
        .collect()
}

/// The key access the registered plugins grant.
#[must_use]
pub fn grants(registered: &[Registration]) -> Vec<KeyAccess> {
    registered
        .iter()
        .filter_map(Registration::granted)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use ::kithara::{
        net::{HttpClient, NetOptions},
        platform::tokio::runtime::Builder,
        ui::{error::UiDocError, ids::SourceUri},
    };
    use keyring_core::sample::Store;
    use kithara_app_library::{Cause, Secrets, SourcePage};
    use kithara_test_utils::{cancel_token, kithara};

    use super::*;
    use crate::pools::{self, PoolsSection};

    thread_local! {
        static REGISTERED: RefCell<Vec<CancelToken>> = const { RefCell::new(Vec::new()) };
    }

    fn register(_: &Environment, context: Context) -> Result<Registration, Cause> {
        context.section::<BTreeMap<String, String>>()?;
        REGISTERED.with_borrow_mut(|registered| registered.push(context.cancel()));
        let page = SourcePage {
            id: "probe",
            endpoints: Vec::new(),
            texts: Vec::new(),
        };
        Ok(Registration::new(page, |_| {
            Err(UiDocError::NotFound {
                origin: SourceUri("probe".to_owned()),
                rel: String::new(),
            })
        }))
    }

    #[cfg(feature = "zvuk")]
    #[kithara::test(native, flash(false))]
    fn the_stock_document_registers_zvuk_and_its_account(cancel_token: CancelToken) {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime builds");
        let pools = pools::build(&PoolsSection::default()).expect("valid app pool policy");
        let net = HttpClient::new(NetOptions::builder().build(), pools, cancel_token.child());
        let environment =
            Environment::new(runtime.handle().clone(), net, Secrets::new(Store::new()));
        let document = Config::load(None, None).expect("the stock document loads");
        let registered =
            mount(&document, &environment, &cancel_token).expect("stock sources register");
        assert_eq!(registered.len(), 1);
        let page = registered[0].page();
        assert_eq!(page.id, "zvuk");
        assert!(
            page.endpoints
                .iter()
                .any(|endpoint| endpoint.name == "account_connect")
        );
        assert!(registered[0].granted().is_some());
    }

    #[kithara::test]
    fn configured_entries_register_with_cancellations_of_their_own(cancel_token: CancelToken) {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime builds");
        let pools = pools::build(&PoolsSection::default()).expect("valid app pool policy");
        let net = HttpClient::new(NetOptions::builder().build(), pools, cancel_token.child());
        let entry = |yaml| serde_yaml_ng::from_str::<Value>(yaml).expect("entry parses");
        let sections = BTreeMap::from([
            ("first".to_owned(), entry("name: one")),
            ("second".to_owned(), entry("name: two")),
            ("null".to_owned(), Value::Null),
        ]);
        let mismatched = BTreeMap::from([("first".to_owned(), entry("one"))]);
        let probes = ["first", "second", "null", "absent"].map(|id| Factory { id, register });

        let environment =
            Environment::new(runtime.handle().clone(), net, Secrets::new(Store::new()));

        let registered = configured(&probes, &sections, &environment, &cancel_token, None)
            .expect("both entries match the schema");
        let refused = configured(&probes, &mismatched, &environment, &cancel_token, None);

        assert_eq!(registered.len(), 2);
        let refused = refused.err().map(|error| error.to_string());
        assert!(
            refused.is_some_and(|message| message.starts_with("sources.first: ")),
            "a mismatched entry is refused by its factory id"
        );
        let [first, second] = REGISTERED
            .take()
            .try_into()
            .expect("two sources registered");
        first.cancel();
        assert!(!second.is_cancelled() && !cancel_token.is_cancelled());
        cancel_token.cancel();
        assert!(second.is_cancelled());
    }
}
