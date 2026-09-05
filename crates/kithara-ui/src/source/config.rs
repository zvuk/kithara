use std::collections::BTreeSet;

use bon::Builder;
use kithara_macros::Patch;

#[cfg(any(feature = "render", feature = "vello"))]
use crate::draw::DrawBuffers;

#[derive(Builder, Clone, Debug, PartialEq, Patch)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct Limits {
    #[builder(default = 256 * 1024)]
    pub max_bytes: usize,
    #[builder(default = 8)]
    pub max_depth: usize,
    #[builder(default = 10_000)]
    pub max_nodes: usize,
}

/// Memory retained by the draw pools between frames.
#[derive(Builder, Clone, Copy, Debug, PartialEq, Eq, Patch)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct DrawPoolLimits {
    /// Hard byte limit shared by every draw buffer kind.
    #[builder(default = 64 * 1024 * 1024)]
    pub max_bytes: usize,
    /// Maximum reusable buffers kept by each pool. Zero is treated as one.
    #[builder(default = 64)]
    pub max_buffers: usize,
    /// Command slots retained by one returned draw-list buffer.
    #[builder(default = 512)]
    pub command_capacity: usize,
    /// Vector verbs retained by one returned path buffer.
    #[builder(default = 128)]
    pub path_capacity: usize,
    /// UTF-8 bytes retained by one returned text buffer.
    #[builder(default = 128)]
    pub text_capacity: usize,
}

impl Default for DrawPoolLimits {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Compiled screens the retained host keeps while a document turns between
/// its pages. The immediate host compiles both deck layouts up front and
/// keeps no screen cache of its own, so it never reads this.
///
/// Measured against the gallery's own pages: seven of them cost more than two
/// milliseconds to compile, which is a hitch on every return visit, and eight
/// covers every page a document offers today without a package of hundreds
/// growing the cache without bound.
pub const SCREEN_CACHE: usize = 8;

/// Canonical compile configuration and its resource limits.
#[derive(Builder, Clone, Debug, PartialEq, Patch)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct UiConfig {
    /// The extension kinds the application registers with its hosts.
    ///
    /// A document naming a `Custom` kind absent from this set is refused while
    /// it compiles, so no host is ever handed an extension it cannot mount.
    ///
    /// Not a document key: registering a kind takes code
    /// (`CustomKinds::names`, built from what the host actually mounted), so
    /// a document naming one here would type but then be refused by every
    /// build that registers no matching kind -- a document-shaped failure for
    /// a code-owned fact.
    #[builder(default)]
    #[patch(skip)]
    pub custom_kinds: BTreeSet<String>,
    #[builder(default)]
    #[patch(nested)]
    pub limits: Limits,
    #[builder(default = 64 * 1024)]
    pub max_arena_bytes: usize,
    /// Compiled screens a host keeps while a document turns between its pages.
    ///
    /// A page is compiled when it is first shown, and kept so that turning
    /// back to it costs nothing. The screen being shown counts as one of
    /// these, so a depth of one keeps no page a host has left.
    ///
    /// Read only by the retained host, through `Screens::new` in
    /// `app::embed::Ui::new`. The immediate host compiles both deck layouts
    /// eagerly at startup and keeps no screen cache, so it never consults
    /// this field.
    #[builder(default = SCREEN_CACHE)]
    pub screen_cache: usize,
    /// The pools every document compiled against this configuration draws
    /// from.
    ///
    /// Shared on purpose. A host compiles one screen per layout and compiles
    /// them all again whenever the skin changes; a pool family per compiled
    /// document would keep as many sets of retained buffers as there are
    /// pages, and throw every one of them away at each redress. One family,
    /// cloned into each compiled document, is what makes a retained buffer
    /// retained. Build the configuration once and compile every screen against
    /// it; the default builds a family of its own, which is one host drawing
    /// one page.
    ///
    /// Not a document key: it is a built value assembled from
    /// [`DrawPoolLimits`], which the document names instead -- see
    /// `Config::ui` in `kithara-app`.
    #[cfg(any(feature = "render", feature = "vello"))]
    #[builder(default)]
    #[patch(skip)]
    pub draw_buffers: DrawBuffers,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod document_tests {
    use kithara_test_utils::kithara;

    use super::{DrawPoolLimits, DrawPoolLimitsPatch, LimitsPatch, UiConfig, UiConfigPatch};
    #[cfg(any(feature = "render", feature = "vello"))]
    use crate::draw::DrawBuffers;

    /// `deny_unknown_fields` arrives through `#[patch(attribute(...))]`,
    /// which emits its token stream verbatim -- only a bogus key proves the
    /// attribute survived generation. `limits`, `max_arena_bytes` and
    /// `screen_cache` are `UiConfigPatch`'s only declared fields, and
    /// `palette` is neither a substring of any of them nor contains one.
    #[kithara::test(native, flash(false))]
    fn an_unknown_field_is_rejected_and_named() {
        let error = serde_yaml_ng::from_str::<UiConfigPatch>("palette: dark\n")
            .expect_err("a typo must not be silently ignored");

        assert!(error.to_string().contains("palette"), "{error}");
    }

    /// `custom_kinds` is a real field on `UiConfig` but must not be document-
    /// reachable: registering a kind takes code, not configuration (see the
    /// field's doc comment), so a document value here would type and then be
    /// refused wherever no code registered a matching kind.
    #[kithara::test(native, flash(false))]
    fn the_code_owned_custom_kinds_field_is_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<UiConfigPatch>("custom_kinds: [gallery]\n")
            .expect_err("a code-registered set must not be document-settable");

        assert!(error.to_string().contains("custom_kinds"), "{error}");
    }

