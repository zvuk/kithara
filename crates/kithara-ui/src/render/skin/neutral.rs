use std::collections::BTreeMap;

use kithara_platform::sync::Arc;

use crate::{
    draw::{Rgba, TRANSPARENT},
    error::UiDocError,
    ids::SourceUri,
    module::TextStyle,
    render::{
        picture::{Pictures, Sheet},
        skin::{CustomSkin, CustomSkins},
        theme::RenderPalette,
    },
    shaping::{FontPolicy, TextResources},
    skin::{
        ButtonSkin, CellSkin, CheckboxSkin, ChipSkin, ChromeSkin, ColorRole, CrossfaderSkin,
        DeckSkin, DividerSkin, DragSkin, FaderSkin, GlobalBarSkin, KnobSkin, LayoutPreviewSkin,
        LayoutSkin, MenuSkin, MeterSkin, NavSkin, PopSkin, PortalMapSkin, RangeSkin, ReadoutSkin,
        ScrollSkin, SegmentedSkin, SelectSkin, SkinDoc, StatusDotSkin, SwatchSkin, TabLargeSkin,
        TableSkin, TelemetrySkin, TextRoleSkin, TextSkin, ToggleSkin, TreeSkin, VisSkin,
        VuStereoSkin, VuVerticalSkin, WaveSkin, WindowSkin, skin_sections,
    },
    source::SourceResolver,
    text::TextDoc,
};

/// The three captions painted around a crossfader track.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CrossfaderLabels {
    pub center: String,
    pub left: String,
    pub right: String,
}

/// Both the resolved skin a renderer reads and the resolve step that fills it
/// in: one arm per section, expanded from the document's own section list, so
/// a new section reaches the renderers by being declared once.
macro_rules! define_skin {
    ($($field:ident: $section:ident => $patch:ident,)*) => {
        /// Resolved skin consumed by renderers.
        #[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
        #[non_exhaustive]
        #[fieldwork(opt_in, get)]
        pub struct Skin {
            pub palette: RenderPalette,
            pub crossfader_labels: CrossfaderLabels,
            pub table_footer_rows: String,
            pub tree_search_placeholder: String,
            $(pub $field: $section,)*
            /// What this skin dresses each extension in, resolved. An
            /// extension reads its own kind out of it and decides what to do
            /// with what it finds, which is all a skin can say about content
            /// the toolkit does not draw.
            custom: Arc<CustomSkins>,
            /// The pictures this skin carries, cut into frames while it
            /// resolved. A document names a picture; the skin is what answers
            /// the name, so switching skins switches the drawings.
            pictures: Arc<Pictures>,
            #[field(get, vis = "pub(crate)")]
            text_resources: Arc<TextResources>,
            /// The document this skin was resolved from, which is what a
            /// host compiles its pages against: what a page measures comes
            /// from the skin's own numbers, not only what it is painted with.
            #[field(get)]
            document: Arc<SkinDoc>,
            /// The skin each named control instance wears instead of this one,
            /// resolved here rather than every frame. Everything but the
            /// sections is shared with this skin, so an override costs its
            /// own numbers and nothing else.
            overrides: BTreeMap<Box<str>, Skin>,
        }

        impl Skin {
            /// Resolves a parsed document under an explicit font policy.
            ///
            /// The resolver is the one the document was loaded through: a skin
            /// names its pictures, and resolving it is what reads them.
            ///
            /// # Errors
            /// Returns [`UiDocError`] when a palette value, embedded font or
            /// named picture is invalid, or [`UiDocError::UnknownTextKey`] when
            /// `catalog` is missing a caption.
            pub fn resolve_with_font_policy(
                document: SkinDoc,
                catalog: &TextDoc,
                origin: &SourceUri,
                resolver: &dyn SourceResolver,
                font_policy: FontPolicy,
            ) -> Result<Self, UiDocError> {
                let palette = RenderPalette::resolve(&document.palette, origin)?;
                let base = Self {
                    custom: Arc::new(CustomSkins::resolve(&document.custom, &palette, origin)?),
                    pictures: Arc::new(Pictures::load(&document.pictures, resolver)?),
                    palette,
                    crossfader_labels: CrossfaderLabels {
                        left: text_field(catalog, "crossfader.left_label", origin)?,
                        center: text_field(catalog, "crossfader.center_label", origin)?,
                        right: text_field(catalog, "crossfader.right_label", origin)?,
                    },
                    table_footer_rows: text_field(catalog, "table.footer_rows", origin)?,
                    tree_search_placeholder: text_field(catalog, "tree.search_placeholder", origin)?,
                    $($field: document.$field,)*
                    text_resources: Arc::new(TextResources::new(font_policy)?),
                    document: Arc::new(document),
                    overrides: BTreeMap::new(),
                };
                let overrides = base
                    .document
                    .overrides
                    .iter()
                    .map(|(path, layer)| {
                        let mut document = (*base.document).clone();
                        document.overrides.clear();
                        layer.clone().apply(&mut document);
                        let dressed = Self {
                            $($field: document.$field,)*
                            document: Arc::new(document),
                            ..base.clone()
                        };
                        (Box::from(path.as_str()), dressed)
                    })
                    .collect();
                Ok(Self { overrides, ..base })
            }
        }
    };
}

