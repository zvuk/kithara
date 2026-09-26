use std::collections::BTreeMap;

use kithara_test_utils::kithara;

use super::{
    super::{Budget, ControlSite, ControlSpec, ExpandedNode},
    expander::*,
};
use crate::{
    builtin,
    error::UiDocError,
    expand::{Binding, BindingKind},
    ids::{DocId, EndpointId, Interner, SourceUri, StrArena},
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry},
    resolve::{ModuleSet, load_module_graph},
    shader::ShaderCache,
    source::{Limits, MemResolver},
    text::TextDoc,
};

struct EmptyRegistry;

impl EndpointRegistry for EmptyRegistry {
    fn endpoint(&self, _category: EndpointCategory, _id: &EndpointId) -> Option<&EndpointDesc> {
        None
    }
}

fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn catalog(pairs: &[(&str, &str)]) -> TextDoc {
    TextDoc {
        id: DocId("test".to_owned()),
        schema: "kithara.text".to_owned(),
        version: 1,
        entries: pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
    }
}

fn deck_fixture() -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "deck.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "deck", parameters: ["deck"],
            root: Column(children: [
                Include(id: "transport", source: "deck/transport.kmodule.ron", with: { "deck": "$deck" }),
            ]))"#,
    );
    resolver.insert(
        "deck/transport.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "transport", parameters: ["deck"],
            root: Row(children: [
                Button(id: "play", label: "PLAY",
                    write: Command(id: "deck.transport.toggle_play", with: { "deck": "$deck" })),
            ]))"#,
    );
    resolver
}

fn expand(
    set: &ModuleSet,
    uri: &SourceUri,
    args: &BTreeMap<String, String>,
) -> Result<(ExpandedNode, StrArena), UiDocError> {
    expand_with_text(set, uri, args, builtin::text_doc())
}

fn expand_with_text(
    set: &ModuleSet,
    uri: &SourceUri,
    args: &BTreeMap<String, String>,
    text: &TextDoc,
) -> Result<(ExpandedNode, StrArena), UiDocError> {
    let mut budget = Budget::new(Limits::default().max_nodes);
    let mut interner = Interner::new(64 * 1024);
    let mut shaders = ShaderCache::default();
    let mut visitor = |_: ControlSite<'_>, _: &SourceUri| Ok(());
    let module = Expander::new(
        Limits::default().max_depth,
        &mut budget,
        &mut interner,
        &EmptyRegistry,
        &mut shaders,
        text,
        &mut visitor,
    )
    .expand_module(set, uri, args, "")?;
    Ok((module.root, interner.finish()))
}

fn expand_with_depth(
    set: &ModuleSet,
    uri: &SourceUri,
    max_depth: usize,
) -> Result<(ExpandedNode, StrArena), UiDocError> {
    let mut budget = Budget::new(Limits::default().max_nodes);
    let mut interner = Interner::new(64 * 1024);
    let mut shaders = ShaderCache::default();
    let mut visitor = |_: ControlSite<'_>, _: &SourceUri| Ok(());
    let module = Expander::new(
        max_depth,
        &mut budget,
        &mut interner,
        &EmptyRegistry,
        &mut shaders,
        builtin::text_doc(),
        &mut visitor,
    )
    .expand_module(set, uri, &BTreeMap::new(), "")?;
    Ok((module.root, interner.finish()))
}

fn depth_fixture(reverse: bool) -> MemResolver {
    let shallow = r#"Include(id: "shallow", source: "c.kmodule.ron")"#;
    let deep = r#"Include(id: "deep", source: "b.kmodule.ron")"#;
    let children = if reverse {
        format!("{deep}, {shallow}")
    } else {
        format!("{shallow}, {deep}")
    };
    let mut resolver = MemResolver::default();
    resolver.insert(
        "a.kmodule.ron",
        &format!(
            r#"(schema: "kithara.module", version: 1, id: "a",
                root: Row(children: [{children}]))"#
        ),
    );
    resolver.insert(
        "b.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "b",
            root: Include(id: "c", source: "c.kmodule.ron"))"#,
    );
    resolver.insert(
        "c.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "c",
            root: Include(id: "d", source: "d.kmodule.ron"))"#,
    );
    resolver.insert(
        "d.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "d",
            root: Text(id: "leaf"))"#,
    );
    resolver
}

