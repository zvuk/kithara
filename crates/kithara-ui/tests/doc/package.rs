use kithara_test_utils::kithara;
use kithara_ui::{
    error::UiDocError,
    ids::{ScreenRole, SourceUri},
    package::load_package,
    source::{FillDocument, Limits, MemResolver},
};

const MANIFEST: &str = r#"(
        schema: "kithara.package",
        version: 1,
        id: "kithara-default",
        contract: 1,
        screens: {
            "player": "player.klayout.ron",
            "player-single": "player-single.klayout.ron",
        },
    )"#;

fn layout(id: &str) -> String {
    format!(r#"(schema: "kithara.layout", version: 1, id: "{id}", root: ())"#)
}

fn holding(manifest: &str) -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert("package.kpackage.ron", manifest);
    resolver.insert("player.klayout.ron", &layout("player"));
    resolver.insert("player-single.klayout.ron", &layout("player-single"));
    resolver
}

#[kithara::test]
fn a_package_names_the_file_behind_a_role() {
    let resolver = holding(MANIFEST);
    let package = load_package(&resolver, "package.kpackage.ron", &Limits::default()).unwrap();

    assert_eq!(
        package
            .screen(&resolver, &ScreenRole("player".into()))
            .unwrap(),
        "player.klayout.ron"
    );
}

#[kithara::test]
fn a_role_the_package_does_not_answer_is_refused_by_name() {
    let resolver = holding(MANIFEST);
    let package = load_package(&resolver, "package.kpackage.ron", &Limits::default()).unwrap();

    let error = package
        .screen(&resolver, &ScreenRole("mixer".into()))
        .unwrap_err();

    assert!(matches!(
        error,
        UiDocError::MissingRole { role, .. } if role == "mixer"
    ));
}

#[kithara::test]
fn a_package_inherits_nothing_unless_it_says_so() {
    let package = load_package(
        &holding(MANIFEST),
        "package.kpackage.ron",
        &Limits::default(),
    )
    .unwrap();

    assert!(!package.inherits);
}

#[kithara::test]
fn a_package_that_says_so_inherits() {
    let manifest = MANIFEST.replace("contract: 1,", "contract: 1, inherits: true,");

    let package = load_package(
        &holding(&manifest),
        "package.kpackage.ron",
        &Limits::default(),
    )
    .unwrap();

    assert!(package.inherits);
}

/// The contract is checked while the documents are still unread, so the
/// message names the mismatch and not whatever a stale document tripped on.
#[kithara::test]
fn a_package_written_for_another_contract_is_refused() {
    let manifest = MANIFEST.replace("contract: 1,", "contract: 7,");

    let error = load_package(
        &holding(&manifest),
        "package.kpackage.ron",
        &Limits::default(),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        UiDocError::ContractMismatch {
            needs: 7,
            offers: 1,
            ..
        }
    ));
}

/// The contract is the entrance check. A package that is wrong in both ways
/// must be told about the contract, because that is the one an author can
/// act on: the rest of the manifest was written for a different build.
#[kithara::test]
fn a_foreign_contract_is_reported_before_anything_else_about_the_package() {
    let manifest = MANIFEST.replace("contract: 1,", "contract: 7,").replace(
        r#"screens: {
            "player": "player.klayout.ron",
            "player-single": "player-single.klayout.ron",
        },"#,
        "screens: {},",
    );

    let error = load_package(
        &holding(&manifest),
        "package.kpackage.ron",
        &Limits::default(),
    )
    .unwrap_err();

    assert!(matches!(error, UiDocError::ContractMismatch { .. }));
}

#[kithara::test]
fn a_package_answering_for_nothing_is_refused() {
    let manifest = MANIFEST.replace(
        r#"screens: {
            "player": "player.klayout.ron",
            "player-single": "player-single.klayout.ron",
        },"#,
        "screens: {},",
    );

    let error = load_package(
        &holding(&manifest),
        "package.kpackage.ron",
        &Limits::default(),
    )
    .unwrap_err();

    assert!(matches!(error, UiDocError::EmptyPackage { .. }));
}

#[kithara::test]
fn a_role_with_no_file_behind_it_is_refused() {
    let manifest = MANIFEST.replace(r#""player": "player.klayout.ron","#, r#""player": "","#);

    let error = load_package(
        &holding(&manifest),
        "package.kpackage.ron",
        &Limits::default(),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        UiDocError::RoleWithoutFile { role, .. } if role == "player"
    ));
}

