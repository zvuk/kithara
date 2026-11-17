//! Skin-owned artworks retain their declaring origins and prepared identity.

use std::cell::Cell;

use kithara_platform::sync::Arc;
use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    draw::DrawListBuilder,
    error::UiDocError,
    ids::SourceUri,
    lottie::emit,
    render::Skin,
    shaping::FontPolicy,
    skin::{SkinDoc, load_skin, parse_skin_over},
    source::{Fill, Limits, LoadedBytes, LoadedSource, MemResolver, SourceResolver},
};

const ARTWORK: &[u8] = include_bytes!("../fixtures/artwork.json");

struct Sources {
    inner: MemResolver,
    reads: Cell<usize>,
}

impl SourceResolver for Sources {
    delegate::delegate! {
        to self.inner {
            fn fills(&self) -> Vec<&Fill>;
            fn load(&self, base: Option<&SourceUri>, rel: &str) -> Result<LoadedSource, UiDocError>;
        }
    }

    fn bytes(&self, base: Option<&SourceUri>, rel: &str) -> Result<LoadedBytes, UiDocError> {
        if rel == "skin/picture.json" {
            self.reads.set(self.reads.get() + 1);
        }
        self.inner.bytes(base, rel)
    }
}

fn resolved(
    document: SkinDoc,
    origin: &SourceUri,
    sources: &dyn SourceResolver,
) -> Result<Skin, UiDocError> {
    Skin::resolve_with_font_policy(
        document,
        builtin::text_doc(),
        origin,
        sources,
        FontPolicy::Embedded,
    )
}

#[kithara::test]
fn inherited_equal_basenames_keep_distinct_origins_and_overlay_identity() {
    let mut resolver = builtin::resolver();
    resolver.insert(
        "base/skin.ron",
        &format!(
            r#"(schema: "kithara.skin", version: 1, id: "base",
        base: "../{}", artworks: {{"left": "picture.json"}})"#,
            builtin::DARK_SKIN_PATH
        ),
    );
    resolver.insert(
        "child/skin.ron",
        r#"(schema: "kithara.skin", version: 1, id: "child",
        base: "../base/skin.ron", artworks: {"right": "picture.json"})"#,
    );
    resolver.insert_bytes("base/picture.json", ARTWORK);
    resolver.insert_bytes("child/picture.json", ARTWORK);
    let document = load_skin(&resolver, "child/skin.ron", &Limits::default()).unwrap();
    assert_eq!(document.artworks["left"], "base/picture.json");
    assert_eq!(document.artworks["right"], "child/picture.json");
    let skin = resolved(document, &SourceUri("child/skin.ron".into()), &resolver).unwrap();
    assert!(!Arc::ptr_eq(
        skin.artwork("left").unwrap(),
        skin.artwork("right").unwrap()
    ));
    let overlay = parse_skin_over(
        skin.document(),
        r#"(schema: "kithara.skin", version: 1,
        id: "overlay", artworks: {"left": "replacement.json"})"#,
        &SourceUri("overlay/skin.ron".into()),
    )
    .unwrap();
    assert_eq!(overlay.artworks["left"], "overlay/replacement.json");
    assert_eq!(overlay.artworks["right"], "child/picture.json");
}

#[kithara::test]
fn repeated_frame_emission_reuses_the_admitted_resource_without_source_reads() {
    let mut resolver = builtin::resolver();
    resolver.insert_bytes("skin/picture.json", ARTWORK);
    let sources = Sources {
        inner: resolver,
        reads: Cell::new(0),
    };
    let origin = SourceUri("skin/skin.ron".into());
    let document = parse_skin_over(
        builtin::skin_doc(),
        r#"(schema: "kithara.skin", version: 1,
        id: "caller", artworks: {"caller": "picture.json"})"#,
        &origin,
    )
    .unwrap();
    let skin = resolved(document, &origin, &sources).unwrap();
    assert_eq!(sources.reads.get(), 1);
    let artwork = skin.artwork("caller").unwrap();
    let clone = skin.clone();
    assert!(Arc::ptr_eq(artwork, clone.artwork("caller").unwrap()));
    let frame = artwork.frame_at(0.25, 1.0);
    let draw = || {
        let mut list = DrawListBuilder::default();
        emit(artwork.composition(), frame, &mut list).unwrap();
        list.finish()
    };
    let expected = draw();
    assert!(!expected.commands().is_empty());
    for _ in 0..8 {
        assert_eq!(draw(), expected);
    }
    assert_eq!(sources.reads.get(), 1);
    assert!(skin.artwork("optional-unbound").is_none());
}

#[kithara::test]
fn artwork_sources_cannot_escape_the_root() {
    for source in ["../../outside.json", "/outside.json"] {
        let document = format!(
            r#"(schema: "kithara.skin", version: 1, id: "bad",
            artworks: {{"caller": "{source}"}})"#
        );
        assert!(matches!(
            parse_skin_over(
                builtin::skin_doc(),
                &document,
                &SourceUri("skin/skin.ron".into())
            ),
            Err(UiDocError::RootEscape { .. })
        ));
    }
}

#[kithara::test]
fn missing_artwork_retains_the_skin_origin_and_resolved_reference() {
    let origin = SourceUri("skin/skin.ron".into());
    let document = parse_skin_over(
        builtin::skin_doc(),
        r#"(schema: "kithara.skin", version: 1,
        id: "caller", artworks: {"caller": "missing.json"})"#,
        &origin,
    )
    .unwrap();
    let error = resolved(document, &origin, &builtin::resolver()).unwrap_err();
    assert!(
        matches!(&error, UiDocError::NotFound { origin: found, rel }
        if found == &origin && rel == "skin/missing.json"),
        "{error:?}"
    );
}
