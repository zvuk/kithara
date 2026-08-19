use kithara_platform::time::Instant;

use super::{
    super::{CursorShape, Hit, Hover, Input, Outcome, PointerInput, PointerPhase},
    DoubleClick, wheel,
};
use crate::draw::{Pt, Rect};

/// How a pointer position becomes a value. A relative track counts travel from
/// the press, so the press only arms it; an absolute track reads the position
/// itself, so the press seeks straight there.
#[derive(Clone, Copy)]
pub(crate) enum Track {
    /// Vertical travel divided by `range` and added to `value`; up is positive.
    RelativeVertical { range: f32, value: f32 },
    /// Horizontal travel over the area's width, scaled and *subtracted* from
    /// `value`: the content moves with the pointer under a fixed playhead, so
    /// dragging right walks the position back.
    RelativeHorizontal { scale: f32, value: f32 },
    /// Horizontal travel added to `value` in pixels, floored at `minimum`. The
    /// only track whose value is a width rather than a fraction, which is why
    /// it has a floor and no ceiling.
    HorizontalPixels { minimum: f32, value: f32 },
    /// The position normalized against the area's height, bottom at zero.
    AbsoluteVertical,
    /// The position normalized against the area's width.
    AbsoluteHorizontal,
    /// The position normalized against the area's width, once: the press seeks
    /// and never arms, so the pointer stays free for whoever wants it next.
    HorizontalClick,
}

impl Track {
    const fn arms(self) -> bool {
        !matches!(self, Self::HorizontalClick)
    }

    /// The same track counting from a new value.
    ///
    /// A relative track starts from the value it was built with, so a host that
    /// keeps its widgets has to re-make it whenever the endpoint moves or the
    /// next drag walks the control back to where it mounted. An absolute track
    /// reads the position itself and has nothing to move; a pixel track's value
    /// is a width rather than a fraction, and is not what an endpoint reports.
    #[cfg(feature = "masonry")]
    pub(crate) const fn at(self, value: f32) -> Self {
        match self {
            Self::RelativeVertical { range, .. } => Self::RelativeVertical { range, value },
            Self::RelativeHorizontal { scale, .. } => Self::RelativeHorizontal { scale, value },
            Self::HorizontalPixels { .. }
            | Self::AbsoluteVertical
            | Self::AbsoluteHorizontal
            | Self::HorizontalClick => self,
        }
    }
}

#[derive(bon::Builder)]
pub(crate) struct Scalar {
    track: Track,
    hover: Hover,
    reset: Option<f32>,
    wheel: Option<WheelStep>,
}

/// Opt-in wheel stepping: the current normalized value plus the per-tick step.
#[derive(Clone, Copy)]
pub(crate) struct WheelStep {
    pub(crate) value: f32,
    pub(crate) step: f32,
}

#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct ScalarState {
    #[field(get = captures_pointer, vis = "pub(crate)")]
    active: bool,
    start_position: f32,
    start_value: f32,
    double_click: DoubleClick,
    wheel_accum: f32,
}

impl ScalarState {
    pub(crate) fn cancel_pointer(&mut self) {
        self.active = false;
    }
}

impl Scalar {
    #[cfg(test)]
    pub(crate) const fn accepts_double_click(&self) -> bool {
        self.reset.is_some()
    }

    #[cfg(test)]
    pub(crate) const fn accepts_wheel(&self) -> bool {
        self.wheel.is_some()
    }

