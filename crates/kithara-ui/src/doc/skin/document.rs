use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    blanket::{FramePatch, Frames, Roles, TextRolePatch},
    controls::{
        ButtonPatch, ButtonSkin, CellPatch, CellSkin, CheckboxPatch, CheckboxSkin, ChipPatch,
        ChipSkin, CrossfaderPatch, CrossfaderSkin, FaderPatch, FaderSkin, KnobPatch, KnobSkin,
        MenuPatch, MenuSkin, NavPatch, NavSkin, PortalMapPatch, PortalMapSkin, RangePatch,
        RangeSkin, ReadoutPatch, ReadoutSkin, SegmentedPatch, SegmentedSkin, SelectPatch,
        SelectSkin, StatusDotPatch, StatusDotSkin, SwatchPatch, SwatchSkin, TabLargePatch,
        TabLargeSkin, TextPatch, TextSkin, TogglePatch, ToggleSkin, VisPatch, VisSkin,
        VuStereoPatch, VuStereoSkin, VuVerticalPatch, VuVerticalSkin,
    },
    custom::{CustomDoc, CustomPatch},
    palette::{PaletteDoc, PalettePatch},
    panels::{
        DeckPatch, DeckSkin, DividerPatch, DividerSkin, DragPatch, DragSkin, GlobalBarPatch,
        GlobalBarSkin, LayoutPreviewPatch, LayoutPreviewSkin, MeterPatch, MeterSkin, PopPatch,
        PopSkin, TablePatch, TableSkin, TelemetryPatch, TelemetrySkin, TreePatch, TreeSkin,
        WavePatch, WaveSkin,
    },
    pictures::{PictureDoc, PicturePatch},
    primitives::{
        ChromePatch, ChromeSkin, LayoutPatch, LayoutSkin, ScrollPatch, ScrollSkin, WindowPatch,
        WindowSkin,
    },
};
use crate::{
    doc::ron_io,
    envelope::{self, DocKind},
    error::UiDocError,
    ids::{DocId, SourceUri},
    source::{Limits, SourceResolver},
};

/// Every section a skin document declares, written once and expanded wherever
/// the set has to appear again: the document itself, the patch a second skin
/// writes over it, and the resolved skin a renderer reads.
macro_rules! skin_sections {
    ($expand:ident) => {
        $expand! {
            button: ButtonSkin => ButtonPatch,
            cell: CellSkin => CellPatch,
            checkbox: CheckboxSkin => CheckboxPatch,
            chip: ChipSkin => ChipPatch,
            chrome: ChromeSkin => ChromePatch,
            crossfader: CrossfaderSkin => CrossfaderPatch,
            deck: DeckSkin => DeckPatch,
            divider: DividerSkin => DividerPatch,
            drag: DragSkin => DragPatch,
            fader: FaderSkin => FaderPatch,
            global_bar: GlobalBarSkin => GlobalBarPatch,
            knob: KnobSkin => KnobPatch,
            layout_preview: LayoutPreviewSkin => LayoutPreviewPatch,
            layout: LayoutSkin => LayoutPatch,
            menu: MenuSkin => MenuPatch,
            meter: MeterSkin => MeterPatch,
            nav: NavSkin => NavPatch,
            pop: PopSkin => PopPatch,
            portal_map: PortalMapSkin => PortalMapPatch,
            range: RangeSkin => RangePatch,
            readout: ReadoutSkin => ReadoutPatch,
            segmented: SegmentedSkin => SegmentedPatch,
            select: SelectSkin => SelectPatch,
            status_dot: StatusDotSkin => StatusDotPatch,
            scroll: ScrollSkin => ScrollPatch,
            swatch: SwatchSkin => SwatchPatch,
            tab_large: TabLargeSkin => TabLargePatch,
            telemetry: TelemetrySkin => TelemetryPatch,
            text: TextSkin => TextPatch,
            toggle: ToggleSkin => TogglePatch,
            table: TableSkin => TablePatch,
            tree: TreeSkin => TreePatch,
            vis: VisSkin => VisPatch,
            vu_stereo: VuStereoSkin => VuStereoPatch,
            vu_vertical: VuVerticalSkin => VuVerticalPatch,
            wave: WaveSkin => WavePatch,
            window: WindowSkin => WindowPatch,
        }
    };
}

