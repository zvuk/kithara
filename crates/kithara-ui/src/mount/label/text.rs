use crate::{
    module::TextStyle,
    mount::Control,
    size::{Dim, SizeSpec},
    skin::SkinDoc,
};

/// A run of text the document supplies or reads.
pub(crate) struct Text {
    pub(crate) style: TextStyle,
}

impl Control for Text {
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
            | TextStyle::PivotValue => SizeSpec::new(Dim::Shrink, Dim::Shrink),
            TextStyle::Body
            | TextStyle::Brand
            | TextStyle::Cell
            | TextStyle::ModuleTitle
            | TextStyle::DeckLetter
            | TextStyle::TrackTitle
            | TextStyle::Telemetry
            | TextStyle::MicroLabel
            | TextStyle::Note
            | TextStyle::Section
            | TextStyle::WindowTitle => skin.text.size,
        }
    }
}

#[cfg(any(feature = "iced", feature = "masonry"))]
pub(crate) mod host {
    use bon::Builder;

    use crate::{
        expand::{Binding, ColorSpec},
        ids::InternId,
        module::{TextAlign, TextStyle},
        skin::{ColorRole, FontFamily, FontWeight},
    };

    /// The run as a host draws it: what it says, and how it is dressed.
    #[derive(Builder)]
    pub(crate) struct Text<'a> {
        pub(crate) active: Option<&'a Binding>,
        pub(crate) active_color: Option<ColorRole>,
        pub(crate) color: Option<&'a ColorSpec>,
        pub(crate) font: Option<FontFamily>,
        pub(crate) label: Option<InternId>,
        pub(crate) weight: Option<FontWeight>,
        pub(crate) align: TextAlign,
        pub(crate) style: TextStyle,
    }
}