/// The manifest says which file stands for a role and the document says
/// which screen it is. A package whose two answers disagree has a typo, and
/// compiling on the manifest alone would draw the wrong screen in silence.
#[kithara::test]
fn a_file_naming_another_screen_is_refused_under_the_role_it_was_put_behind() {
    let manifest = MANIFEST.replace(
        r#""player": "player.klayout.ron","#,
        r#""player": "player-single.klayout.ron","#,
    );
    let resolver = holding(&manifest);
    let package = load_package(&resolver, "package.kpackage.ron", &Limits::default()).unwrap();

    let error = package
        .screen(&resolver, &ScreenRole("player".into()))
        .unwrap_err();

    assert!(matches!(
        error,
        UiDocError::RoleMismatch { found, .. } if found == "player-single"
    ));
}

#[kithara::test]
fn a_role_whose_file_is_not_there_is_not_found() {
    let manifest = MANIFEST.replace(
        r#""player": "player.klayout.ron","#,
        r#""player": "gone.klayout.ron","#,
    );
    let resolver = holding(&manifest);
    let package = load_package(&resolver, "package.kpackage.ron", &Limits::default()).unwrap();

    let error = package
        .screen(&resolver, &ScreenRole("player".into()))
        .unwrap_err();

    assert!(matches!(error, UiDocError::NotFound { .. }));
}

#[kithara::test]
fn a_manifest_that_is_another_kind_of_document_is_refused() {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "package.kpackage.ron",
        r#"(schema: "kithara.layout", version: 1, id: "player", root: ())"#,
    );

    let error = load_package(&resolver, "package.kpackage.ron", &Limits::default()).unwrap_err();

    assert!(matches!(
        error,
        UiDocError::WrongDocKind {
            expected: "package",
            ..
        }
    ));
}

#[kithara::test]
fn a_manifest_the_resolver_does_not_hold_is_not_found() {
    let error = load_package(
        &MemResolver::default(),
        "package.kpackage.ron",
        &Limits::default(),
    )
    .unwrap_err();

    assert!(matches!(error, UiDocError::NotFound { .. }));
}

fn filled(addresses: &[&str]) -> MemResolver {
    let mut resolver = MemResolver::default();
    resolver.insert(
        "package.kpackage.ron",
        r#"(schema: "kithara.package", version: 1, id: "filled", contract: 1,
            screens: { "page": "page.klayout.ron" })"#,
    );
    resolver.insert(
        "page.klayout.ron",
        r#"(schema: "kithara.layout", version: 1, id: "page",
            root: Module(instance: "demo", source: "rack.kmodule.ron", size: (w: Fill, h: Fill)))"#,
    );
    resolver.insert(
        "rack.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "rack",
            root: Slot(id: "items", from: "items", each: Include(source: "item.kmodule.ron")))"#,
    );
    resolver.insert(
        "item.kmodule.ron",
        r#"(schema: "kithara.module", version: 1, id: "item", parameters: ["key", "source"],
            root: Slot(id: "content"))"#,
    );
    let fill = r#"(schema: "kithara.module", version: 1, id: "fill", parameters: ["key", "source"],
        root: Spacer(id: "face"))"#;
    for address in addresses {
        let origin = SourceUri("fill.kmodule.ron".to_owned());
        let document = FillDocument::parse(fill, origin).expect("the fill parses");
        resolver.fill(address, "plugin", document);
    }
    resolver
}

#[kithara::test]
fn a_package_takes_fills_of_a_collection_its_screens_show() {
    let resolver = filled(&["rack/items"]);
    load_package(&resolver, "package.kpackage.ron", &Limits::default()).unwrap();
}

#[kithara::test]
fn a_fill_of_an_address_no_slot_shows_fails_loading_naming_it_and_its_plugin() {
    for address in ["rack/missing", "nowhere/items"] {
        let resolver = filled(&["rack/items", address]);
        let error =
            load_package(&resolver, "package.kpackage.ron", &Limits::default()).unwrap_err();

        assert!(
            matches!(&error, UiDocError::UnknownFill { address: named, key } if named == address && key == "plugin"),
            "{error}"
        );
    }
}
