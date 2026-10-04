pub use bon;

use crate::Nested;

/// The nested live configuration `get` reads out of `owner`'s configuration.
pub fn nested<R, G>(owner: R, get: G) -> Nested<R, G> {
    Nested { get, owner }
}
