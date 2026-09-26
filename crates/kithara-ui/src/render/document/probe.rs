//! A context for a test that measures what one reader answers rather than
//! what a document is shaped like.

use super::{Clock, Ctx};
use crate::{compile::CompiledUi, render::Reads};

/// A context over a document that says nothing.
pub(crate) fn probe(reads: &dyn Reads) -> Ctx<'_, '_> {
    use std::sync::LazyLock;

    use crate::{
        builtin,
        compile::compile,
        ids::EndpointId,
        registry::{EndpointCategory, EndpointDesc, EndpointRegistry},
        source::UiConfig,
        view,
    };

    struct Nothing;

    impl EndpointRegistry for Nothing {
        fn endpoint(&self, _category: EndpointCategory, _id: &EndpointId) -> Option<&EndpointDesc> {
            None
        }
    }

    static EMPTY: LazyLock<CompiledUi> = LazyLock::new(|| {
        let mut resolver = builtin::resolver();
        resolver.insert(
            "probe.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "probe",
                root: Module(instance: "page", source: "modules/probe.kmodule.ron"))"#,
        );
        resolver.insert(
            "modules/probe.kmodule.ron",
            r#"(schema: "kithara.module", version: 1, id: "probe", root: Slot(id: "probe", default: []))"#,
        );
        compile(
            "probe.klayout.ron",
            &resolver,
            &Nothing,
            builtin::skin_doc(),
            builtin::text_doc(),
            &UiConfig::default(),
            &view::EMPTY,
        )
        .unwrap_or_else(|error| panic!("the probe document must compile: {error}"))
    });

    Ctx::new(
        &EMPTY,
        reads,
        &view::EMPTY,
        builtin::skin_doc(),
        Clock::default(),
    )
}
