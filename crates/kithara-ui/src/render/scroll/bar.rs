use crate::{
    draw::{DrawListBuilder, Rect, Rgba},
    render::Skin,
};

/// What a viewport's indicator looks like, resolved once from the skin so a
/// host that has no skin at paint time still draws the same bar.
#[derive(Clone, Copy)]
pub(crate) struct Bar {
    inset: f32,
    min_length: f32,
    thumb_color: Rgba,
    track_color: Rgba,
    width: f32,
}

impl Bar {
    pub(crate) fn new(skin: &Skin) -> Self {
        let scroll = skin.scroll;
        Self {
            inset: scroll.inset,
            min_length: scroll.min_length,
            thumb_color: skin.rgba(scroll.thumb),
            track_color: skin.rgba(scroll.track),
            width: scroll.width,
        }
    }

    /// Draws the bar over the right edge of `bounds`: a thumb covering `share`
    /// of the track, `at` of the way down whatever the track leaves it.
    ///
    /// Whole rows on both hosts: two rasterisers agree about a rectangle that
    /// starts and ends on a pixel, and need not about one that stops halfway
    /// through a row.
    pub(super) fn draw(self, bounds: Rect, share: f32, at: f32, list: &mut DrawListBuilder) {
        if self.width <= 0.0 {
            return;
        }
        let track = self.track(bounds);
        if track.h <= 0.0 {
            return;
        }
        let length = (track.h * share).max(self.min_length).min(track.h).round();
        list.fill_rect(track, self.track_color);
        list.fill_rect(
            Rect {
                y: (track.y + (track.h - length) * at).round(),
                h: length,
                ..track
            },
            self.thumb_color,
        );
    }

    /// The track hangs inside the window it belongs to, the way a frame side
    /// does: a bar centred on the edge would put half of itself outside the
    /// window, on whatever is drawn beside it.
    fn track(self, bounds: Rect) -> Rect {
        Rect {
            x: (bounds.x + bounds.w - self.width - self.inset).round(),
            y: (bounds.y + self.inset).round(),
            w: self.width,
            h: (bounds.h - self.inset - self.inset).max(0.0).round(),
        }
    }
}
