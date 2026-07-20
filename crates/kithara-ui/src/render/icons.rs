use iced::{
    Color, Element, Length, Point, Rectangle, Renderer, Theme,
    widget::{
        Canvas,
        canvas::{self, Frame, Geometry, Path, Stroke},
        svg::{self, Handle as SvgHandle, Svg},
    },
};

/// Icon available to renderers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Icon {
    Disc,
    Collection,
    Folder,
    Faders,
    Gear,
    Home,
    Instrument,
    Lock,
    MusicNote,
    Pause,
    Play,
    PlaylistAdd,
    Playlist,
    Plus,
    RepeatOnce,
    Repeat,
    Shuffle,
    SkipBack,
    SkipForward,
    SpeakerHigh,
    SpeakerLow,
    SpeakerX,
    Search,
    Charts,
    Clock,
    Monitor,
    Usb,
    Waveform,
    Zvuk,
    ZoomIn,
    ZoomOut,
}

impl Icon {
    /// Renders this icon with the given size and color.
    #[must_use]
    pub fn view<'a, M: 'a>(self, size: f32, color: Color) -> Element<'a, M> {
        icon_bytes(self).map_or_else(
            || {
                Canvas::new(ZoomIcon {
                    plus: self == Self::ZoomIn,
                    color,
                })
                .width(Length::Fixed(size))
                .height(Length::Fixed(size))
                .into()
            },
            |bytes| {
                Svg::new(SvgHandle::from_memory(bytes))
                    .width(Length::Fixed(size))
                    .height(Length::Fixed(size))
                    .style(move |_theme, _status| svg::Style { color: Some(color) })
                    .into()
            },
        )
    }
}

fn icon_bytes(icon: Icon) -> Option<&'static [u8]> {
    match icon {
        Icon::Disc => Some(include_bytes!("../../assets/icons/disc.svg")),
        Icon::Collection => Some(include_bytes!("../../assets/icons/collection.svg")),
        Icon::Folder => Some(include_bytes!("../../assets/icons/folder.svg")),
        Icon::Faders => Some(include_bytes!("../../assets/icons/faders.svg")),
        Icon::Gear => Some(include_bytes!("../../assets/icons/gear.svg")),
        Icon::Home => Some(include_bytes!("../../assets/icons/home.svg")),
        Icon::Instrument => Some(include_bytes!("../../assets/icons/instrument.svg")),
        Icon::Lock => Some(include_bytes!("../../assets/icons/lock.svg")),
        Icon::MusicNote => Some(include_bytes!("../../assets/icons/music-note.svg")),
        Icon::Pause => Some(include_bytes!("../../assets/icons/pause.svg")),
        Icon::Play => Some(include_bytes!("../../assets/icons/play.svg")),
        Icon::PlaylistAdd => Some(include_bytes!("../../assets/icons/playlist-add.svg")),
        Icon::Playlist => Some(include_bytes!("../../assets/icons/playlist.svg")),
        Icon::Plus => Some(include_bytes!("../../assets/icons/plus.svg")),
        Icon::RepeatOnce => Some(include_bytes!("../../assets/icons/repeat-once.svg")),
        Icon::Repeat => Some(include_bytes!("../../assets/icons/repeat.svg")),
        Icon::Shuffle => Some(include_bytes!("../../assets/icons/shuffle.svg")),
        Icon::SkipBack => Some(include_bytes!("../../assets/icons/skip-back.svg")),
        Icon::SkipForward => Some(include_bytes!("../../assets/icons/skip-forward.svg")),
        Icon::SpeakerHigh => Some(include_bytes!("../../assets/icons/speaker-high.svg")),
        Icon::SpeakerLow => Some(include_bytes!("../../assets/icons/speaker-low.svg")),
        Icon::SpeakerX => Some(include_bytes!("../../assets/icons/speaker-x.svg")),
        Icon::Search => Some(include_bytes!("../../assets/icons/search.svg")),
        Icon::Charts => Some(include_bytes!("../../assets/icons/charts.svg")),
        Icon::Clock => Some(include_bytes!("../../assets/icons/clock.svg")),
        Icon::Monitor => Some(include_bytes!("../../assets/icons/monitor.svg")),
        Icon::Usb => Some(include_bytes!("../../assets/icons/usb.svg")),
        Icon::Waveform => Some(include_bytes!("../../assets/icons/waveform.svg")),
        Icon::Zvuk => Some(include_bytes!("../../assets/icons/zvuk.svg")),
        Icon::ZoomIn | Icon::ZoomOut => None,
    }
}

struct ZoomIcon {
    plus: bool,
    color: Color,
}

impl<Message> canvas::Program<Message> for ZoomIcon {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let scale = bounds.width.min(bounds.height) / 12.0;
        let center = Point::new(4.75 * scale, 4.75 * scale);
        let stroke = Stroke::default()
            .with_color(self.color)
            .with_width(1.25 * scale);
        frame.stroke(&Path::circle(center, 3.25 * scale), stroke);
        frame.stroke(
            &Path::line(
                Point::new(7.1 * scale, 7.1 * scale),
                Point::new(10.75 * scale, 10.75 * scale),
            ),
            stroke,
        );
        frame.stroke(
            &Path::line(
                Point::new(3.2 * scale, center.y),
                Point::new(6.3 * scale, center.y),
            ),
            stroke,
        );
        if self.plus {
            frame.stroke(
                &Path::line(
                    Point::new(center.x, 3.2 * scale),
                    Point::new(center.x, 6.3 * scale),
                ),
                stroke,
            );
        }
        vec![frame.into_geometry()]
    }
}
