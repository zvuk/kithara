use bon::Builder;

use crate::{
    expand::Binding,
    ids::InternId,
    module::{TextAlign, TextStyle},
    mount::Control,
    size::{Dim, SizeSpec},
    skin::{ColorRole, SkinDoc},
};

/// A run of text the document supplies or reads.
#[derive(Builder)]
pub(crate) struct Text<'a> {
    pub(crate) active: Option<&'a Binding>,
    pub(crate) active_color: Option<ColorRole>,
    pub(crate) align: TextAlign,
    pub(crate) color: Option<ColorRole>,
    pub(crate) label: Option<InternId>,
    pub(crate) style: TextStyle,
}

impl Control for Text<'_> {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        match self.style {
            TextStyle::VisFooter => SizeSpec::new(Dim::Fill, Dim::Fixed(skin.vis.footer_height)),
            TextStyle::VisMeta | TextStyle::VisTitle => {
                SizeSpec::new(Dim::Fill, Dim::Fixed(skin.vis.header_height))
            }
            TextStyle::BrandSmall
            | TextStyle::Caption
            | TextStyle::Mono
            | TextStyle::PivotArrow
            | TextStyle::PivotDuration
            | TextStyle::PivotFooter
            | TextStyle::PivotLabel
            | TextStyle::PivotRatio
            | TextStyle::PivotSmall
            | TextStyle::PivotTitle
            | TextStyle::PivotTrackArtist
            | TextStyle::PivotTrackTitle
            | TextStyle::PivotValue => SizeSpec::new(Dim::Shrink, Dim::Fill),
            TextStyle::Body
            | TextStyle::Brand
            | TextStyle::DeckLetter
            | TextStyle::TrackTitle
            | TextStyle::Telemetry
            | TextStyle::MicroLabel
            | TextStyle::Section => skin.text.size,
        }
    }
}