    /// `draw_buffers` is a real field on `UiConfig` but must not be document-
    /// reachable: it is a *built* value assembled from [`DrawPoolLimits`],
    /// which the document names instead through its own top-level section
    /// (see the field's doc comment and `Config::ui` in
    /// `kithara-app`), so a document value here would type and then be
    /// overwritten before it ever reached a compiled document.
    #[cfg(any(feature = "render", feature = "vello"))]
    #[kithara::test(native, flash(false))]
    fn the_built_draw_buffers_field_is_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<UiConfigPatch>("draw_buffers: null\n")
            .expect_err("a built value must not be document-settable");

        assert!(error.to_string().contains("draw_buffers"), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn the_arena_byte_cap_lands_without_moving_the_screen_cache() {
        let patch: UiConfigPatch =
            serde_yaml_ng::from_str("max_arena_bytes: 131072\n").expect("the document types");
        let mut config = UiConfig::default();
        // Seeded off a value the default does not produce, so a whole-struct
        // `apply` that rebuilt every unnamed field from `Default` cannot pass
        // this assertion by coincidence.
        config.screen_cache = 3;

        config.apply(patch);

        assert_eq!(config.max_arena_bytes, 131_072);
        assert_eq!(
            config.screen_cache, 3,
            "a field the document does not name must keep its seeded value"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_limits_section_reaches_the_nested_limits_without_moving_max_nodes() {
        let patch: UiConfigPatch =
            serde_yaml_ng::from_str("limits:\n  max_depth: 12\n").expect("the document types");
        let mut config = UiConfig::default();
        config.limits.max_nodes = 42;

        config.apply(patch);

        assert_eq!(config.limits.max_depth, 12);
        assert_eq!(
            config.limits.max_nodes, 42,
            "a nested field the document does not name must keep its seeded value"
        );
    }

    /// `LimitsPatch` refuses on its own, independent of `UiConfigPatch`:
    /// `max_bytes`, `max_depth` and `max_nodes` are its only declared
    /// fields, and `ceiling` is neither a substring of any of them nor
    /// contains one.
    #[kithara::test(native, flash(false))]
    fn an_unknown_limits_field_is_rejected_and_named() {
        let error = serde_yaml_ng::from_str::<LimitsPatch>("ceiling: 4\n")
            .expect_err("a typo must not be silently ignored");

        assert!(error.to_string().contains("ceiling"), "{error}");
    }

    /// `DrawPoolLimitsPatch` refuses on its own: `max_bytes`, `max_buffers`,
    /// `command_capacity`, `path_capacity` and `text_capacity` are its only
    /// declared fields, and `cushion` is neither a substring of any of them
    /// nor contains one. `max_bytes` also names an unrelated `Limits` field
    /// with a 256 KiB default rather than this struct's 64 MiB one --
    /// picking it here would prove nothing about which struct refused.
    #[kithara::test(native, flash(false))]
    fn an_unknown_draw_pool_field_is_rejected_and_named() {
        let error = serde_yaml_ng::from_str::<DrawPoolLimitsPatch>("cushion: 4\n")
            .expect_err("a typo must not be silently ignored");

        assert!(error.to_string().contains("cushion"), "{error}");
    }

    /// The path this task exists for: a `DrawPoolLimits` a document names
    /// reaches the `DrawBuffers` a `UiConfig` is built with. `max_buffers`
    /// names `DrawPoolLimits`, not `Limits.max_bytes` (256 KiB default) --
    /// this struct's own `max_bytes` default is 64 MiB.
    #[cfg(any(feature = "render", feature = "vello"))]
    #[kithara::test(native, flash(false))]
    fn a_draw_pool_limits_document_reaches_the_draw_buffers_a_ui_config_is_built_with() {
        let patch: DrawPoolLimitsPatch =
            serde_yaml_ng::from_str("max_buffers: 4\n").expect("the document types");
        let mut limits = DrawPoolLimits::default();
        // Seeded off a value the default does not produce, so an `apply`
        // that rebuilt every unnamed field from `Default` cannot pass this
        // assertion by coincidence.
        limits.command_capacity = 7;

        limits.apply(patch);

        let config = UiConfig::builder()
            .draw_buffers(DrawBuffers::new(limits))
            .build();

        assert_eq!(config.draw_buffers.limits().max_buffers, 4);
        assert_eq!(
            config.draw_buffers.limits().command_capacity,
            7,
            "a field the document does not name must keep its seeded value"
        );
    }
}