fn posed_fixture(inner: &str) -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "posed.kmodule.ron",
        &format!(
            r#"(schema: "kithara.module", version: 1, id: "posed",
                root: Object(id: "outer", transform: (position: (10.0, 0.0)),
                    child: {inner}))"#
        ),
    );
    resolver
}

fn expand_posed(inner: &str) -> ExpandedNode {
    let resolver = posed_fixture(inner);
    let (uri, set) =
        load_module_graph(&resolver, None, "posed.kmodule.ron", &Limits::default()).unwrap();
    expand(&set, &uri, &BTreeMap::new()).unwrap().0
}

/// The pose survives expansion instead of being folded away, because a
/// `phase` endpoint can move it between one frame and the next and
/// expansion runs once.
#[kithara::test]
fn an_object_keeps_its_pose_through_expansion() {
    let ExpandedNode::Object { pose, .. } = expand_posed(r#"Text(id: "leaf")"#) else {
        panic!("the fixture root is one object");
    };

    assert_eq!(pose.position, (10.0, 0.0));
}

#[kithara::test]
fn an_object_with_no_track_carries_none() {
    let ExpandedNode::Object { to, phase, .. } = expand_posed(r#"Text(id: "leaf")"#) else {
        panic!("the fixture root is one object");
    };

    assert_eq!((to, phase), (None, None));
}

#[kithara::test]
fn objects_nest_the_way_the_document_wrote_them() {
    let root = expand_posed(
        r#"Object(id: "inner", transform: (position: (0.0, 4.0)), child: Text(id: "leaf"))"#,
    );

    let ExpandedNode::Object { child, .. } = root else {
        panic!("the fixture root is one object");
    };
    assert!(matches!(*child, ExpandedNode::Object { .. }));
}

#[kithara::test]
fn an_object_that_travels_keeps_both_ends_of_its_track() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "track.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "track",
            root: Object(id: "spin", to: (rotation: 360.0),
                phase: Model(id: "gallery.spin"), child: Text(id: "leaf")))"#,
    );
    let (uri, set) =
        load_module_graph(&resolver, None, "track.kmodule.ron", &Limits::default()).unwrap();

    let (root, _) = expand(&set, &uri, &BTreeMap::new()).unwrap();

    let ExpandedNode::Object { to, phase, .. } = root else {
        panic!("the fixture root is one object");
    };
    assert_eq!(to.map(|to| to.rotation), Some(360.0));
    assert!(phase.is_some());
}

#[kithara::test]
fn nested_include_receives_substituted_args() {
    let resolver = deck_fixture();
    let (uri, set) =
        load_module_graph(&resolver, None, "deck.kmodule.ron", &Limits::default()).unwrap();
    let (root, arena) = expand(&set, &uri, &args(&[("deck", "b")])).unwrap();

    let ExpandedNode::Column { children, .. } = root else {
        panic!("expected column");
    };
    let ExpandedNode::Row { children, .. } = &children[0] else {
        panic!("expected row");
    };
    let ExpandedNode::Control { path, write, .. } = &children[0] else {
        panic!("expected control");
    };
    assert_eq!(arena.resolve(*path), "transport/play");
    let Some(Binding {
        kind: BindingKind::Command,
        with,
        ..
    }) = write
    else {
        panic!("expected command");
    };
    let deck = with
        .iter()
        .find(|(key, _)| arena.resolve(**key) == "deck")
        .map(|(_, value)| arena.resolve(*value));
    assert_eq!(deck, Some("b"));
}

#[kithara::test]
fn missing_argument_is_unresolved_param() {
    let resolver = deck_fixture();
    let (uri, set) =
        load_module_graph(&resolver, None, "deck.kmodule.ron", &Limits::default()).unwrap();
    let error = expand(&set, &uri, &BTreeMap::new()).unwrap_err();
    assert!(matches!(
        error,
        UiDocError::UnresolvedParam { name, .. } if name == "deck"
    ));
}

