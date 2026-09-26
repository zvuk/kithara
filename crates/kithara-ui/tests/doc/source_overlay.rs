use kithara_test_utils::kithara;
use kithara_ui::{
    error::UiDocError,
    source::{MemResolver, OverlayResolver, SourceResolver},
};

fn layered() -> OverlayResolver<MemResolver, MemResolver> {
    let mut over = MemResolver::default();
    over.insert("player.klayout.ron", "(id: \"mine\")");
    over.insert_bytes("logo.png", &[9, 9]);
    let mut under = MemResolver::default();
    under.insert("player.klayout.ron", "(id: \"shipped\")");
    under.insert("deck.kmodule.ron", "(id: \"deck\")");
    under.insert_bytes("logo.png", &[1, 1]);
    under.insert_bytes("spinner.png", &[2, 2]);
    OverlayResolver::new(over, under)
}

#[kithara::test]
fn the_upper_layer_answers_a_name_both_layers_hold() {
    let resolver = layered();

    let loaded = resolver.load(None, "player.klayout.ron").unwrap();

    assert_eq!(loaded.text, "(id: \"mine\")");
}

#[kithara::test]
fn a_name_only_the_lower_layer_holds_is_answered_from_below() {
    let resolver = layered();

    let loaded = resolver.load(None, "deck.kmodule.ron").unwrap();

    assert_eq!(loaded.text, "(id: \"deck\")");
}

#[kithara::test]
fn a_name_neither_layer_holds_is_not_found() {
    let resolver = layered();

    let error = resolver.load(None, "nowhere.kmodule.ron").unwrap_err();

    assert!(matches!(error, UiDocError::NotFound { .. }));
}

/// A layer that refuses is not a layer that is missing something. These
/// stand on real directories because the refusals they check for cannot be
/// made in memory.
#[cfg(not(target_arch = "wasm32"))]
mod on_disk {
    use std::fs;

    use kithara_test_utils::kithara;
    use kithara_ui::source::FileResolver;
    use tempfile::TempDir;

    use super::*;

    fn shipped() -> (TempDir, FileResolver) {
        let root = TempDir::new().unwrap();
        fs::write(root.path().join("player.klayout.ron"), "(id: \"shipped\")").unwrap();
        let resolver = FileResolver::new(root.path()).unwrap();
        (root, resolver)
    }

    #[kithara::test]
    fn a_layer_led_out_of_its_root_refuses_rather_than_falling_through() {
        let (_under_root, under) = shipped();
        let over_root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("secret"), "not yours").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret"),
            over_root.path().join("player.klayout.ron"),
        )
        .unwrap();
        let over = FileResolver::new(over_root.path()).unwrap();
        let resolver = OverlayResolver::new(over, under);

        let error = resolver.load(None, "player.klayout.ron").unwrap_err();

        assert!(matches!(error, UiDocError::RootEscape { .. }));
    }

    #[kithara::test]
    fn a_layer_that_will_not_open_refuses_rather_than_falling_through() {
        let (_under_root, under) = shipped();
        let over_root = TempDir::new().unwrap();
        fs::create_dir(over_root.path().join("player.klayout.ron")).unwrap();
        let over = FileResolver::new(over_root.path()).unwrap();
        let resolver = OverlayResolver::new(over, under);

        let error = resolver.load(None, "player.klayout.ron").unwrap_err();

        assert!(matches!(error, UiDocError::Unreadable { .. }));
    }

    /// Each layer keeps what it read, and reading through the overlay is
    /// still reading through that layer.
    #[kithara::test]
    fn what_a_layer_kept_still_answers_through_the_overlay() {
        let (under_root, under) = shipped();
        let over = FileResolver::new(TempDir::new().unwrap().path()).unwrap();
        let resolver = OverlayResolver::new(over, under);
        resolver.load(None, "player.klayout.ron").unwrap();
        fs::remove_file(under_root.path().join("player.klayout.ron")).unwrap();

        let again = resolver.load(None, "player.klayout.ron").unwrap();

        assert_eq!(again.text, "(id: \"shipped\")");
    }
}

#[kithara::test]
fn the_upper_layer_answers_bytes_both_layers_hold() {
    let resolver = layered();

    let loaded = resolver.bytes(None, "logo.png").unwrap();

    assert_eq!(&*loaded.bytes, &[9, 9]);
}

#[kithara::test]
fn bytes_only_the_lower_layer_holds_are_answered_from_below() {
    let resolver = layered();

    let loaded = resolver.bytes(None, "spinner.png").unwrap();

    assert_eq!(&*loaded.bytes, &[2, 2]);
}
