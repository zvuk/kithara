use super::Bar;
use crate::{
    draw::{DrawListBuilder, Rect},
    interact::{Input, recognizers::wheel},
};

/// How far one wheel detent moves the content. A viewport of rows is read by
/// the row, so a detent is worth about one of them rather than a page.
const STEP: f32 = 40.0;

/// A bounded window over content taller than itself.
///
/// The offset is the host's, not the document's: the document declares the
/// window, and how far the content may travel is only known once the child has
/// been laid out. Both hosts keep one of these, so a wheel means the same
/// distance on either and the indicator they draw comes out of the same three
/// numbers.
#[derive(Default)]
pub(crate) struct Window {
    accum: f32,
    content: f32,
    extent: f32,
    offset: f32,
}

impl Window {
    pub(crate) const fn new() -> Self {
        Self {
            accum: 0.0,
            content: 0.0,
            extent: 0.0,
            offset: 0.0,
        }
    }

    /// Takes the measured content and window, and answers where the content
    /// now starts. A window that grew past the end of the travel pulls the
    /// content back down with it rather than leaving blank space below.
    pub(crate) fn measured(&mut self, content: f32, extent: f32) -> f32 {
        self.content = content;
        self.extent = extent;
        self.offset = self.offset.clamp(0.0, self.travel());
        self.offset
    }

    /// Moves the window, answering whether it actually moved.
    ///
    /// A wheel at either end of the travel is not consumed, so it continues to
    /// whatever encloses this viewport instead of being swallowed by a window
    /// that has nowhere left to go.
    pub(crate) fn wheel(&mut self, input: Input<'_>) -> bool {
        let Input::Wheel(scroll) = input else {
            return false;
        };
        let steps = wheel::steps(&mut self.accum, scroll);
        if steps == 0.0 {
            return false;
        }
        let next = steps.mul_add(STEP, self.offset).clamp(0.0, self.travel());
        std::mem::replace(&mut self.offset, next) != next
    }

    const fn travel(&self) -> f32 {
        let travel = self.content - self.extent;
        if travel > 0.0 { travel } else { 0.0 }
    }

    /// Draws the indicator over the window's own right edge.
    ///
    /// A window with nothing hidden below it draws nothing: an indicator that
    /// fills its own track says the same as no indicator at all, and saying it
    /// on every page that merely declares a viewport would put a bar beside
    /// content that never moves.
    pub(crate) fn indicate(&self, bounds: Rect, bar: Bar, list: &mut DrawListBuilder) {
        let travel = self.travel();
        if travel <= 0.0 {
            return;
        }
        bar.draw(
            bounds,
            self.extent / self.content,
            self.offset / travel,
            list,
        );
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Bar, STEP, Window};
    use crate::{
        builtin,
        draw::{DrawCmd, DrawListBuilder, Geom, Rect},
        interact::{Input, Scroll},
    };

    fn viewport(content: f32, extent: f32) -> Window {
        let mut view = Window::default();
        view.measured(content, extent);
        view
    }