#[kithara::test]
fn undeclared_argument_is_rejected() {
    let resolver = deck_fixture();
    let (uri, set) =
        load_module_graph(&resolver, None, "deck.kmodule.ron", &Limits::default()).unwrap();
    let error = expand(&set, &uri, &args(&[("deck", "a"), ("bogus", "1")])).unwrap_err();
    assert!(matches!(
        error,
        UiDocError::UnknownParam { name, .. } if name == "bogus"
    ));
}

#[kithara::test]
fn undeclared_include_argument_reports_instance_path() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "deck.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "deck", parameters: ["deck"],
            root: Include(id: "transport", source: "transport.kmodule.ron", with: {
                "deck": "$deck",
                "bogus": "1",
            }))"#,
    );
    resolver.insert(
        "transport.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "transport", parameters: ["deck"],
            root: Button(id: "play", label: "PLAY"))"#,
    );
    let (uri, set) =
        load_module_graph(&resolver, None, "deck.kmodule.ron", &Limits::default()).unwrap();
    let limits = Limits::default();
    let mut budget = Budget::new(limits.max_nodes);
    let mut interner = Interner::new(64 * 1024);
    let mut shaders = ShaderCache::default();
    let mut visitor = |_: ControlSite<'_>, _: &SourceUri| Ok(());

    let error = Expander::new(
        limits.max_depth,
        &mut budget,
        &mut interner,
        &EmptyRegistry,
        &mut shaders,
        builtin::text_doc(),
        &mut visitor,
    )
    .expand_module(&set, &uri, &args(&[("deck", "a")]), "deck-a")
    .unwrap_err();
    let message = error.to_string();
    assert!(matches!(
        error,
        UiDocError::UnknownParam { name, .. } if name == "bogus"
    ));
    assert!(message.contains("deck-a/transport"), "{message}");
}

#[kithara::test]
fn doubled_dollar_expands_to_literal_dollar() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "price.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "price",
            root: Chip(id: "price", label: "$$5.99"))"#,
    );
    let (uri, set) =
        load_module_graph(&resolver, None, "price.kmodule.ron", &Limits::default()).unwrap();

    let (root, arena) = expand(&set, &uri, &BTreeMap::new()).unwrap();
    let ExpandedNode::Control {
        spec: ControlSpec::Chip { label, .. },
        ..
    } = root
    else {
        panic!("expected control");
    };
    assert_eq!(arena.resolve(label), "$5.99");
}

#[kithara::test]
fn doubled_at_expands_to_literal_at() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "handle.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "handle",
            root: Chip(id: "handle", label: "@@user"))"#,
    );
    let (uri, set) =
        load_module_graph(&resolver, None, "handle.kmodule.ron", &Limits::default()).unwrap();

    let (root, arena) = expand(&set, &uri, &BTreeMap::new()).unwrap();
    let ExpandedNode::Control {
        spec: ControlSpec::Chip { label, .. },
        ..
    } = root
    else {
        panic!("expected control");
    };
    assert_eq!(arena.resolve(label), "@user");
}

#[kithara::test]
fn at_key_resolves_directly_to_its_catalog_value() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "greeting.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "greeting",
            root: Chip(id: "chip", label: "@menu.modules"))"#,
    );
    let (uri, set) =
        load_module_graph(&resolver, None, "greeting.kmodule.ron", &Limits::default()).unwrap();
    let text = catalog(&[("menu.modules", "Modules")]);

    let (root, arena) = expand_with_text(&set, &uri, &BTreeMap::new(), &text).unwrap();
    let ExpandedNode::Control {
        spec: ControlSpec::Chip { label, .. },
        ..
    } = root
    else {
        panic!("expected control");
    };
    assert_eq!(arena.resolve(label), "Modules");
}

#[kithara::test]
fn bare_at_sign_mid_string_is_not_a_marker() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "contact.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "contact",
            root: Chip(id: "chip", label: "user@example.com"))"#,
    );
    let (uri, set) =
        load_module_graph(&resolver, None, "contact.kmodule.ron", &Limits::default()).unwrap();

    let (root, arena) = expand(&set, &uri, &BTreeMap::new()).unwrap();
    let ExpandedNode::Control {
        spec: ControlSpec::Chip { label, .. },
        ..
    } = root
    else {
        panic!("expected control");
    };
    assert_eq!(arena.resolve(label), "user@example.com");
}