    pub(crate) fn on_input(
        &self,
        state: &mut ScalarState,
        input: Input<'_>,
        hit: &Hit,
        now: Instant,
    ) -> Outcome {
        match input {
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Down => {
                let Some(position) = hit.inside() else {
                    return Outcome::IGNORED;
                };
                if let Some(value) = self.reset
                    && state.double_click.register(position, now)
                {
                    state.active = false;
                    return Outcome::set(value);
                }
                let travel_position = pointer.at.unwrap_or(position);
                state.active = self.track.arms();
                match self.track {
                    Track::RelativeVertical { value, .. } => {
                        state.start_position = travel_position.y;
                        state.start_value = value;
                        Outcome::captured()
                    }
                    Track::RelativeHorizontal { value, .. }
                    | Track::HorizontalPixels { value, .. } => {
                        state.start_position = travel_position.x;
                        state.start_value = value;
                        Outcome::captured()
                    }
                    Track::AbsoluteVertical => seek_down(position, hit.area()),
                    Track::AbsoluteHorizontal | Track::HorizontalClick => {
                        seek_across(position, hit.area())
                    }
                }
            }
            Input::Pointer(PointerInput {
                phase: PointerPhase::Move,
                at: Some(at),
                ..
            }) if state.active => match self.track {
                Track::RelativeVertical { range, .. } => Outcome::set(
                    (state.start_value + (state.start_position - at.y) / range).clamp(0.0, 1.0),
                ),
                Track::RelativeHorizontal { scale, .. } => {
                    let width = hit.area().w;
                    if width > 0.0 {
                        Outcome::set(
                            (state.start_value - (at.x - state.start_position) / width * scale)
                                .clamp(0.0, 1.0),
                        )
                    } else {
                        Outcome::IGNORED
                    }
                }
                Track::HorizontalPixels { minimum, .. } => {
                    Outcome::set((state.start_value + at.x - state.start_position).max(minimum))
                }
                Track::AbsoluteVertical => hit
                    .at()
                    .map_or(Outcome::IGNORED, |position| seek_down(position, hit.area())),
                Track::AbsoluteHorizontal | Track::HorizontalClick => {
                    hit.at().map_or(Outcome::IGNORED, |position| {
                        seek_across(position, hit.area())
                    })
                }
            },
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Up && state.active => {
                state.active = false;
                Outcome::captured()
            }
            Input::Wheel(scroll) if hit.over() => {
                let Some(wheel) = self.wheel else {
                    return Outcome::IGNORED;
                };
                let steps = wheel::steps(&mut state.wheel_accum, scroll);
                if steps == 0.0 {
                    return Outcome::captured();
                }
                let value = wheel.step.mul_add(steps, wheel.value);
                Outcome::set(value.clamp(0.0, 1.0))
            }
            Input::InputMethod(_)
            | Input::KeyPressed { .. }
            | Input::KeyReleased { .. }
            | Input::ModifiersChanged(_)
            | Input::Pointer(_)
            | Input::Wheel(_) => Outcome::IGNORED,
        }
    }

    pub(crate) fn cursor(&self, state: &ScalarState, hit: &Hit) -> CursorShape {
        self.hover.cursor(state.active, hit)
    }
}

fn seek_down(position: Pt, area: Rect) -> Outcome {
    (area.h > 0.0)
        .then(|| (1.0 - (position.y - area.y) / area.h).clamp(0.0, 1.0))
        .map_or(Outcome::IGNORED, Outcome::set)
}