    fn lines(y: f32) -> Input<'static> {
        Input::Wheel(Scroll::Lines { x: 0.0, y })
    }

    fn window() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 100.0,
        }
    }

    fn drawn(view: &Window) -> Vec<Rect> {
        let mut list = DrawListBuilder::default();
        view.indicate(window(), Bar::new(builtin::skin()), &mut list);
        list.finish()
            .commands()
            .iter()
            .filter_map(|command| match command {
                DrawCmd::Fill {
                    geom: Geom::Rect(rect),
                    ..
                } => Some(*rect),
                _ => None,
            })
            .collect()
    }

    /// The window walks the content and stops at its end rather than running
    /// past it into blank space.
    #[kithara::test]
    fn the_window_travels_the_overflow_and_clamps_at_both_ends() {
        let mut view = viewport(300.0, 100.0);

        assert!(view.wheel(lines(-1.0)));
        assert_eq!(view.offset, STEP);
        for _ in 0..20 {
            view.wheel(lines(-1.0));
        }
        assert_eq!(view.offset, 200.0, "the last row is the end of the travel");

        for _ in 0..20 {
            view.wheel(lines(1.0));
        }
        assert_eq!(view.offset, 0.0);
    }

    /// A window with nothing hidden below it has no travel, so the wheel is
    /// left for whatever encloses it.
    #[kithara::test]
    fn a_window_taller_than_its_content_answers_no_wheel() {
        let mut view = viewport(80.0, 100.0);

        assert!(!view.wheel(lines(-1.0)));
        assert_eq!(view.offset, 0.0);
    }

    /// At the end of the travel a further wheel the same way is not consumed,
    /// while one back the other way still is.
    #[kithara::test]
    fn the_end_of_travel_releases_the_wheel_in_one_direction_only() {
        let mut view = viewport(140.0, 100.0);
        assert!(view.wheel(lines(-1.0)));

        assert!(!view.wheel(lines(-1.0)), "there is nowhere further to go");
        assert!(view.wheel(lines(1.0)));
    }

    /// A window that grew past the end of its travel takes the content back
    /// down with it, instead of holding an offset that would leave the last
    /// row above the bottom edge.
    #[kithara::test]
    fn a_window_that_grew_pulls_the_content_back_to_the_end_of_the_travel() {
        let mut view = viewport(300.0, 100.0);
        for _ in 0..20 {
            view.wheel(lines(-1.0));
        }

        assert_eq!(view.measured(300.0, 250.0), 50.0);
    }

    #[kithara::test]
    fn a_window_with_nothing_hidden_draws_no_indicator() {
        assert!(drawn(&viewport(80.0, 100.0)).is_empty());
    }

    #[kithara::test]
    fn an_overflowing_window_draws_a_track_and_a_thumb() {
        assert_eq!(drawn(&viewport(300.0, 100.0)).len(), 2);
    }

    /// The thumb says how much of the content the window shows: a third of it
    /// here, so a third of the track.
    #[kithara::test]
    fn the_thumb_is_the_visible_share_of_the_track() {
        let drawn = drawn(&viewport(300.0, 100.0));
        let [track, thumb] = drawn.as_slice() else {
            panic!("an overflowing window draws a track and a thumb: {drawn:?}");
        };

        assert_eq!(thumb.h, (track.h / 3.0).round());
    }

    #[kithara::test]
    fn the_thumb_starts_at_the_top_of_the_track_before_any_wheel() {
        let drawn = drawn(&viewport(300.0, 100.0));
        let [track, thumb] = drawn.as_slice() else {
            panic!("an overflowing window draws a track and a thumb: {drawn:?}");
        };

        assert_eq!(thumb.y, track.y);
    }

    /// At the end of the travel the thumb is at the end of its own, so the
    /// bar reads as finished rather than as nearly finished.
    #[kithara::test]
    fn the_thumb_ends_with_the_travel() {
        let mut view = viewport(300.0, 100.0);
        for _ in 0..20 {
            view.wheel(lines(-1.0));
        }

        let drawn = drawn(&view);
        let [track, thumb] = drawn.as_slice() else {
            panic!("an overflowing window draws a track and a thumb: {drawn:?}");
        };

        assert_eq!(thumb.y + thumb.h, track.y + track.h);
    }

    /// The track hangs inside the window it belongs to, the way a frame side
    /// does: a bar centred on the right edge would put half of itself outside
    /// the window, on whatever is drawn beside it.
    #[kithara::test]
    fn the_track_hangs_inside_the_window() {
        let drawn = drawn(&viewport(300.0, 100.0));
        let [track, _] = drawn.as_slice() else {
            panic!("an overflowing window draws a track and a thumb: {drawn:?}");
        };
        let window = window();

        assert!(track.x + track.w <= window.x + window.w);
    }
}