macro_rules! define_skin_doc {
    ($($field:ident: $section:ident => $patch:ident,)*) => {
        #[derive(Clone, Debug, Deserialize, PartialEq, Serialize, kithara_derive::SkinWalk)]
        #[serde(deny_unknown_fields)]
        #[non_exhaustive]
        pub struct SkinDoc {
            /// What this skin dresses the extensions a document places in, by
            /// kind. The toolkit owns no section for content it does not draw,
            /// so this is the only thing a skin can say about one.
            #[skin(skip_frames, skip_roles)]
            pub custom: CustomDoc,
            #[skin(skip_frames, skip_roles)]
            pub id: DocId,
            #[skin(skip_frames, skip_roles)]
            pub palette: PaletteDoc,
            /// The pictures this skin carries, which is the whole set a
            /// document may name.
            #[skin(skip_frames, skip_roles)]
            pub pictures: PictureDoc,
            #[skin(skip_frames, skip_roles)]
            pub schema: String,
            #[skin(skip_frames, skip_roles)]
            pub version: u32,
            /// What this skin restates for one control instance, by the path
            /// the document gave it. A control the skin never names wears the
            /// sections below unchanged.
            #[serde(default)]
            #[skin(skip_frames, skip_roles)]
            pub overrides: BTreeMap<String, SkinLayer>,
            $(pub $field: $section,)*
        }

        /// What a skin restates without saying whose skin it is: the blankets
        /// and the sections, each optional. A patch is this plus an identity;
        /// an override is this aimed at one control.
        #[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
        #[serde(deny_unknown_fields)]
        #[non_exhaustive]
        pub struct SkinLayer {
            /// Restated over every frame the layer reaches, before its own
            /// sections are applied.
            #[serde(default)]
            pub frames: Option<FramePatch>,
            /// Restated over every typographic role, on the same terms.
            #[serde(default)]
            pub text_roles: Option<TextRolePatch>,
            $(#[serde(default)] pub $field: Option<$patch>,)*
        }

        impl SkinLayer {
            /// Takes everything this layer restates, keeping the rest.
            ///
            /// A blanket comes before the sections it reaches, so a skin that
            /// rounds every frame and then names one square control gets both.
            pub(crate) fn apply(self, doc: &mut SkinDoc) {
                if let Some(frames) = self.frames {
                    doc.each_frame(&mut |frame| frames.apply(frame));
                }
                if let Some(roles) = self.text_roles {
                    doc.each_role(&mut |role| roles.apply(role));
                }
                $(if let Some(section) = self.$field {
                    doc.$field.patch(section);
                })*
            }
        }

        /// What one skin restates of another: any section, any field, and
        /// nothing it does not name. The envelope is not optional - a patch
        /// is a document of its own, with its own identity.
        #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
        #[serde(deny_unknown_fields)]
        #[non_exhaustive]
        pub struct SkinPatch {
            /// The skin this one is written over, relative to its own origin.
            #[serde(default)]
            pub base: Option<String>,
            pub id: DocId,
            pub schema: String,
            pub version: u32,
            /// Extensions this skin dresses differently from the ones it
            /// inherits, setting by setting.
            #[serde(default)]
            pub custom: Option<CustomPatch>,
            #[serde(default)]
            pub palette: Option<PalettePatch>,
            /// Pictures this skin draws instead of the ones it inherits, by
            /// name.
            #[serde(default)]
            pub pictures: Option<PicturePatch>,
            /// Restated over every frame the skin inherits, before its own
            /// sections are applied.
            #[serde(default)]
            pub frames: Option<FramePatch>,
            /// Restated over every typographic role, on the same terms.
            #[serde(default)]
            pub text_roles: Option<TextRolePatch>,
            /// Control instances this skin dresses differently from the ones
            /// it inherits. An entry is restated whole: it replaces whatever
            /// the base said about that path rather than merging with it.
            #[serde(default)]
            pub overrides: Option<BTreeMap<String, SkinLayer>>,
            $(#[serde(default)] pub $field: Option<$patch>,)*
        }

        impl SkinDoc {
            /// Takes everything the patch restates, keeping the rest.
            pub(crate) fn apply(&mut self, patch: SkinPatch) {
                self.id = patch.id;
                self.schema = patch.schema;
                self.version = patch.version;
                if let Some(palette) = patch.palette {
                    self.palette.patch(palette);
                }
                if let Some(custom) = patch.custom {
                    self.custom.patch(custom);
                }
                if let Some(pictures) = patch.pictures {
                    self.pictures.patch(pictures);
                }
                if let Some(overrides) = patch.overrides {
                    self.overrides.extend(overrides);
                }
                SkinLayer {
                    frames: patch.frames,
                    text_roles: patch.text_roles,
                    $($field: patch.$field,)*
                }
                .apply(self);
            }
        }
    };
}

pub(crate) use skin_sections;

skin_sections!(define_skin_doc);

/// Parses and validates a complete skin document.
///
/// # Errors
/// Returns [`UiDocError`] when the envelope, body, or palette is invalid.
pub fn parse_skin(text: &str, origin: &SourceUri) -> Result<SkinDoc, UiDocError> {
    let envelope = envelope::probe(text, origin)?;
    if envelope.kind != DocKind::Skin {
        return Err(UiDocError::WrongDocKind {
            origin: origin.clone(),
            expected: DocKind::Skin.name(),
            found: envelope.kind.name(),
        });
    }
    let mut document: SkinDoc =
        ron_io::options()
            .from_str(text)
            .map_err(|source| UiDocError::Syntax {
                origin: origin.clone(),
                source: Box::new(source),
            })?;
    document.palette.validate(origin)?;
    document.custom.validate(origin)?;
    document.pictures.rebase(origin)?;
    Ok(document)
}

/// Parses a skin that restates only part of `base`.
///
/// A skin document names every field it declares; a patch names only what it
/// changes, and inherits the rest from the skin it is written over. That is
/// what lets one palette or one control be re-skinned without copying the
/// six hundred values around it.
///
/// # Errors
/// Returns [`UiDocError`] when the envelope, body, or resulting palette is
/// invalid.
pub fn parse_skin_over(
    base: &SkinDoc,
    text: &str,
    origin: &SourceUri,
) -> Result<SkinDoc, UiDocError> {
    let envelope = envelope::probe(text, origin)?;
    if envelope.kind != DocKind::Skin {
        return Err(UiDocError::WrongDocKind {
            origin: origin.clone(),
            expected: DocKind::Skin.name(),
            found: envelope.kind.name(),
        });
    }
    let mut patch: SkinPatch =
        ron_io::options()
            .from_str(text)
            .map_err(|source| UiDocError::Syntax {
                origin: origin.clone(),
                source: Box::new(source),
            })?;
    if let Some(pictures) = patch.pictures.as_mut() {
        pictures.rebase(origin)?;
    }
    let mut document = base.clone();
    document.apply(patch);
    document.palette.validate(origin)?;
    document.custom.validate(origin)?;
    Ok(document)
}

/// Reads only the skin a document is written over, before its body is known.
#[derive(Debug, Deserialize)]
struct SkinLineage {
    #[serde(default)]
    base: Option<String>,
}

/// Loads a skin and every skin it is written over, newest last.
///
/// A skin that names no base is a whole document. A skin that names one
/// restates only what it changes, which is what lets a theme ship as a
/// handful of colours instead of a copy of the six hundred values it leaves
/// alone.
///
/// # Errors
/// Returns [`UiDocError`] when a document in the chain is missing, invalid, or
/// the chain is longer than `limits.max_depth`.
pub fn load_skin(
    resolver: &dyn SourceResolver,
    rel: &str,
    limits: &Limits,
) -> Result<SkinDoc, UiDocError> {
    load_over(resolver, None, rel, 0, limits)
}

fn load_over(
    resolver: &dyn SourceResolver,
    base: Option<&SourceUri>,
    rel: &str,
    depth: usize,
    limits: &Limits,
) -> Result<SkinDoc, UiDocError> {
    let loaded = resolver.load(base, rel)?;
    if depth > limits.max_depth {
        return Err(UiDocError::DepthExceeded {
            depth,
            origin: loaded.uri,
            max: limits.max_depth,
        });
    }
    let lineage: SkinLineage =
        ron_io::options()
            .from_str(&loaded.text)
            .map_err(|source| UiDocError::Syntax {
                origin: loaded.uri.clone(),
                source: Box::new(source),
            })?;
    match lineage.base {
        None => parse_skin(&loaded.text, &loaded.uri),
        Some(parent) => {
            let base = load_over(resolver, Some(&loaded.uri), &parent, depth + 1, limits)?;
            parse_skin_over(&base, &loaded.text, &loaded.uri)
        }
    }
}