#[kithara::test]
fn unknown_at_key_is_a_compile_error() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "greeting.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "greeting",
            root: Chip(id: "chip", label: "@missing.key"))"#,
    );
    let (uri, set) =
        load_module_graph(&resolver, None, "greeting.kmodule.ron", &Limits::default()).unwrap();
    let text = catalog(&[]);

    let error = expand_with_text(&set, &uri, &BTreeMap::new(), &text).unwrap_err();
    assert!(matches!(
        error,
        UiDocError::UnknownTextKey { key, .. } if key == "missing.key"
    ));
}

#[kithara::test]
fn escaped_at_argument_survives_substitution_before_key_resolution() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "wrapper.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "wrapper",
            root: Include(id: "inner", source: "inner.kmodule.ron", with: { "label": "@@handle" }))"#,
    );
    resolver.insert(
        "inner.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "inner", parameters: ["label"],
            root: Chip(id: "chip", label: "$label"))"#,
    );
    let (uri, set) =
        load_module_graph(&resolver, None, "wrapper.kmodule.ron", &Limits::default()).unwrap();

    let (root, arena) = expand(&set, &uri, &BTreeMap::new()).unwrap();
    let ExpandedNode::Control {
        spec: ControlSpec::Chip { label, .. },
        ..
    } = root
    else {
        panic!("expected control");
    };
    assert_eq!(arena.resolve(label), "@handle");
}

#[kithara::test]
fn substituted_at_key_resolves_through_the_catalog() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "wrapper.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "wrapper",
            root: Include(id: "inner", source: "inner.kmodule.ron", with: { "label": "@menu.modules" }))"#,
    );
    resolver.insert(
        "inner.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "inner", parameters: ["label"],
            root: Chip(id: "chip", label: "$label"))"#,
    );
    let (uri, set) =
        load_module_graph(&resolver, None, "wrapper.kmodule.ron", &Limits::default()).unwrap();
    let text = catalog(&[("menu.modules", "Modules")]);

    let (root, arena) = expand_with_text(&set, &uri, &BTreeMap::new(), &text).unwrap();
    let ExpandedNode::Control {
        spec: ControlSpec::Chip { label, .. },
        ..
    } = root
    else {
        panic!("expected control");
    };
    assert_eq!(arena.resolve(label), "Modules");
}

#[kithara::test]
fn module_title_resolves_an_at_key_without_dollar_substitution() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "titled.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "titled", title: "@menu.modules",
            root: Chip(id: "chip", label: "PLAY"))"#,
    );
    let (uri, set) =
        load_module_graph(&resolver, None, "titled.kmodule.ron", &Limits::default()).unwrap();
    let text = catalog(&[("menu.modules", "Modules")]);
    let mut budget = Budget::new(Limits::default().max_nodes);
    let mut interner = Interner::new(64 * 1024);
    let mut shaders = ShaderCache::default();
    let mut visitor = |_: ControlSite<'_>, _: &SourceUri| Ok(());

    let module = Expander::new(
        Limits::default().max_depth,
        &mut budget,
        &mut interner,
        &EmptyRegistry,
        &mut shaders,
        &text,
        &mut visitor,
    )
    .expand_module(&set, &uri, &BTreeMap::new(), "")
    .unwrap();
    let arena = interner.finish();

    assert_eq!(arena.resolve(module.title.unwrap()), "Modules");
}

#[kithara::test]
fn expansion_depth_limit_is_independent_of_include_order() {
    for reverse in [false, true] {
        let resolver = depth_fixture(reverse);
        let (uri, set) =
            load_module_graph(&resolver, None, "a.kmodule.ron", &Limits::default()).unwrap();
        let error = expand_with_depth(&set, &uri, 2).unwrap_err();
        assert!(matches!(
            error,
            UiDocError::DepthExceeded {
                origin,
                depth: 3,
                max: 2,
            } if origin.0 == "d.kmodule.ron"
        ));
    }
}