fn seek_across(position: Pt, area: Rect) -> Outcome {
    (area.w > 0.0)
        .then(|| ((position.x - area.x) / area.w).clamp(0.0, 1.0))
        .map_or(Outcome::IGNORED, Outcome::set)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{
        super::super::{Scroll, mouse as mouse_input},
        *,
    };

    fn knob() -> Rect {
        Rect {
            h: 34.0,
            w: 34.0,
            x: 0.0,
            y: 0.0,
        }
    }

    fn hit(y: f32) -> Hit {
        Hit::new(Some(Pt { x: 17.0, y }), knob())
    }

    /// This recognizer normalizes against the area, so it reads the hit and not
    /// the event; the event carries the same point so the fixture reads true.
    fn moved(y: f32) -> Input<'static> {
        Input::Pointer(mouse_input(PointerPhase::Move, Some(Pt { x: 17.0, y })))
    }

    fn moved_on_meter(y: f32) -> Input<'static> {
        Input::Pointer(mouse_input(PointerPhase::Move, Some(Pt { x: 6.0, y })))
    }

    fn pointer(phase: PointerPhase) -> Input<'static> {
        Input::Pointer(mouse_input(phase, None))
    }

    fn drag(value: f32) -> Scalar {
        Scalar::builder()
            .track(Track::RelativeVertical {
                range: 128.0,
                value,
            })
            .hover(Hover::new(CursorShape::ResizeV))
            .build()
    }

    fn resetting(reset: f32) -> Scalar {
        Scalar::builder()
            .track(Track::RelativeVertical {
                range: 128.0,
                value: 0.8,
            })
            .hover(Hover::new(CursorShape::ResizeV))
            .reset(reset)
            .build()
    }

    fn wheel_drag() -> Scalar {
        Scalar::builder()
            .track(Track::RelativeVertical {
                range: 140.0,
                value: 0.5,
            })
            .hover(Hover::new(CursorShape::ResizeV))
            .wheel(WheelStep {
                value: 0.5,
                step: 0.25,
            })
            .build()
    }

    /// The VU meter's area: offset from the origin, so an inverted axis that
    /// forgot to subtract `area.y` would still look right at the top edge.
    fn meter() -> Rect {
        Rect {
            h: 40.0,
            w: 12.0,
            x: 0.0,
            y: 10.0,
        }
    }

    fn on_meter(y: f32) -> Hit {
        Hit::new(Some(Pt { x: 6.0, y }), meter())
    }

    fn seeking() -> Scalar {
        Scalar::builder()
            .track(Track::AbsoluteVertical)
            .hover(Hover::new(CursorShape::ResizeV))
            .build()
    }

    #[kithara::test]
    fn relative_vertical_drag_is_up_positive_and_scaled_by_range() {
        let drag = drag(0.5);
        let now = Instant::now();

        for (from, to, expected) in [(33.0, 1.0, 0.75), (1.0, 33.0, 0.25)] {
            let mut state = ScalarState::default();
            assert_eq!(
                drag.on_input(&mut state, pointer(PointerPhase::Down), &hit(from), now),
                Outcome::captured()
            );
            assert_eq!(
                drag.on_input(&mut state, moved(to), &hit(to), now),
                Outcome::set(expected),
                "{from} -> {to}"
            );
        }
    }

    #[kithara::test]
    fn relative_drag_measures_travel_in_the_pointer_space() {
        let drag = drag(0.5);
        let mut state = ScalarState::default();
        let now = Instant::now();
        let down = Input::Pointer(mouse_input(
            PointerPhase::Down,
            Some(Pt { x: 17.0, y: 50.0 }),
        ));

        assert_eq!(
            drag.on_input(&mut state, down, &hit(10.0), now),
            Outcome::captured()
        );
        assert_eq!(
            drag.on_input(&mut state, moved(18.0), &hit(-22.0), now),
            Outcome::set(0.75)
        );
    }

    #[kithara::test]
    fn relative_vertical_press_captures_without_publishing() {
        let drag = drag(0.5);
        let mut state = ScalarState::default();
        let outcome = drag.on_input(
            &mut state,
            pointer(PointerPhase::Down),
            &hit(17.0),
            Instant::now(),
        );

        assert_eq!(outcome.value(), None, "a relative press seeks nothing");
        assert!(outcome.is_captured());
    }

    #[kithara::test]
    fn a_reset_never_becomes_a_drag() {
        let drag = resetting(0.5);
        let cursor = hit(17.0);
        let mut state = ScalarState::default();
        let now = Instant::now();

        drag.on_input(&mut state, pointer(PointerPhase::Down), &cursor, now);
        drag.on_input(&mut state, pointer(PointerPhase::Up), &cursor, now);
        assert_eq!(
            drag.on_input(&mut state, pointer(PointerPhase::Down), &cursor, now),
            Outcome::set(0.5)
        );
        assert_eq!(
            drag.on_input(&mut state, moved(1.0), &hit(1.0), now),
            Outcome::IGNORED,
            "the press that reset the value must not have armed a drag"
        );
    }

    #[kithara::test]
    fn the_release_after_a_reset_is_not_captured() {
        let drag = resetting(0.5);
        let cursor = hit(17.0);
        let mut state = ScalarState::default();
        let now = Instant::now();

        drag.on_input(&mut state, pointer(PointerPhase::Down), &cursor, now);
        drag.on_input(&mut state, pointer(PointerPhase::Up), &cursor, now);
        drag.on_input(&mut state, pointer(PointerPhase::Down), &cursor, now);

        assert_eq!(
            drag.on_input(&mut state, pointer(PointerPhase::Up), &cursor, now),
            Outcome::IGNORED,
            "no gesture is active, so the release belongs to whoever is behind"
        );
    }

    #[kithara::test]
    fn relative_drag_double_click_resets_to_configured_value() {
        let drag = resetting(0.5);
        let cursor = hit(17.0);
        let mut state = ScalarState::default();
        let now = Instant::now();

        assert_eq!(
            drag.on_input(&mut state, pointer(PointerPhase::Down), &cursor, now),
            Outcome::captured()
        );
        assert_eq!(
            drag.on_input(&mut state, pointer(PointerPhase::Up), &cursor, now),
            Outcome::captured()
        );
        assert_eq!(
            drag.on_input(&mut state, pointer(PointerPhase::Down), &cursor, now),
            Outcome::set(0.5)
        );
    }

    #[kithara::test]
    fn wheel_steps_the_value_by_direction_and_clamps() {
        let drag = wheel_drag();
        let cursor = hit(17.0);
        let mut state = ScalarState::default();
        let now = Instant::now();

        assert_eq!(
            drag.on_input(&mut state, Input::Wheel(Scroll::lines(-1.0)), &cursor, now,),
            Outcome::set(0.75)
        );
        assert_eq!(
            drag.on_input(&mut state, Input::Wheel(Scroll::lines(1.0)), &cursor, now,),
            Outcome::set(0.25)
        );
        assert_eq!(
            drag.on_input(&mut state, Input::Wheel(Scroll::lines(0.0)), &cursor, now,),
            Outcome::captured(),
            "zero delta must still capture over an opted-in control"
        );
        assert_eq!(
            drag.on_input(
                &mut state,
                Input::Wheel(Scroll::lines(1.0)),
                &hit(100.0),
                now,
            ),
            Outcome::IGNORED
        );
    }

    #[kithara::test]
    fn trackpad_pixels_accumulate_to_whole_steps() {
        let drag = wheel_drag();
        let cursor = hit(17.0);
        let mut state = ScalarState::default();
        let now = Instant::now();

        assert_eq!(
            drag.on_input(
                &mut state,
                Input::Wheel(Scroll::pixels(-12.0)),
                &cursor,
                now,
            ),
            Outcome::captured(),
            "sub-threshold pixels capture without publishing"
        );
        assert_eq!(
            drag.on_input(
                &mut state,
                Input::Wheel(Scroll::pixels(-12.0)),
                &cursor,
                now,
            ),
            Outcome::set(0.75)
        );
        assert_eq!(
            drag.on_input(&mut state, Input::Wheel(Scroll::pixels(45.0)), &cursor, now,),
            Outcome::set(0.0)
        );
    }

    #[kithara::test]
    fn an_absolute_press_seeks_the_inverted_position() {
        let seeking = seeking();
        let now = Instant::now();

        // Every fraction here is dyadic, so the equality holds without a residue.
        for (y, expected) in [(10.0, 1.0), (20.0, 0.75), (30.0, 0.5), (40.0, 0.25)] {
            let mut state = ScalarState::default();
            assert_eq!(
                seeking.on_input(&mut state, pointer(PointerPhase::Down), &on_meter(y), now),
                Outcome::set(expected),
                "at y={y}"
            );
        }
    }

    #[kithara::test]
    fn an_absolute_drag_on_zero_height_publishes_nothing() {
        let seeking = seeking();
        let mut state = ScalarState::default();
        let now = Instant::now();
        seeking.on_input(
            &mut state,
            pointer(PointerPhase::Down),
            &on_meter(30.0),
            now,
        );

        let flattened = Hit::new(Some(Pt { x: 6.0, y: 30.0 }), Rect { h: 0.0, ..meter() });

        assert_eq!(
            seeking.on_input(&mut state, moved_on_meter(30.0), &flattened, now),
            Outcome::IGNORED,
            "a degenerate height must publish nothing rather than a clamped zero"
        );
    }

    #[kithara::test]
    fn an_absolute_drag_keeps_publishing_after_the_pointer_leaves() {
        let seeking = seeking();
        let mut state = ScalarState::default();
        let now = Instant::now();
        seeking.on_input(
            &mut state,
            pointer(PointerPhase::Down),
            &on_meter(30.0),
            now,
        );

        assert_eq!(
            seeking.on_input(&mut state, moved_on_meter(200.0), &on_meter(200.0), now),
            Outcome::set(0.0)
        );
    }

    #[kithara::test]
    fn an_absolute_press_seeks_and_captures_and_the_release_is_silent() {
        let seeking = seeking();
        let cursor = on_meter(30.0);
        let mut state = ScalarState::default();
        let now = Instant::now();

        let pressed = seeking.on_input(&mut state, pointer(PointerPhase::Down), &cursor, now);
        let released = seeking.on_input(&mut state, pointer(PointerPhase::Up), &cursor, now);

        assert_eq!(pressed, Outcome::set(0.5));
        assert_eq!(released, Outcome::captured());
    }

    /// A rail offset from the origin, so a normalization that forgot to
    /// subtract `area.x` would still look right at the left edge.
    fn rail() -> Rect {
        Rect {
            h: 40.0,
            w: 200.0,
            x: 20.0,
            y: 4.0,
        }
    }

    fn on_rail(x: f32) -> Hit {
        Hit::new(Some(Pt { x, y: 24.0 }), rail())
    }

    fn across(x: f32) -> Input<'static> {
        Input::Pointer(mouse_input(PointerPhase::Move, Some(Pt { x, y: 24.0 })))
    }

    fn sliding(track: Track) -> Scalar {
        Scalar::builder()
            .track(track)
            .hover(Hover::new(CursorShape::ResizeH))
            .build()
    }

    #[kithara::test]
    fn an_absolute_horizontal_drag_maps_to_the_normalized_position_and_clamps() {
        let slide = sliding(Track::AbsoluteHorizontal);
        let mut state = ScalarState::default();
        let now = Instant::now();

        assert_eq!(
            slide.on_input(
                &mut state,
                pointer(PointerPhase::Down),
                &on_rail(120.0),
                now
            ),
            Outcome::set(0.5),
            "the press seeks, offset by the rail's own origin"
        );
        for (x, expected) in [(20.0, 0.0), (220.0, 1.0), (0.0, 0.0), (260.0, 1.0)] {
            assert_eq!(
                slide.on_input(&mut state, across(x), &on_rail(x), now),
                Outcome::set(expected),
                "at x={x}"
            );
        }
    }

    #[kithara::test]
    fn an_absolute_horizontal_drag_on_zero_width_publishes_nothing() {
        let slide = sliding(Track::AbsoluteHorizontal);
        let mut state = ScalarState::default();
        let now = Instant::now();
        slide.on_input(
            &mut state,
            pointer(PointerPhase::Down),
            &on_rail(120.0),
            now,
        );

        let flattened = Hit::new(Some(Pt { x: 120.0, y: 24.0 }), Rect { w: 0.0, ..rail() });

        assert_eq!(
            slide.on_input(&mut state, across(120.0), &flattened, now),
            Outcome::IGNORED,
            "a degenerate width must publish nothing rather than a clamped zero"
        );
    }

    #[kithara::test]
    fn a_click_track_seeks_once_and_never_arms_a_drag() {
        let click = Scalar::builder()
            .track(Track::HorizontalClick)
            .hover(Hover::new(CursorShape::Pointer))
            .build();
        let mut state = ScalarState::default();
        let now = Instant::now();

        assert_eq!(
            click.on_input(
                &mut state,
                pointer(PointerPhase::Down),
                &on_rail(120.0),
                now
            ),
            Outcome::set(0.5)
        );
        assert_eq!(
            click.on_input(&mut state, across(220.0), &on_rail(220.0), now),
            Outcome::IGNORED,
            "the press seeks without arming, so the move belongs to whoever is behind"
        );
    }

    #[kithara::test]
    fn a_relative_horizontal_drag_walks_back_from_the_start_value() {
        let slide = sliding(Track::RelativeHorizontal {
            scale: 0.2,
            value: 0.4,
        });
        let mut state = ScalarState::default();
        let now = Instant::now();

        assert_eq!(
            slide.on_input(
                &mut state,
                pointer(PointerPhase::Down),
                &on_rail(120.0),
                now
            ),
            Outcome::captured(),
            "a relative press seeks nothing"
        );

        let Some(value) = slide
            .on_input(&mut state, across(170.0), &on_rail(170.0), now)
            .value()
        else {
            panic!("a relative drag must publish its movement");
        };
        // 50 px of a 200 px rail, scaled by 0.2, walked back from 0.4.
        assert!((value - 0.35).abs() < 0.000_1, "got {value}");

        assert_eq!(
            slide.on_input(&mut state, pointer(PointerPhase::Up), &on_rail(170.0), now),
            Outcome::captured()
        );
    }

    #[kithara::test]
    fn a_pixel_drag_counts_from_the_start_width_and_floors_at_the_minimum() {
        let divider = Rect {
            h: 22.0,
            w: 7.0,
            x: 0.0,
            y: 0.0,
        };
        let on_divider = |x: f32| Hit::new(Some(Pt { x, y: 11.0 }), divider);
        let drag = Scalar::builder()
            .track(Track::HorizontalPixels {
                minimum: 28.0,
                value: 180.0,
            })
            .hover(Hover::new(CursorShape::ResizeH))
            .build();
        let mut state = ScalarState::default();
        let now = Instant::now();

        assert_eq!(
            drag.on_input(
                &mut state,
                pointer(PointerPhase::Down),
                &on_divider(3.0),
                now
            ),
            Outcome::captured(),
            "a width drag has no value to seek on the press"
        );
        assert_eq!(
            drag.on_input(
                &mut state,
                Input::Pointer(mouse_input(
                    PointerPhase::Move,
                    Some(Pt { x: 43.0, y: 11.0 }),
                )),
                &on_divider(43.0),
                now,
            ),
            Outcome::set(220.0)
        );
        assert_eq!(
            drag.on_input(
                &mut state,
                Input::Pointer(mouse_input(
                    PointerPhase::Move,
                    Some(Pt { x: -300.0, y: 11.0 }),
                )),
                &on_divider(-300.0),
                now,
            ),
            Outcome::set(28.0),
            "the floor is a width, so it clamps below and not above"
        );
    }

    #[kithara::test]
    fn a_wheel_without_an_opt_in_leaves_the_scroll_to_whoever_is_behind() {
        let seeking = seeking();
        let mut state = ScalarState::default();

        assert_eq!(
            seeking.on_input(
                &mut state,
                Input::Wheel(Scroll::lines(1.0)),
                &on_meter(30.0),
                Instant::now(),
            ),
            Outcome::IGNORED,
            "a control that did not opt in must let the page scroll"
        );
    }

    #[kithara::test]
    fn without_a_reset_the_second_press_is_an_ordinary_press() {
        let seeking = seeking();
        let cursor = on_meter(30.0);
        let mut state = ScalarState::default();
        let now = Instant::now();

        seeking.on_input(&mut state, pointer(PointerPhase::Down), &cursor, now);
        seeking.on_input(&mut state, pointer(PointerPhase::Up), &cursor, now);

        assert_eq!(
            seeking.on_input(&mut state, pointer(PointerPhase::Down), &cursor, now),
            Outcome::set(0.5),
            "the second press seeks again rather than resetting"
        );
        assert_eq!(
            seeking.on_input(&mut state, moved_on_meter(20.0), &on_meter(20.0), now),
            Outcome::set(0.75),
            "and it armed the gesture, so the move that follows still drags"
        );
    }
}
