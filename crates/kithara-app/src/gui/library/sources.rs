use std::collections::BTreeMap;

use kithara::{
    net::HttpClient,
    platform::{CancelToken, tokio::runtime::Handle},
};
use kithara_app_library::{Context, Environment, Factory, RegisterError, Registration};
use serde_yaml_ng::Value;

pub(in crate::gui) use self::consts::FACTORIES;

mod consts {
    use kithara_app_library::Factory;

    /// Every library source this build can mount.
    pub(in crate::gui) const FACTORIES: &[Factory] = &[
        #[cfg(feature = "zvuk")]
        kithara_app_zvuk::Source::FACTORY,
    ];
}

/// Registers sources with non-null configuration entries and child cancellation tokens.
pub(in crate::gui) fn configured(
    factories: &[Factory],
    sections: &BTreeMap<String, Value>,
    net: &HttpClient,
    runtime: &Handle,
    shutdown: &CancelToken,
) -> Result<Vec<Registration>, RegisterError> {
    let environment = Environment::new(runtime.clone(), net.clone());
    factories
        .iter()
        .filter_map(|factory| {
            let section = sections
                .get(factory.id)
                .filter(|section| !section.is_null())?;
            Some(
                (factory.register)(
                    &environment,
                    Context::new(shutdown.child(), section.clone()),
                )
                .map_err(|cause| RegisterError::new(factory.id, cause)),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use kithara_app_library::Cause;
    use kithara_test_utils::{cancel_token, kithara};

    use super::*;
    use crate::gui::{library::StartupSource, test_fixture};

    thread_local! {
        static REGISTERED: RefCell<Vec<CancelToken>> = const { RefCell::new(Vec::new()) };
    }

    fn register(_: &Environment, context: Context) -> Result<Registration, Cause> {
        context.section::<BTreeMap<String, String>>()?;
        REGISTERED.with_borrow_mut(|registered| registered.push(context.cancel()));
        Ok(StartupSource::registered(Vec::new()))
    }

    #[kithara::test]
    fn configured_entries_register_with_cancellations_of_their_own(cancel_token: CancelToken) {
        let runtime = test_fixture::runtime();
        let net = test_fixture::config().net;
        let entry = |yaml| serde_yaml_ng::from_str::<Value>(yaml).expect("entry parses");
        let sections = BTreeMap::from([
            ("first".to_owned(), entry("name: one")),
            ("second".to_owned(), entry("name: two")),
            ("null".to_owned(), Value::Null),
        ]);
        let mismatched = BTreeMap::from([("first".to_owned(), entry("one"))]);
        let probes = ["first", "second", "null", "absent"].map(|id| Factory { id, register });

        let registered = configured(&probes, &sections, &net, runtime.handle(), &cancel_token)
            .expect("both entries match the schema");
        let refused = configured(&probes, &mismatched, &net, runtime.handle(), &cancel_token);

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
