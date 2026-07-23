mod common;

use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    compile::{CompiledNode, compile},
    expand::{ControlSpec, ExpandedNode},
    ids::SourceUri,
    module::parse_module,
    skin::ColorRole,
    source::{MemResolver, UiConfig},
};

fn resolver(module: &str) -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "swatch.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "swatch",
            root: Module(instance: "swatch", source: "swatch.kmodule.ron"))"#,
    );
    resolver.insert("swatch.kmodule.ron", module);
    resolver
}

#[kithara::test]
fn swatch_compiles_without_bindings() {
    let resolver = resolver(
        r#"(schema: "kithara.module", version: 1, id: "swatch",
            root: Swatch(id: "accent", role: Accent, label: "GOLD ACTIVE"))"#,
    );

    let ui = compile(
        "swatch.klayout.ron",
        &resolver,
        &common::TestRegistry::default(),
        builtin::skin_doc(),
        &UiConfig::default(),
    )
    .unwrap();
    let CompiledNode::Module { root, .. } = &ui.root else {
        panic!("expected module root");
    };
    let ExpandedNode::Control {
        spec: ControlSpec::Swatch { role, label },
        read,
        write,
        ..
    } = &**root
    else {
        panic!("expected swatch control");
    };

    assert_eq!(*role, ColorRole::Accent);
    assert_eq!(ui.resolve(*label), "GOLD ACTIVE");
    assert_eq!(read, &None);
    assert_eq!(write, &None);
}

#[kithara::test]
fn swatch_rejects_binding_fields() {
    let origin = SourceUri("swatch.kmodule.ron".to_owned());
    let module = r#"(schema: "kithara.module", version: 1, id: "swatch",
        root: Swatch(
            id: "accent",
            role: Accent,
            label: "GOLD",
            read: Model(id: "palette.accent"),
        ))"#;

    assert!(parse_module(module, &origin).is_err());
}
