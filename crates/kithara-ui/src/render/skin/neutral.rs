use std::collections::BTreeMap;

use kithara_platform::sync::Arc;

use crate::{
    error::UiDocError,
    ids::SourceUri,
    render::{
        picture::{Pictures, Sheet},
        skin::{CustomSkin, CustomSkins},
        theme::RenderPalette,
    },
    shaping::{FontPolicy, TextResources},
    skin::{
        ButtonSkin, CellSkin, CheckboxSkin, ChipSkin, ChromeSkin, CrossfaderSkin, DeckSkin,
        DividerSkin, DragSkin, FaderSkin, GlobalBarSkin, KnobSkin, LayoutPreviewSkin, LayoutSkin,
        MenuSkin, MeterSkin, ModalSkin, NavSkin, PopSkin, PortalMapSkin, RangeSkin, ReadoutSkin,
        ScrollSkin, SegmentedSkin, SelectSkin, SkinDoc, StatusDotSkin, SwatchSkin, TabLargeSkin,
        TableSkin, TelemetrySkin, TextSkin, ToggleSkin, TreeSkin, VisSkin, VuStereoSkin,
        VuVerticalSkin, WaveSkin, WindowSkin, skin_sections,
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
            pub(crate) text_resources: Arc<TextResources>,
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
                    pictures: Arc::new(Pictures::load(&document.pictures, &document.artworks, origin, resolver)?),
                    palette,
                    crossfader_labels: CrossfaderLabels {
                        left: text_field(catalog, "crossfader.left_label", origin)?,
                        center: text_field(catalog, "crossfader.center_label", origin)?,
                        right: text_field(catalog, "crossfader.right_label", origin)?,
                    },
                    table_footer_rows: text_field(catalog, "table.footer_rows", origin)?,
                    tree_search_placeholder: text_field(catalog, "tree.search_placeholder", origin)?,
                    $($field: document.$field,)*
                    text_resources: Arc::new(load_text_resources(&document, font_policy, origin, resolver)?),
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

fn load_text_resources(
    document: &SkinDoc,
    policy: FontPolicy,
    origin: &SourceUri,
    resolver: &dyn SourceResolver,
) -> Result<TextResources, UiDocError> {
    let mut fonts = BTreeMap::new();
    for (&family, files) in &document.fonts {
        let bytes = files
            .iter()
            .map(|path| resolver.bytes(None, path).map(|loaded| loaded.bytes))
            .collect::<Result<Vec<_>, _>>()?;
        fonts.insert(family, bytes);
    }
    TextResources::with_fonts(policy, fonts).map_err(|source| UiDocError::Font {
        origin: origin.clone(),
        source,
    })
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

    /// The picture one name means in this skin, cut into its frames.
    ///
    /// A name this skin carries nothing for draws nothing, which is what an
    /// unbound control does everywhere else.
    #[must_use]
    pub fn sheet(&self, name: &str) -> Option<&Arc<Sheet>> {
        self.pictures.sheet(name)
    }

    /// A skin-owned Lottie, or one of the toolkit's shipped artworks.
    #[must_use]
    pub fn artwork(&self, name: &str) -> Option<&Arc<crate::lottie::Artwork>> {
        self.pictures
            .artwork(name)
            .or_else(|| crate::lottie::builtin_artwork(name))
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