skin_sections!(define_skin);

/// The one rule that picks between a node's own colour and its active one.
///
/// A node is active or it is not, and the active role only wins while it is;
/// a node naming no active role keeps the base one it declared.
pub(crate) fn active_tone(
    base: Option<ColorRole>,
    active: Option<ColorRole>,
    on: bool,
) -> Option<ColorRole> {
    on.then_some(active).flatten().or(base)
}

impl Skin {
    /// The skin one control instance wears.
    ///
    /// A skin dresses a control by kind; an override dresses one instance the
    /// document named, and everything the override leaves alone is still the
    /// skin's. A path the skin never names is this skin, so asking is always
    /// safe and never copies.
    #[must_use]
    pub fn at(&self, path: &str) -> &Self {
        self.overrides.get(path).unwrap_or(self)
    }

    /// What this skin dresses one extension kind in.
    ///
    /// A kind this skin never names is dressed in nothing rather than refused:
    /// an extension is registered by the application, and a skin is written
    /// without knowing which build will wear it. What an extension draws when
    /// it is dressed in nothing is its own business.
    #[must_use]
    pub fn custom(&self, kind: &str) -> &CustomSkin {
        self.custom.kind(kind).unwrap_or(&EMPTY_DRESS)
    }

    /// What the skin's own document calls it, which is how anything offering a
    /// choice of skins tells one from another.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.document.id.0
    }

    /// Resolves a parsed document into neutral colors, render metrics and the
    /// pictures it names, pulling the crossfader, tree search and table footer
    /// captions from `catalog`.
    ///
    /// # Errors
    /// Returns [`UiDocError`] when a palette value, embedded font or named
    /// picture is invalid, or [`UiDocError::UnknownTextKey`] when `catalog` is
    /// missing one of those captions.
    pub fn resolve(
        document: SkinDoc,
        catalog: &TextDoc,
        origin: &SourceUri,
        resolver: &dyn SourceResolver,
    ) -> Result<Self, UiDocError> {
        Self::resolve_with_font_policy(document, catalog, origin, resolver, FontPolicy::System)
    }

    pub(crate) fn rgba(&self, role: ColorRole) -> Rgba {
        self.palette[role]
    }

    /// The picture one name means in this skin, cut into its frames.
    ///
    /// A name this skin carries nothing for draws nothing, which is what an
    /// unbound control does everywhere else.
    #[must_use]
    pub fn sheet(&self, name: &str) -> Option<&Arc<Sheet>> {
        self.pictures.sheet(name)
    }

    /// The typography one document text style names, with the tone already
    /// selected.
    ///
    /// Both hosts ask the skin rather than keeping a table each: a style the
    /// two answered differently would paint the same document in two
    /// typefaces, which is the one thing the shared base exists to prevent.
    /// There is no wildcard arm, so a new style does not build until it is
    /// given a skin entry.
    pub(crate) fn text_role(
        &self,
        style: TextStyle,
        color: Option<ColorRole>,
        active_color: Option<ColorRole>,
        active: bool,
    ) -> TextRoleSkin {
        let role = self.text.role(style);
        let skin_active = (style == TextStyle::DeckLetter).then_some(self.text.deck_letter_active);
        TextRoleSkin {
            color: active_tone(color, active_color.or(skin_active), active).unwrap_or(role.color),
            ..role
        }
    }

    /// Resolves one state of a [`StateColors`]: a state naming no role paints
    /// nothing.
    pub(crate) fn tint(&self, role: Option<ColorRole>) -> Rgba {
        role.map_or(TRANSPARENT, |role| self.rgba(role))
    }
}

