//! Required artwork references are checked after module argument substitution.

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledUi, compile},
    error::UiDocError,
    registry::MapEndpoints,
    skin::SkinDoc,
    source::{MemResolver, UiConfig},
    view,
};

fn compiled(artwork: &str, active: &str, skin: &SkinDoc) -> Result<CompiledUi, UiDocError> {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "layout.ron",
        &format!(
            r#"(schema: "kithara.layout", version: 1, id: "test",
            root: Module(instance: "demo", source: "panels/module.ron",
                with: {{"art": "{artwork}"}}))"#
        ),
    );
    resolver.insert(
        "panels/module.ron",
        &format!(
            r#"(schema: "kithara.module", version: 1, id: "test",
            parameters: ["art"],
            root: Lottie(id: "picture", artwork: "$art", seconds: 1.0,
                active_artwork: {active}))"#
        ),
    );
    compile(
        "layout.ron",
        &resolver,
        &MapEndpoints::default(),
        skin,
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
}

#[kithara::test]
#[case::primary("missing", "None", "missing")]
#[case::active("pulse", "Some(\"missing\")", "missing")]
#[case::empty("", "None", "")]
fn unknown_required_artwork_is_refused_with_its_declaring_origin(
    #[case] artwork: &str,
    #[case] active: &str,
    #[case] expected: &str,
) {
    let error = compiled(artwork, active, builtin::skin_doc()).expect_err("required artwork");
    assert!(
        matches!(&error, UiDocError::UnknownArtwork { origin, name, path }
            if origin.0 == "panels/module.ron" && name == expected && path == "demo/picture"),
        "{error:?}"
    );
}

#[kithara::test]
fn declared_artwork_and_optional_absent_active_artwork_compile() {
    let mut skin = builtin::skin_doc().clone();
    skin.artworks.insert("caller".into(), "caller.json".into());
    assert!(compiled("caller", "None", &skin).is_ok());
    assert!(compiled("pulse", "Some(\"spark\")", &skin).is_ok());
}

#[kithara::test]
fn an_included_modules_required_artwork_keeps_its_own_origin() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "layout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "nested",
        root: Module(instance: "demo", source: "panels/root.ron"))"#,
    );
    resolver.insert(
        "panels/root.ron",
        r#"(schema: "kithara.module", version: 1, id: "outer",
        root: Include(id: "inner", source: "../child/inner.ron", with: {"art": "missing"}))"#,
    );
    resolver.insert(
        "child/inner.ron",
        r#"(schema: "kithara.module", version: 1, id: "inner",
        parameters: ["art"], root: Lottie(id: "picture", artwork: "$art", seconds: 1.0))"#,
    );
    let error = compile(
        "layout.ron",
        &resolver,
        &MapEndpoints::default(),
        builtin::skin_doc(),
        builtin::text_doc(),
        &UiConfig::default(),
        &view::EMPTY,
    )
    .expect_err("required artwork");
    assert!(
        matches!(&error, UiDocError::UnknownArtwork { origin, name, .. }
        if origin.0 == "child/inner.ron" && name == "missing"),
        "{error:?}"
    );
}
