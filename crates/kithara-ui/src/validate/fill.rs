use std::collections::{BTreeMap, BTreeSet};

use super::check_fill_key;
use crate::{
    error::UiDocError,
    ids::SourceUri,
    module::ModuleDoc,
    source::{FillDocument, SourceResolver},
};

pub(crate) fn check_fill_set(resolver: &dyn SourceResolver) -> Result<(), UiDocError> {
    let mut keys: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut origins: BTreeMap<&SourceUri, &ModuleDoc> = BTreeMap::new();
    for fill in resolver.fills() {
        check_fill_key(&fill.address, &fill.key)?;
        if !keys.entry(&fill.address).or_default().insert(&fill.key) {
            return Err(UiDocError::FillKey {
                address: fill.address.clone(),
                key: fill.key.clone(),
                reason: "is taken by another fill of the collection",
            });
        }
        let FillDocument::Parsed { document, origin } = &fill.document else {
            continue;
        };
        let refuse = |reason| UiDocError::FillOrigin {
            reason,
            origin: origin.clone(),
            address: fill.address.clone(),
        };
        match resolver.module(None, &origin.0) {
            Err(UiDocError::NotFound { .. }) => {}
            Ok(_) => return Err(refuse("names a document the package holds")),
            Err(error) => return Err(error),
        }
        let first = origins.entry(origin).or_insert(document);
        if **first != **document {
            return Err(refuse("names the origin of another parsed document"));
        }
    }
    Ok(())
}