/// What a kind this skin never names is dressed in, which is nothing. It is a
/// static rather than a fresh empty one so the answer can be borrowed.
static EMPTY_DRESS: CustomSkin = CustomSkin::EMPTY;

fn text_field(catalog: &TextDoc, key: &str, origin: &SourceUri) -> Result<String, UiDocError> {
    catalog
        .get(key)
        .map(str::to_owned)
        .ok_or_else(|| UiDocError::UnknownTextKey {
            origin: origin.clone(),
            key: key.to_owned(),
            path: format!("skin.{key}"),
        })
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{builtin, module::TextStyle, skin::ColorRole};

    #[kithara::test]
    fn a_node_colour_stands_in_for_the_one_the_role_carries() {
        let skin = builtin::skin();

        assert_eq!(
            skin.text_role(TextStyle::Mono, Some(ColorRole::Text), None, false),
            TextRoleSkin {
                color: ColorRole::Text,
                ..skin.text.mono
            }
        );
    }

    #[kithara::test]
    fn a_node_switches_between_the_two_colours_it_names() {
        let skin = builtin::skin();
        let role = |active| {
            skin.text_role(
                TextStyle::Mono,
                Some(ColorRole::Muted),
                Some(ColorRole::Accent),
                active,
            )
        };

        assert_eq!(
            role(true),
            TextRoleSkin {
                color: ColorRole::Accent,
                ..skin.text.mono
            }
        );
        assert_eq!(
            role(false),
            TextRoleSkin {
                color: ColorRole::Muted,
                ..skin.text.mono
            }
        );
    }

    #[kithara::test]
    fn an_active_node_naming_one_colour_keeps_it() {
        let skin = builtin::skin();

        assert_eq!(
            skin.text_role(TextStyle::Caption, Some(ColorRole::Accent), None, true),
            TextRoleSkin {
                color: ColorRole::Accent,
                ..skin.text.caption
            }
        );
    }

    #[kithara::test]
    fn the_deck_letter_takes_the_active_colour_its_skin_entry_declares() {
        let skin = builtin::skin();
        let base = skin.text_role(TextStyle::DeckLetter, None, None, false);

        assert_eq!(base, skin.text.deck_letter);
        assert_eq!(
            skin.text_role(TextStyle::DeckLetter, None, None, true),
            TextRoleSkin {
                color: skin.text.deck_letter_active,
                ..base
            }
        );
        assert_eq!(
            skin.text_role(TextStyle::DeckLetter, None, Some(ColorRole::Warning), true),
            TextRoleSkin {
                color: ColorRole::Warning,
                ..base
            }
        );
    }

    #[kithara::test]
    fn brand_small_resolves_under_the_display_family_and_never_the_mono_one() {
        let skin = builtin::skin();
        let role = skin.text_role(TextStyle::BrandSmall, None, None, false);

        assert_eq!(role, skin.text.brand_small);
        assert_eq!(
            skin.text_role(TextStyle::BrandSmall, None, None, true),
            role
        );
        assert_ne!(
            role.font, skin.text.mono.font,
            "the mono micro roles are Mono and the brand pair is Display"
        );
    }

    #[kithara::test]
    fn a_style_declaring_no_active_colour_ignores_the_flag() {
        let skin = builtin::skin();

        for style in [
            TextStyle::Body,
            TextStyle::Brand,
            TextStyle::TrackTitle,
            TextStyle::Telemetry,
            TextStyle::MicroLabel,
            TextStyle::Section,
            TextStyle::Mono,
            TextStyle::PivotArrow,
            TextStyle::PivotTitle,
            TextStyle::PivotValue,
            TextStyle::Caption,
            TextStyle::VisFooter,
            TextStyle::VisMeta,
            TextStyle::VisTitle,
        ] {
            assert_eq!(
                skin.text_role(style, None, None, true),
                skin.text_role(style, None, None, false),
                "{style:?}"
            );
        }
    }

    #[kithara::test]
    fn active_tone_takes_the_active_role_only_while_the_flag_is_set() {
        let pair =
            |active| active_tone(Some(ColorRole::LineInner), Some(ColorRole::Accent), active);

        assert_eq!(pair(true), Some(ColorRole::Accent));
        assert_eq!(pair(false), Some(ColorRole::LineInner));
        assert_eq!(
            active_tone(Some(ColorRole::LineHi), None, true),
            Some(ColorRole::LineHi)
        );
        assert_eq!(active_tone(None, None, true), None);
    }
}
