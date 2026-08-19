use kithara_platform::time::Instant;

use super::{
    component::{PickerSnapshot, RetainedComponent, TextInputSnapshot},
    model::{Descriptor, Emission, Kind, Target},
    router::Router,
};
use crate::interact::{CursorShape, Input, InputMethodRequest, Rect};

#[derive(Default)]
pub(crate) struct Engine {
    components: Vec<RetainedComponent>,
    router: Router,
}

impl Engine {
    pub(crate) fn reconcile(&mut self, descriptors: impl IntoIterator<Item = Descriptor>) {
        let mut retained = std::mem::take(&mut self.components);
        self.components = descriptors
            .into_iter()
            .map(|descriptor| {
                let retained_index = retained.iter().position(|component| {
                    component.path() == descriptor.path() && component.kind() == descriptor.kind()
                });
                match retained_index {
                    Some(index) => retained.remove(index).reconcile(descriptor),
                    None => descriptor.into(),
                }
            })
            .collect();
        self.router.reconcile(&self.components);
    }

    pub(crate) fn handle(
        &mut self,
        input: Input<'_>,
        targets: &[Target<'_>],
        now: Instant,
    ) -> Option<Emission> {
        self.router
            .handle(&mut self.components, input, targets, now)
    }

    pub(crate) fn cursor(&self, targets: &[Target<'_>]) -> CursorShape {
        self.router.cursor(&self.components, targets)
    }

    pub(crate) fn scroll_offset(&self, path: &str) -> Option<f32> {
        self.components
            .iter()
            .find(|component| component.path() == path && component.kind() == Kind::Scroll)
            .and_then(RetainedComponent::scroll_offset)
    }

    #[cfg(feature = "masonry")]
    pub(crate) fn column_divider_value(&self, path: &str) -> Option<f32> {
        self.components
            .iter()
            .find(|component| component.path() == path && component.kind() == Kind::ColumnDivider)
            .and_then(RetainedComponent::column_divider_value)
    }

    pub(crate) fn pressed_item_index(&self, path: &str) -> Option<usize> {
        self.item_pressed(path).flatten()
    }

    /// Whether a hand is pulling an item out of a list this engine drives.
    ///
    /// The gesture never takes the pointer, so a host that delivers events by
    /// hit-testing loses the drag the moment the hand leaves the list. Asking
    /// this is how such a host knows to keep feeding the list its own gesture.
    #[cfg(feature = "masonry")]
    pub(crate) fn holds_item(&self) -> bool {
        self.components
            .iter()
            .any(|component| RetainedComponent::pressed_item_index(component).is_some())
    }

    pub(crate) fn item_pressed(&self, path: &str) -> Option<Option<usize>> {
        self.components
            .iter()
            .find(|component| component.kind() == Kind::Item && component.event_path() == path)
            .map(RetainedComponent::pressed_item_index)
    }

    pub(crate) fn picker_snapshot(&self, path: &str) -> Option<PickerSnapshot> {
        self.components
            .iter()
            .find(|component| component.path() == path && component.kind() == Kind::Picker)
            .and_then(RetainedComponent::picker_snapshot)
    }

    pub(crate) fn text_input_snapshot(&self, path: &str) -> Option<TextInputSnapshot> {
        let focused = self.router.focused_path() == Some(path);
        self.components
            .iter()
            .find(|component| component.path() == path && component.kind() == Kind::TextInput)
            .and_then(|component| component.text_input_snapshot(focused))
    }

    pub(crate) fn text_input_snapshots(&self) -> Vec<(String, TextInputSnapshot)> {
        let focused = self.router.focused_path();
        self.components
            .iter()
            .filter_map(|component| {
                component
                    .text_input_snapshot(focused == Some(component.path()))
                    .map(|snapshot| (component.path().to_owned(), snapshot))
            })
            .collect()
    }

    pub(crate) fn input_method<'a>(
        &'a self,
        targets: &[Target<'_>],
    ) -> Option<InputMethodRequest<'a>> {
        let path = self.router.focused_path()?;
        let component = self
            .components
            .iter()
            .find(|component| component.path() == path && component.kind() == Kind::TextInput)?;
        let area = targets
            .iter()
            .find(|target| target.path == path)?
            .hit
            .area();
        component.input_method(area)
    }

    pub(crate) fn has_pressed_item(&self) -> bool {
        self.components
            .iter()
            .any(|component| component.pressed_item_index().is_some())
    }

    pub(crate) fn set_scroll_viewport(&mut self, path: &str, area: Rect) {
        if let Some(component) = self
            .components
            .iter_mut()
            .find(|component| component.path() == path && component.kind() == Kind::Scroll)
        {
            component.set_scroll_viewport(area);
        }
    }

    #[cfg(feature = "masonry")]
    pub(crate) fn clear_focus(&mut self) {
        self.router.clear_focus(&mut self.components);
    }

    delegate::delegate! {
        to self.router {
            pub(crate) const fn captures_pointer(&self) -> bool;
            pub(crate) fn captures(&self, path: &str) -> bool;
            pub(crate) fn focused_path(&self) -> Option<&str>;
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{super::model::EngineEvent, *};
    use crate::{
        draw::{Pt, Rect},
        engine::ScrollConfig,
        interact::{
            Hit, Hover, InputMethod, Key, Modifiers, Outcome, PointerOwnership, PointerPhase,
            Scroll, ScrollAxis, TextInputLayout, mouse as mouse_input,
            recognizers::{DragEvent, WheelStep},
        },
    };

    fn knob(path: &str, current: f32) -> Descriptor {
        Descriptor::knob(path.to_owned(), current, 100.0, 0.1)
    }

    fn scroll(path: &str) -> Descriptor {
        Descriptor::scroll(
            path.to_owned(),
            ScrollConfig::items(ScrollAxis::Vertical, 200.0, 10, 20.0, 20.0, 8.0),
        )
    }

    fn plain_scroll(path: &str, axis: ScrollAxis) -> Descriptor {
        Descriptor::scroll(path.to_owned(), ScrollConfig::plain(axis, 200.0))
    }

    fn text_input(path: &str, query: &str) -> Descriptor {
        Descriptor::text_input(
            path.to_owned(),
            query.to_owned(),
            TextInputLayout::new([(0, 4.0), (1, 14.0), (2, 30.0)], 3.0, 12.0, 12.0),
        )
    }

    fn target(path: &str, x: f32, y: f32) -> Target<'_> {
        Target::new(
            path,
            Hit::new(
                Some(Pt { x, y }),
                Rect {
                    h: 100.0,
                    w: 100.0,
                    x: 0.0,
                    y: 0.0,
                },
            ),
        )
    }

    fn text_target(path: &str, x: f32, origin_x: f32, origin_y: f32) -> Target<'_> {
        Target::new(
            path,
            Hit::new(
                Some(Pt {
                    x,
                    y: origin_y + 8.0,
                }),
                Rect {
                    h: 24.0,
                    w: 100.0,
                    x: origin_x,
                    y: origin_y,
                },
            ),
        )
    }

    fn item_target(path: &str, index: usize, x: f32, y: f32) -> Target<'_> {
        let target = target(path, x, y);
        Target::item(path, target.hit, index)
    }

    fn picker_target(path: &str, index: Option<usize>, pointer_y: f32, top: f32) -> Target<'_> {
        let hit = Hit::new(
            Some(Pt {
                x: 50.0,
                y: pointer_y,
            }),
            Rect {
                h: 20.0,
                w: 100.0,
                x: 0.0,
                y: top,
            },
        );
        index.map_or_else(
            || Target::new(path, hit),
            |index| Target::item(path, hit, index),
        )
    }

    fn axis_lines(axis: ScrollAxis, delta: f32) -> Scroll {
        match axis {
            ScrollAxis::Horizontal => Scroll::Lines { x: delta, y: 0.0 },
            ScrollAxis::Vertical => Scroll::Lines { x: 0.0, y: delta },
        }
    }

    fn axis_pixels(axis: ScrollAxis, delta: f32) -> Scroll {
        match axis {
            ScrollAxis::Horizontal => Scroll::Pixels { x: delta, y: 0.0 },
            ScrollAxis::Vertical => Scroll::Pixels { x: 0.0, y: delta },
        }
    }

    fn key_pressed(key: Key<'static>) -> Input<'static> {
        Input::KeyPressed {
            key,
            modifiers: Modifiers::default(),
            text: None,
        }
    }

    fn key_released(key: Key<'static>) -> Input<'static> {
        Input::KeyReleased {
            key,
            modifiers: Modifiers::default(),
        }
    }

    fn typed(text: &'static str) -> Input<'static> {
        Input::KeyPressed {
            key: Key::Character(text),
            modifiers: Modifiers::default(),
            text: Some(text),
        }
    }

    fn shifted(key: Key<'static>) -> Input<'static> {
        Input::KeyPressed {
            key,
            modifiers: Modifiers::new(false, false, false, true),
            text: None,
        }
    }

    fn pointer_input(phase: PointerPhase, at: Option<Pt>) -> Input<'static> {
        Input::Pointer(mouse_input(phase, at))
    }

    fn value(emission: Option<Emission>) -> Option<f64> {
        emission.and_then(|emission| match emission.outcome.value() {
            Some(EngineEvent::Scalar(value)) => Some(value),
            Some(
                EngineEvent::Activate
                | EngineEvent::Crossing(_)
                | EngineEvent::Index(_)
                | EngineEvent::Drag { .. }
                | EngineEvent::Text(_),
            )
            | None => None,
        })
    }

    #[kithara::test]
    fn fader_quantizes_the_wide_value_at_iced_step_boundaries() {
        let path = "gallery/faders/default";
        let observed = [29.0, 30.0, 31.0].map(|x| {
            let mut engine = Engine::default();
            engine.reconcile([Descriptor::fader(
                path.to_owned(),
                Hover::new(CursorShape::Grab),
                Some(0.01),
                None,
            )]);
            value(engine.handle(
                pointer_input(PointerPhase::Down, None),
                &[Target::new(
                    path,
                    Hit::new(
                        Some(Pt { x, y: 8.0 }),
                        Rect {
                            h: 16.0,
                            w: 200.0,
                            x: 0.0,
                            y: 0.0,
                        },
                    ),
                )],
                Instant::now(),
            ))
        });

        assert_eq!(observed, [Some(0.14), Some(0.15), Some(0.16)]);
    }

    #[kithara::test]
    fn volume_fader_keeps_drag_continuous_and_wheel_step_separate() {
        let path = "gallery/faders/volume";
        let mut engine = Engine::default();
        engine.reconcile([Descriptor::fader(
            path.to_owned(),
            Hover::new(CursorShape::ResizeH),
            None,
            Some(WheelStep {
                value: 0.5,
                step: 0.01,
            }),
        )]);
        let targets = [Target::new(
            path,
            Hit::new(
                Some(Pt { x: 29.0, y: 7.0 }),
                Rect {
                    h: 14.0,
                    w: 200.0,
                    x: 0.0,
                    y: 0.0,
                },
            ),
        )];

        assert_eq!(
            value(engine.handle(
                pointer_input(PointerPhase::Down, None),
                &targets,
                Instant::now()
            )),
            Some(f64::from(29.0_f32 / 200.0_f32))
        );
        assert_eq!(
            value(engine.handle(Input::Wheel(Scroll::lines(1.0)), &targets, Instant::now(),)),
            Some(f64::from(0.49_f32))
        );
    }

    #[kithara::test]
    fn activation_publishes_once_on_press() {
        let mut engine = Engine::default();
        let path = "gallery/toggles/enabled";
        engine.reconcile([Descriptor::activation(path.to_owned())]);

        let press = engine
            .handle(
                pointer_input(PointerPhase::Down, None),
                &[target(path, 50.0, 50.0)],
                Instant::now(),
            )
            .map(|emission| {
                let captured = emission.outcome.is_captured();
                (emission.outcome.value(), captured)
            });
        assert_eq!(press, Some((Some(EngineEvent::Activate), true)));

        assert!(
            engine
                .handle(
                    pointer_input(PointerPhase::Move, Some(Pt { x: 55.0, y: 50.0 })),
                    &[target(path, 55.0, 50.0)],
                    Instant::now(),
                )
                .is_none()
        );
        assert!(
            engine
                .handle(
                    pointer_input(PointerPhase::Up, None),
                    &[target(path, 55.0, 50.0)],
                    Instant::now(),
                )
                .is_none()
        );
    }

    #[kithara::test]
    fn activation_press_that_misses_publishes_nothing() {
        let mut engine = Engine::default();
        let path = "gallery/toggles/enabled";
        engine.reconcile([Descriptor::activation(path.to_owned())]);

        assert!(
            engine
                .handle(
                    pointer_input(PointerPhase::Down, None),
                    &[target(path, 150.0, 50.0)],
                    Instant::now(),
                )
                .is_none()
        );
    }

    #[kithara::test]
    fn crossing_publishes_once_per_boundary_and_survives_reconciliation() {
        let mut engine = Engine::default();
        let path = "deck-a/drop";
        engine.reconcile([Descriptor::crossing(path.to_owned())]);

        let enter = engine
            .handle(
                pointer_input(PointerPhase::Move, Some(Pt { x: 50.0, y: 50.0 })),
                &[target(path, 50.0, 50.0)],
                Instant::now(),
            )
            .unwrap_or_else(|| panic!("entering the drop zone must publish"));
        assert_eq!(enter.path, path);
        assert_eq!(enter.child, None);
        assert!(!enter.outcome.is_captured());
        assert_eq!(enter.outcome.value(), Some(EngineEvent::Crossing(true)));
        assert!(!engine.captures_pointer());

        engine.reconcile([Descriptor::crossing(path.to_owned())]);
        assert!(
            engine
                .handle(
                    pointer_input(PointerPhase::Move, Some(Pt { x: 55.0, y: 50.0 })),
                    &[target(path, 55.0, 50.0)],
                    Instant::now(),
                )
                .is_none(),
            "reconciliation must not turn an inside move into another entry"
        );

        let leave = engine
            .handle(
                pointer_input(PointerPhase::Leave, None),
                &[target(path, 55.0, 50.0)],
                Instant::now(),
            )
            .unwrap_or_else(|| panic!("leaving the drop zone must publish"));
        assert!(!leave.outcome.is_captured());
        assert_eq!(leave.outcome.value(), Some(EngineEvent::Crossing(false)));
        assert!(!engine.captures_pointer());
        assert!(
            engine
                .handle(
                    pointer_input(PointerPhase::Leave, None),
                    &[target(path, 55.0, 50.0)],
                    Instant::now(),
                )
                .is_none()
        );
    }

    #[kithara::test]
    fn segmented_publishes_the_uniform_cell_index_on_press() {
        let mut engine = Engine::default();
        let path = "cells/beat";
        let area = Rect {
            h: 20.0,
            w: 100.0,
            x: 10.0,
            y: 20.0,
        };
        engine.reconcile([Descriptor::segmented(path.to_owned(), 4)]);

        assert_eq!(
            engine.cursor(&[Target::new(
                path,
                Hit::new(Some(Pt { x: 10.0, y: 30.0 }), area),
            )]),
            CursorShape::Pointer
        );

        for (x, expected) in [(10.0, 0), (34.0, 0), (35.0, 1), (109.0, 3)] {
            let emission = engine
                .handle(
                    pointer_input(PointerPhase::Down, None),
                    &[Target::new(path, Hit::new(Some(Pt { x, y: 30.0 }), area))],
                    Instant::now(),
                )
                .unwrap_or_else(|| panic!("a segmented press must publish its cell index"));
            assert_eq!(emission.outcome.value(), Some(EngineEvent::Index(expected)));
            assert!(!engine.captures_pointer());
        }

        assert!(
            engine
                .handle(
                    pointer_input(PointerPhase::Down, None),
                    &[Target::new(
                        path,
                        Hit::new(Some(Pt { x: 110.0, y: 30.0 }), area),
                    )],
                    Instant::now(),
                )
                .is_none()
        );
    }

    #[kithara::test]
    fn retained_item_drag_keeps_its_target_identity_and_publishes_the_row_index() {
        let mut engine = Engine::default();
        let path = "library/tracks";
        let target_path = "library/tracks/rows";
        let now = Instant::now();
        engine.reconcile([Descriptor::item(target_path.to_owned(), path.to_owned(), 4)]);

        assert_eq!(
            engine
                .handle(
                    pointer_input(PointerPhase::Down, None),
                    &[item_target(target_path, 3, 10.0, 13.0)],
                    now,
                )
                .map(|emission| emission.outcome),
            Some(Outcome::captured())
        );
        assert_eq!(engine.pressed_item_index(path), Some(3));
        assert!(
            engine
                .handle(
                    pointer_input(PointerPhase::Move, Some(Pt { x: 11.0, y: 13.0 })),
                    &[item_target(target_path, 3, 11.0, 13.0)],
                    now,
                )
                .is_none()
        );
        let started = engine
            .handle(
                pointer_input(PointerPhase::Move, Some(Pt { x: 40.0, y: 13.0 })),
                &[item_target(target_path, 3, 40.0, 13.0)],
                now,
            )
            .unwrap_or_else(|| panic!("crossing the threshold must start the row drag"));

        assert_eq!(started.path, path);
        assert_eq!(
            started.outcome,
            Outcome::observed(EngineEvent::Drag {
                event: DragEvent::Started,
                index: 3,
            })
        );
        assert!(!engine.captures_pointer());

        let dropped = engine
            .handle(
                pointer_input(PointerPhase::Up, None),
                &[target(target_path, 200.0, 200.0)],
                now,
            )
            .unwrap_or_else(|| panic!("the row watcher must publish after its hit leaves view"));
        assert_eq!(
            dropped.outcome,
            Outcome::observed(EngineEvent::Drag {
                event: DragEvent::Dropped,
                index: 3,
            })
        );
        assert_eq!(engine.pressed_item_index(path), None);
    }

    #[kithara::test]
    fn retained_item_selects_on_release_without_starting_a_drag() {
        let mut engine = Engine::default();
        let path = "library/tracks";
        let target_path = "library/tracks/rows";
        let now = Instant::now();
        engine.reconcile([Descriptor::item(target_path.to_owned(), path.to_owned(), 4)]);

        let _ = engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[item_target(target_path, 2, 10.0, 13.0)],
            now,
        );
        let selected = engine
            .handle(
                pointer_input(PointerPhase::Up, None),
                &[item_target(target_path, 2, 10.0, 13.0)],
                now,
            )
            .unwrap_or_else(|| panic!("a plain row release must publish its index"));

        assert_eq!(selected.path, path);
        assert_eq!(selected.outcome, Outcome::set(EngineEvent::Index(2)));
        assert_eq!(engine.pressed_item_index(path), None);
    }

    #[kithara::test]
    fn retained_item_cancels_a_held_index_removed_by_reconciliation() {
        let mut engine = Engine::default();
        let path = "library/tracks";
        let target_path = "library/tracks/rows";
        let now = Instant::now();
        engine.reconcile([Descriptor::item(target_path.to_owned(), path.to_owned(), 4)]);
        let _ = engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[item_target(target_path, 3, 10.0, 13.0)],
            now,
        );

        engine.reconcile([Descriptor::item(target_path.to_owned(), path.to_owned(), 3)]);
        assert_eq!(engine.pressed_item_index(path), None);
        assert!(
            engine
                .handle(
                    pointer_input(PointerPhase::Move, Some(Pt { x: 40.0, y: 13.0 })),
                    &[Target::new(
                        target_path,
                        Hit::new(
                            None,
                            Rect {
                                h: 0.0,
                                w: 0.0,
                                x: 0.0,
                                y: 0.0,
                            },
                        ),
                    )],
                    now,
                )
                .is_none()
        );
    }

    #[kithara::test]
    fn retained_item_cancel_clears_its_held_index() {
        let mut engine = Engine::default();
        let path = "library/tracks";
        let target_path = "library/tracks/rows";
        let now = Instant::now();
        engine.reconcile([Descriptor::item(target_path.to_owned(), path.to_owned(), 4)]);
        let _ = engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[item_target(target_path, 2, 10.0, 13.0)],
            now,
        );
        assert_eq!(engine.pressed_item_index(path), Some(2));

        let _ = engine.handle(
            pointer_input(PointerPhase::Cancel, None),
            &[item_target(target_path, 2, 10.0, 13.0)],
            now,
        );

        assert_eq!(engine.pressed_item_index(path), None);
        assert!(
            engine
                .handle(
                    pointer_input(PointerPhase::Move, Some(Pt { x: 40.0, y: 13.0 })),
                    &[item_target(target_path, 2, 40.0, 13.0)],
                    now,
                )
                .is_none(),
            "a cancelled row must not resume its former drag"
        );
    }

    #[kithara::test]
    fn column_divider_uses_the_wide_hit_rect_and_publishes_pixel_width() {
        let mut engine = Engine::default();
        let path = "library/tracks/width/deck";
        let now = Instant::now();
        let hit_rect = Rect {
            h: 22.0,
            w: 7.0,
            x: 100.0,
            y: 0.0,
        };
        let divider = Target::new(path, Hit::new(Some(Pt { x: 100.5, y: 11.0 }), hit_rect));
        engine.reconcile([Descriptor::column_divider(path.to_owned(), 64.0, 28.0)]);

        assert_eq!(divider.hit.area(), hit_rect);
        assert_eq!(
            engine
                .handle(pointer_input(PointerPhase::Down, None), &[divider], now)
                .map(|emission| emission.outcome),
            Some(Outcome::captured().with_ownership(PointerOwnership::Claim)),
            "the outer half-pixel of the seven-pixel grab area must arm the drag"
        );
        assert_eq!(
            value(engine.handle(
                pointer_input(PointerPhase::Move, Some(Pt { x: 140.5, y: 11.0 })),
                &[divider],
                now,
            )),
            Some(104.0)
        );
    }

    #[kithara::test]
    fn activation_does_not_take_the_capture_slot() {
        let mut engine = Engine::default();
        let toggle = "gallery/toggles/enabled";
        let meter = "gallery/meters/level";
        engine.reconcile([
            Descriptor::vertical_vu(meter.to_owned()),
            Descriptor::activation(toggle.to_owned()),
        ]);

        let activation = engine
            .handle(
                pointer_input(PointerPhase::Down, None),
                &[target(toggle, 50.0, 50.0)],
                Instant::now(),
            )
            .map(|emission| emission.outcome.value());
        assert_eq!(activation, Some(Some(EngineEvent::Activate)));
        assert!(
            !engine.captures_pointer(),
            "a captured activation outcome must not occupy the router capture slot"
        );

        let scalar = engine
            .handle(
                pointer_input(PointerPhase::Down, None),
                &[target(meter, 50.0, 25.0)],
                Instant::now(),
            )
            .map(|emission| emission.outcome.value());
        assert_eq!(scalar, Some(Some(EngineEvent::Scalar(0.75))));
    }

    #[kithara::test]
    fn click_wave_seeks_on_press_without_holding_capture() {
        let mut engine = Engine::default();
        let wave = "overview/a/wave";
        let meter = "gallery/meters/level";
        engine.reconcile([
            Descriptor::vertical_vu(meter.to_owned()),
            Descriptor::wave(wave.to_owned()),
        ]);

        let press = engine
            .handle(
                pointer_input(PointerPhase::Down, None),
                &[target(wave, 25.0, 50.0)],
                Instant::now(),
            )
            .map(|emission| {
                let captured = emission.outcome.is_captured();
                (emission.path, emission.outcome.value(), captured)
            });
        assert_eq!(
            press,
            Some((wave.to_owned(), Some(EngineEvent::Scalar(0.25)), true,))
        );
        assert!(
            !engine.captures_pointer(),
            "a click wave answers the press without retaining the pointer"
        );

        let next = engine
            .handle(
                pointer_input(PointerPhase::Down, None),
                &[target(meter, 50.0, 25.0)],
                Instant::now(),
            )
            .map(|emission| (emission.path, emission.outcome.value()));
        assert_eq!(
            next,
            Some((meter.to_owned(), Some(EngineEvent::Scalar(0.75)),))
        );
    }

    #[kithara::test]
    fn hero_wave_shift_drag_publishes_child_endpoints_and_releases_capture() {
        let mut engine = Engine::default();
        let path = "deck-a/wave";
        let now = Instant::now();
        engine.reconcile([Descriptor::hero_wave(
            path.to_owned(),
            0.5,
            0.5,
            0.25..0.75,
            0.625,
            0.4,
        )]);

        assert!(
            engine
                .handle(
                    Input::ModifiersChanged(Modifiers::new(false, false, false, true)),
                    &[target(path, 25.0, 50.0)],
                    now,
                )
                .is_none()
        );
        let start = engine
            .handle(
                pointer_input(PointerPhase::Down, None),
                &[target(path, 25.0, 50.0)],
                now,
            )
            .unwrap_or_else(|| panic!("a shifted press must publish the loop start"));
        assert_eq!(start.child, Some("loop_start"));
        assert_eq!(start.outcome.value(), Some(EngineEvent::Scalar(0.375)));
        assert!(engine.captures_pointer());

        let end = engine
            .handle(
                pointer_input(PointerPhase::Move, Some(Pt { x: 75.0, y: 50.0 })),
                &[target(path, 75.0, 50.0)],
                now,
            )
            .unwrap_or_else(|| panic!("a shifted drag must publish the loop end"));
        assert_eq!(end.child, Some("loop_end"));
        assert_eq!(end.outcome.value(), Some(EngineEvent::Scalar(0.625)));
        assert!(engine.captures_pointer());

        let release = engine
            .handle(
                pointer_input(PointerPhase::Up, None),
                &[target(path, 75.0, 50.0)],
                now,
            )
            .unwrap_or_else(|| panic!("the loop release must finish the gesture"));
        assert_eq!(release.child, None);
        assert_eq!(
            release.outcome,
            Outcome::captured().with_ownership(PointerOwnership::Release)
        );
        assert!(!engine.captures_pointer());
    }

    #[kithara::test]
    fn cancel_clears_the_router_slot_and_the_component_gesture() {
        let mut engine = Engine::default();
        let path = "studio/gain";
        let now = Instant::now();
        engine.reconcile([knob(path, 0.5)]);

        let _ = engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[target(path, 50.0, 50.0)],
            now,
        );
        assert!(engine.captures_pointer());

        let cancel = engine
            .handle(
                pointer_input(PointerPhase::Cancel, None),
                &[target(path, 50.0, 50.0)],
                now,
            )
            .unwrap_or_else(|| panic!("cancel must release the retained component"));
        assert_eq!(
            cancel.outcome,
            Outcome::IGNORED.with_ownership(PointerOwnership::Release)
        );
        assert!(!engine.captures_pointer());
        assert!(
            engine
                .handle(
                    pointer_input(PointerPhase::Move, Some(Pt { x: 50.0, y: 0.0 })),
                    &[target(path, 50.0, 0.0)],
                    now,
                )
                .is_none(),
            "a hit-tested move after cancel must not resume the stale drag"
        );
    }

    #[kithara::test]
    fn hero_wave_refreshes_its_plain_drag_and_keeps_grip_outside_bounds() {
        let mut engine = Engine::default();
        let wave = "deck-a/wave";
        let meter = "gallery/meters/level";
        engine.reconcile([
            Descriptor::vertical_vu(meter.to_owned()),
            Descriptor::hero_wave(wave.to_owned(), 0.5, 0.75, 0.5..1.0, 0.5, 0.4),
        ]);

        let press = engine
            .handle(
                pointer_input(PointerPhase::Down, None),
                &[target(wave, 50.0, 50.0)],
                Instant::now(),
            )
            .map(|emission| (emission.path, emission.outcome));
        assert_eq!(
            press,
            Some((
                wave.to_owned(),
                Outcome::captured().with_ownership(PointerOwnership::Claim)
            ))
        );
        assert!(engine.captures_pointer());

        engine.reconcile([
            Descriptor::vertical_vu(meter.to_owned()),
            Descriptor::hero_wave(wave.to_owned(), 0.25, 0.0, 0.0..0.25, 0.3, 0.2),
        ]);
        let moved = engine
            .handle(
                pointer_input(PointerPhase::Move, Some(Pt { x: 150.0, y: 50.0 })),
                &[target(wave, 150.0, 50.0), target(meter, 50.0, 25.0)],
                Instant::now(),
            )
            .map(|emission| (emission.path, emission.outcome.value()));
        assert_eq!(
            moved,
            Some((wave.to_owned(), Some(EngineEvent::Scalar(0.5))))
        );
    }

    #[kithara::test]
    fn hero_wave_wheel_publishes_zoom_without_holding_capture() {
        let mut engine = Engine::default();
        let path = "deck-a/wave";
        engine.reconcile([Descriptor::hero_wave(
            path.to_owned(),
            0.5,
            0.5,
            0.25..0.75,
            0.625,
            0.4,
        )]);

        for (delta, expected) in [(1.0, 0.625_f32), (-1.0, 0.4), (0.0, 0.4)] {
            let emission = engine
                .handle(
                    Input::Wheel(Scroll::lines(delta)),
                    &[target(path, 50.0, 50.0)],
                    Instant::now(),
                )
                .unwrap_or_else(|| panic!("a hero wave wheel must publish zoom"));
            assert_eq!(emission.child, Some("zoom"));
            assert_eq!(
                emission.outcome.value(),
                Some(EngineEvent::Scalar(f64::from(expected)))
            );
            assert!(!engine.captures_pointer());
        }
    }

    #[kithara::test]
    fn changing_wave_style_rebuilds_state_and_clears_hero_capture() {
        let mut engine = Engine::default();
        let path = "deck-a/wave";
        let now = Instant::now();
        engine.reconcile([Descriptor::hero_wave(
            path.to_owned(),
            0.5,
            0.5,
            0.25..0.75,
            0.625,
            0.4,
        )]);
        engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[target(path, 50.0, 50.0)],
            now,
        );
        assert!(engine.captures_pointer());

        engine.reconcile([Descriptor::wave(path.to_owned())]);

        assert!(!engine.captures_pointer());
        assert!(
            engine
                .handle(
                    pointer_input(PointerPhase::Move, Some(Pt { x: 75.0, y: 50.0 })),
                    &[target(path, 75.0, 50.0)],
                    now,
                )
                .is_none(),
            "ordinary Wave must not retain HeroWave's plain-drag state"
        );
    }

    #[kithara::test]
    fn reconciliation_refreshes_config_and_retains_an_active_drag() {
        let mut engine = Engine::default();
        let now = Instant::now();
        engine.reconcile([knob("studio/gain", 0.25)]);

        let press = engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[target("studio/gain", 50.0, 50.0)],
            now,
        );
        assert_eq!(
            press.map(|emission| emission.outcome),
            Some(Outcome::captured().with_ownership(PointerOwnership::Claim))
        );

        engine.reconcile([Descriptor::knob("studio/gain".to_owned(), 0.9, 200.0, 0.2)]);
        assert_eq!(
            value(engine.handle(
                pointer_input(PointerPhase::Move, Some(Pt { x: 50.0, y: 0.0 })),
                &[target("studio/gain", 50.0, 0.0)],
                now,
            )),
            Some(0.5),
            "the retained start value combines with the refreshed drag range"
        );
    }

    #[kithara::test]
    fn fader_reconciliation_refreshes_the_wide_drag_step_without_losing_capture() {
        let mut engine = Engine::default();
        let path = "gallery/faders/default";
        let now = Instant::now();
        engine.reconcile([Descriptor::fader(
            path.to_owned(),
            Hover::new(CursorShape::Grab),
            Some(0.01),
            None,
        )]);
        let _ = engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[target(path, 29.0, 50.0)],
            now,
        );
        assert!(engine.captures_pointer());

        engine.reconcile([Descriptor::fader(
            path.to_owned(),
            Hover::new(CursorShape::Grab),
            Some(0.2),
            None,
        )]);

        assert!(engine.captures_pointer());
        assert_eq!(
            value(engine.handle(
                pointer_input(PointerPhase::Move, Some(Pt { x: 31.0, y: 50.0 })),
                &[target(path, 31.0, 50.0)],
                now,
            )),
            Some(0.4)
        );
    }

    #[kithara::test]
    fn a_kind_change_rebuilds_state_and_clears_the_captured_identity() {
        let mut engine = Engine::default();
        let now = Instant::now();
        engine.reconcile([knob("studio/level", 0.5)]);
        engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[target("studio/level", 50.0, 50.0)],
            now,
        );

        engine.reconcile([Descriptor::vertical_vu("studio/level".to_owned())]);
        let emission = engine
            .handle(
                pointer_input(PointerPhase::Down, None),
                &[target("studio/level", 50.0, 25.0)],
                now,
            )
            .map(|emission| {
                let value = match emission.outcome.value() {
                    Some(EngineEvent::Scalar(value)) => Some(value),
                    Some(
                        EngineEvent::Activate
                        | EngineEvent::Crossing(_)
                        | EngineEvent::Index(_)
                        | EngineEvent::Drag { .. }
                        | EngineEvent::Text(_),
                    )
                    | None => None,
                };
                (emission.path, value)
            });

        assert_eq!(emission, Some(("studio/level".to_owned(), Some(0.75))));
    }

    #[kithara::test]
    fn topmost_non_ignored_component_handles_input_first() {
        let mut engine = Engine::default();
        engine.reconcile([
            knob("studio/back", 0.5),
            Descriptor::vertical_vu("studio/front".to_owned()),
        ]);

        let emission = engine
            .handle(
                pointer_input(PointerPhase::Down, None),
                &[
                    target("studio/back", 50.0, 25.0),
                    target("studio/front", 50.0, 25.0),
                ],
                Instant::now(),
            )
            .map(|emission| emission.path);

        assert_eq!(emission.as_deref(), Some("studio/front"));
    }

    #[kithara::test]
    fn capture_holder_routes_exclusively_until_release() {
        let mut engine = Engine::default();
        let now = Instant::now();
        engine.reconcile([
            knob("studio/back", 0.5),
            Descriptor::vertical_vu("studio/front".to_owned()),
        ]);
        engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[
                target("studio/back", 50.0, 25.0),
                target("studio/front", 50.0, 25.0),
            ],
            now,
        );

        let moved = engine
            .handle(
                pointer_input(PointerPhase::Move, Some(Pt { x: 50.0, y: 125.0 })),
                &[
                    target("studio/front", 50.0, 125.0),
                    target("studio/back", 50.0, 50.0),
                ],
                now,
            )
            .map(|emission| emission.path);
        assert_eq!(moved.as_deref(), Some("studio/front"));

        engine.handle(
            pointer_input(PointerPhase::Up, None),
            &[target("studio/front", 50.0, 125.0)],
            now,
        );
        assert!(!engine.captures_pointer());
        let next = engine
            .handle(
                pointer_input(PointerPhase::Down, None),
                &[
                    target("studio/front", 50.0, 50.0),
                    target("studio/back", 50.0, 50.0),
                ],
                now,
            )
            .map(|emission| emission.path);
        assert_eq!(next.as_deref(), Some("studio/back"));
    }

    #[kithara::test]
    fn captured_outcome_does_not_persist_capture_without_active_state() {
        let mut engine = Engine::default();
        let now = Instant::now();
        engine.reconcile([knob("studio/back", 0.5), knob("studio/front", 0.5)]);

        let wheel = engine.handle(
            Input::Wheel(Scroll::lines(0.0)),
            &[
                target("studio/back", 50.0, 50.0),
                target("studio/front", 50.0, 50.0),
            ],
            now,
        );
        assert_eq!(
            wheel.map(|emission| emission.outcome),
            Some(Outcome::captured())
        );
        assert!(
            !engine.captures_pointer(),
            "a wheel outcome must not occupy the router capture slot"
        );

        let next = engine
            .handle(
                pointer_input(PointerPhase::Down, None),
                &[
                    target("studio/front", 50.0, 50.0),
                    target("studio/back", 50.0, 50.0),
                ],
                now,
            )
            .map(|emission| emission.path);
        assert_eq!(next.as_deref(), Some("studio/back"));
    }

    #[kithara::test]
    fn innermost_scroll_that_can_move_consumes_the_wheel() {
        let mut engine = Engine::default();
        let now = Instant::now();
        engine.reconcile([scroll("outer"), scroll("inner")]);

        let emission = engine
            .handle(
                Input::Wheel(Scroll::lines(-1.0)),
                &[target("outer", 50.0, 50.0), target("inner", 50.0, 50.0)],
                now,
            )
            .unwrap_or_else(|| panic!("the innermost movable scroll must consume the wheel"));

        assert_eq!(emission.path, "inner");
        assert_eq!(emission.outcome, Outcome::captured());
        assert_eq!(engine.scroll_offset("inner"), Some(60.0));
        assert_eq!(engine.scroll_offset("outer"), Some(0.0));
    }

    #[kithara::test]
    fn vertical_scroll_passes_a_horizontal_wheel_to_the_outer_horizontal_scroll() {
        let mut engine = Engine::default();
        let now = Instant::now();
        engine.reconcile([
            plain_scroll("table/scroll-x", ScrollAxis::Horizontal),
            plain_scroll("table", ScrollAxis::Vertical),
        ]);

        let emission = engine
            .handle(
                Input::Wheel(Scroll::Lines { x: -1.0, y: 0.0 }),
                &[
                    target("table/scroll-x", 50.0, 50.0),
                    target("table", 50.0, 50.0),
                ],
                now,
            )
            .unwrap_or_else(|| panic!("the outer horizontal scroll must consume the wheel"));

        assert_eq!(emission.path, "table/scroll-x");
        assert_eq!(emission.outcome, Outcome::captured());
        assert_eq!(engine.scroll_offset("table"), Some(0.0));
        assert_eq!(engine.scroll_offset("table/scroll-x"), Some(60.0));
    }

    #[kithara::test]
    fn each_scroll_axis_consumes_both_directions_only_while_it_can_move() {
        for axis in [ScrollAxis::Horizontal, ScrollAxis::Vertical] {
            let mut engine = Engine::default();
            let now = Instant::now();
            let path = match axis {
                ScrollAxis::Horizontal => "horizontal",
                ScrollAxis::Vertical => "vertical",
            };
            let target = target(path, 50.0, 50.0);
            engine.reconcile([plain_scroll(path, axis)]);

            assert!(
                engine
                    .handle(Input::Wheel(axis_lines(axis, 1.0)), &[target], now)
                    .is_none(),
                "a wheel past the leading boundary must remain ignored for {axis:?}"
            );
            let toward_end = engine
                .handle(Input::Wheel(axis_lines(axis, -1.0)), &[target], now)
                .unwrap_or_else(|| panic!("{axis:?} must consume travel toward its end"));
            assert_eq!(toward_end.outcome, Outcome::captured());

            let _ = engine.handle(Input::Wheel(axis_pixels(axis, -1_000.0)), &[target], now);
            assert_eq!(engine.scroll_offset(path), Some(100.0));
            assert!(
                engine
                    .handle(Input::Wheel(axis_pixels(axis, -1.0)), &[target], now)
                    .is_none(),
                "a wheel past the trailing boundary must remain ignored for {axis:?}"
            );
            let toward_start = engine
                .handle(Input::Wheel(axis_lines(axis, 1.0)), &[target], now)
                .unwrap_or_else(|| panic!("{axis:?} must consume travel toward its start"));
            assert_eq!(toward_start.outcome, Outcome::captured());
            assert_eq!(engine.scroll_offset(path), Some(40.0));
        }
    }

    #[kithara::test]
    fn scroll_outside_the_hit_is_ignored() {
        let mut engine = Engine::default();
        let path = "tree/browser";
        engine.reconcile([scroll(path)]);

        assert!(
            engine
                .handle(
                    Input::Wheel(Scroll::lines(-1.0)),
                    &[target(path, 150.0, 150.0)],
                    Instant::now(),
                )
                .is_none()
        );
        assert_eq!(engine.scroll_offset(path), Some(0.0));
    }

    #[kithara::test]
    fn wheel_continues_outward_when_the_inner_scroll_is_at_its_boundary() {
        let mut engine = Engine::default();
        let now = Instant::now();
        engine.reconcile([scroll("outer"), scroll("inner")]);
        let inner = target("inner", 50.0, 50.0);

        let _ = engine.handle(Input::Wheel(Scroll::pixels(-1_000.0)), &[inner], now);
        assert_eq!(engine.scroll_offset("inner"), Some(100.0));

        let emission = engine
            .handle(
                Input::Wheel(Scroll::pixels(-10.0)),
                &[target("outer", 50.0, 50.0), inner],
                now,
            )
            .unwrap_or_else(|| panic!("the movable outer scroll must receive the wheel"));

        assert_eq!(emission.path, "outer");
        assert_eq!(emission.outcome, Outcome::captured());
        assert_eq!(engine.scroll_offset("inner"), Some(100.0));
        assert_eq!(engine.scroll_offset("outer"), Some(10.0));
    }

    #[kithara::test]
    fn bottom_scroll_ignores_downward_wheel_but_consumes_upward_wheel() {
        let mut engine = Engine::default();
        let now = Instant::now();
        let path = "tree/browser";
        let target = target(path, 50.0, 50.0);
        engine.reconcile([scroll(path)]);

        let down = engine
            .handle(Input::Wheel(Scroll::pixels(-1_000.0)), &[target], now)
            .unwrap_or_else(|| panic!("the scroll must consume travel to the bottom"));
        assert_eq!(down.outcome, Outcome::captured());
        assert_eq!(engine.scroll_offset(path), Some(100.0));

        assert!(
            engine
                .handle(Input::Wheel(Scroll::pixels(-1.0)), &[target], now)
                .is_none(),
            "a downward wheel at the bottom must remain ignored"
        );

        let up = engine
            .handle(Input::Wheel(Scroll::lines(1.0)), &[target], now)
            .unwrap_or_else(|| panic!("an upward wheel at the bottom must be consumed"));
        assert_eq!(up.outcome, Outcome::captured());
        assert_eq!(engine.scroll_offset(path), Some(40.0));
    }

    #[kithara::test]
    fn scrollbar_lane_does_not_activate_the_row_behind_it() {
        let mut engine = Engine::default();
        let path = "tree/browser";
        engine.reconcile([scroll(path)]);

        assert!(
            engine
                .handle(
                    pointer_input(PointerPhase::Down, None),
                    &[target(path, 99.0, 10.0)],
                    Instant::now(),
                )
                .is_none()
        );
    }

    #[kithara::test]
    fn layout_viewport_clamps_the_canonical_offset() {
        let mut engine = Engine::default();
        let path = "tree/browser";
        let target = target(path, 50.0, 50.0);
        engine.reconcile([scroll(path)]);

        let _ = engine.handle(
            Input::Wheel(Scroll::pixels(-1_000.0)),
            &[target],
            Instant::now(),
        );
        assert_eq!(engine.scroll_offset(path), Some(100.0));

        engine.set_scroll_viewport(
            path,
            Rect {
                h: 180.0,
                w: 100.0,
                x: 0.0,
                y: 0.0,
            },
        );

        assert_eq!(engine.scroll_offset(path), Some(20.0));
    }

    #[kithara::test]
    fn scroll_offset_survives_descriptor_reconciliation() {
        let mut engine = Engine::default();
        let path = "tree/browser";
        let target = target(path, 50.0, 50.0);
        engine.reconcile([scroll(path)]);
        let _ = engine.handle(Input::Wheel(Scroll::lines(-1.0)), &[target], Instant::now());

        engine.reconcile([scroll(path)]);

        assert_eq!(engine.scroll_offset(path), Some(60.0));
    }

    #[kithara::test]
    fn scrolled_row_click_emits_the_visible_index() {
        let mut engine = Engine::default();
        let now = Instant::now();
        let path = "tree/browser";
        engine.reconcile([scroll(path)]);
        let target = target(path, 50.0, 10.0);

        let wheel = engine
            .handle(Input::Wheel(Scroll::lines(-1.0)), &[target], now)
            .unwrap_or_else(|| panic!("the tree must scroll before the row click"));
        assert_eq!(wheel.outcome, Outcome::captured());
        assert_eq!(wheel.outcome.value(), None);

        let click = engine
            .handle(pointer_input(PointerPhase::Down, None), &[target], now)
            .unwrap_or_else(|| panic!("the visible row must activate"));
        assert_eq!(click.outcome, Outcome::set(EngineEvent::Index(3)));
    }

    #[kithara::test]
    fn cursor_follows_active_capture_then_topmost_hover() {
        let mut engine = Engine::default();
        let now = Instant::now();
        engine.reconcile([knob("studio/back", 0.5), knob("studio/front", 0.5)]);

        assert_eq!(
            engine.cursor(&[
                target("studio/back", 50.0, 50.0),
                target("studio/front", 150.0, 150.0),
            ]),
            CursorShape::ResizeV
        );
        engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[target("studio/front", 50.0, 50.0)],
            now,
        );
        assert_eq!(
            engine.cursor(&[target("studio/front", 150.0, 150.0)]),
            CursorShape::ResizeV
        );
        engine.handle(
            pointer_input(PointerPhase::Up, None),
            &[target("studio/front", 150.0, 150.0)],
            now,
        );
        assert_eq!(
            engine.cursor(&[
                target("studio/back", 150.0, 150.0),
                target("studio/front", 150.0, 150.0),
            ]),
            CursorShape::None
        );
    }

    #[kithara::test]
    fn stereo_meter_seeks_horizontally_with_a_horizontal_cursor() {
        let mut engine = Engine::default();
        let path = "gallery/meters/stereo";
        engine.reconcile([Descriptor::stereo_meter(path.to_owned())]);

        assert_eq!(
            engine.cursor(&[target(path, 25.0, 50.0)]),
            CursorShape::ResizeH
        );
        assert_eq!(
            value(engine.handle(
                pointer_input(PointerPhase::Down, None),
                &[target(path, 25.0, 50.0)],
                Instant::now(),
            )),
            Some(0.25)
        );
    }

    #[kithara::test]
    fn crossfader_seeks_horizontally_with_a_horizontal_cursor() {
        let mut engine = Engine::default();
        let path = "mixer/xfade";
        engine.reconcile([Descriptor::crossfader(path.to_owned())]);

        assert_eq!(
            engine.cursor(&[target(path, 25.0, 50.0)]),
            CursorShape::ResizeH
        );
        assert_eq!(
            value(engine.handle(
                pointer_input(PointerPhase::Down, None),
                &[target(path, 25.0, 50.0)],
                Instant::now(),
            )),
            Some(0.25)
        );
    }

    #[kithara::test]
    fn focused_text_input_publishes_typing_but_not_caret_or_selection_changes() {
        let mut engine = Engine::default();
        let path = "library/search";
        let now = Instant::now();
        let at_end = text_target(path, 30.0, 0.0, 0.0);
        engine.reconcile([text_input(path, "ab")]);
        let focused = engine
            .handle(pointer_input(PointerPhase::Down, None), &[at_end], now)
            .unwrap_or_else(|| panic!("pressing the text input must focus it"));
        assert_eq!(
            focused.outcome,
            Outcome::captured().with_ownership(PointerOwnership::Claim)
        );

        let moved = engine
            .handle(key_pressed(Key::ArrowLeft), &[at_end], now)
            .unwrap_or_else(|| panic!("moving the focused caret must be consumed"));
        assert_eq!(moved.outcome, Outcome::captured());
        let selected = engine
            .handle(shifted(Key::ArrowLeft), &[at_end], now)
            .unwrap_or_else(|| panic!("extending the focused selection must be consumed"));
        assert_eq!(selected.outcome, Outcome::captured());
        assert_eq!(
            engine.text_input_snapshot(path),
            Some(TextInputSnapshot {
                caret: 0,
                focused: true,
                preedit: None,
                selection: Some(0..1),
            })
        );

        let _ = engine.handle(pointer_input(PointerPhase::Down, None), &[at_end], now);
        let first = engine
            .handle(typed("x"), &[at_end], now)
            .unwrap_or_else(|| panic!("ordinary typing must publish the resulting query"));
        assert_eq!(
            first.outcome,
            Outcome::set(EngineEvent::Text("abx".to_owned()))
        );
        let second = engine
            .handle(typed("y"), &[at_end], now)
            .unwrap_or_else(|| panic!("each ordinary key must publish the next query"));
        assert_eq!(
            second.outcome,
            Outcome::set(EngineEvent::Text("abxy".to_owned()))
        );
        let backspaced = engine
            .handle(key_pressed(Key::Backspace), &[at_end], now)
            .unwrap_or_else(|| panic!("backspace must edit the current working query"));
        assert_eq!(
            backspaced.outcome,
            Outcome::set(EngineEvent::Text("abx".to_owned()))
        );
    }

    #[kithara::test]
    fn text_input_keeps_the_caret_on_a_merged_grapheme_boundary() {
        let mut engine = Engine::default();
        let path = "library/search";
        let now = Instant::now();
        let at_start = text_target(path, 4.0, 0.0, 0.0);
        engine.reconcile([Descriptor::text_input(
            path.to_owned(),
            "🇧".to_owned(),
            TextInputLayout::new([(0, 4.0), (4, 20.0)], 3.0, 12.0, 12.0),
        )]);
        let _ = engine.handle(pointer_input(PointerPhase::Down, None), &[at_start], now);

        let inserted = engine
            .handle(typed("🇦"), &[at_start], now)
            .unwrap_or_else(|| panic!("typing must publish the merged flag grapheme"));
        assert_eq!(
            inserted.outcome,
            Outcome::set(EngineEvent::Text("🇦🇧".to_owned()))
        );
        assert_eq!(
            engine
                .text_input_snapshot(path)
                .map(|snapshot| snapshot.caret),
            Some(8)
        );

        let backspaced = engine
            .handle(key_pressed(Key::Backspace), &[at_start], now)
            .unwrap_or_else(|| panic!("backspace must remove the whole merged grapheme"));
        assert_eq!(
            backspaced.outcome,
            Outcome::set(EngineEvent::Text(String::new()))
        );
    }

    #[kithara::test]
    fn text_input_preedit_replaces_silently_and_commit_publishes_once() {
        let mut engine = Engine::default();
        let path = "library/search";
        let now = Instant::now();
        let at_end = text_target(path, 30.0, 0.0, 0.0);
        engine.reconcile([text_input(path, "ab")]);
        let _ = engine.handle(pointer_input(PointerPhase::Down, None), &[at_end], now);

        for input in [
            Input::InputMethod(InputMethod::Preedit {
                content: "かな",
                selection: Some((0, 3)),
            }),
            Input::InputMethod(InputMethod::Preedit {
                content: "日本",
                selection: Some((3, 6)),
            }),
        ] {
            let preedit = engine
                .handle(input, &[at_end], now)
                .unwrap_or_else(|| panic!("preedit changes must be answered"));
            assert_eq!(preedit.outcome, Outcome::captured());
        }
        let snapshot = engine
            .text_input_snapshot(path)
            .unwrap_or_else(|| panic!("composition must remain engine-owned"));
        let preedit = snapshot
            .preedit
            .unwrap_or_else(|| panic!("the latest preedit must be retained"));
        assert_eq!(preedit.content, "日本");
        assert_eq!(preedit.selection, Some(3..6));

        let committed = engine
            .handle(
                Input::InputMethod(InputMethod::Commit("日")),
                &[at_end],
                now,
            )
            .unwrap_or_else(|| panic!("commit must publish the resulting query"));
        assert_eq!(
            committed.outcome,
            Outcome::set(EngineEvent::Text("ab日".to_owned()))
        );
        assert!(
            engine
                .text_input_snapshot(path)
                .is_some_and(|snapshot| snapshot.preedit.is_none())
        );
    }

    #[kithara::test]
    fn text_input_reports_absolute_logical_caret_rect_at_two_positions() {
        let mut engine = Engine::default();
        let path = "library/search";
        let now = Instant::now();
        let first = text_target(path, 44.0, 40.0, 20.0);
        engine.reconcile([text_input(path, "ab")]);
        let _ = engine.handle(pointer_input(PointerPhase::Down, None), &[first], now);
        let first_caret = engine
            .input_method(&[first])
            .unwrap_or_else(|| panic!("focused input must enable the input method"))
            .caret;

        let last = text_target(path, 70.0, 40.0, 20.0);
        let _ = engine.handle(pointer_input(PointerPhase::Down, None), &[last], now);
        let last_caret = engine
            .input_method(&[last])
            .unwrap_or_else(|| panic!("moving the caret must update its rectangle"))
            .caret;

        assert_eq!(
            first_caret,
            Rect {
                x: 44.0,
                y: 23.0,
                w: 1.0,
                h: 12.0
            }
        );
        assert_eq!(
            last_caret,
            Rect {
                x: 70.0,
                y: 23.0,
                w: 1.0,
                h: 12.0
            }
        );
    }

    #[kithara::test]
    fn text_input_owns_delete_and_backspace_only_while_focused() {
        let path = "library/search";
        let now = Instant::now();

        for key in [Key::Delete, Key::Backspace] {
            let mut engine = Engine::default();
            let at_end = text_target(path, 30.0, 0.0, 0.0);
            engine.reconcile([text_input(path, "ab")]);
            assert!(engine.handle(key_pressed(key), &[at_end], now).is_none());

            let _ = engine.handle(pointer_input(PointerPhase::Down, None), &[at_end], now);
            let emission = engine
                .handle(key_pressed(key), &[at_end], now)
                .unwrap_or_else(|| panic!("focused text input must consume {key:?}"));
            assert!(emission.outcome.is_captured());
            assert!(matches!(
                emission.outcome.value(),
                Some(EngineEvent::Text(_))
            ));
        }
    }

    #[kithara::test]
    fn focused_picker_navigates_and_selects_with_the_keyboard() {
        let mut engine = Engine::default();
        let path = "library/scope";
        let target = target(path, 50.0, 50.0);
        let now = Instant::now();
        engine.reconcile([Descriptor::picker(path.to_owned(), 4, Some(1))]);

        let opened = engine
            .handle(pointer_input(PointerPhase::Down, None), &[target], now)
            .unwrap_or_else(|| panic!("pressing the picker anchor must open it"));
        assert_eq!(opened.outcome, Outcome::captured());
        let snapshot = engine
            .picker_snapshot(path)
            .unwrap_or_else(|| panic!("the picker must expose its retained paint state"));
        assert!(snapshot.open);
        assert_eq!(snapshot.highlighted, Some(1));

        let navigated = engine
            .handle(key_pressed(Key::ArrowDown), &[], now)
            .unwrap_or_else(|| panic!("a focused picker must consume arrow navigation"));
        assert_eq!(navigated.outcome, Outcome::captured());
        assert_eq!(
            engine
                .picker_snapshot(path)
                .and_then(|snapshot| snapshot.highlighted),
            Some(2)
        );

        let selected = engine
            .handle(key_pressed(Key::Enter), &[], now)
            .unwrap_or_else(|| panic!("enter must select the highlighted option"));
        assert_eq!(selected.outcome, Outcome::set(EngineEvent::Index(2)));
        assert_eq!(
            engine.picker_snapshot(path).map(|snapshot| snapshot.open),
            Some(false)
        );

        for expected in [true, false] {
            let toggled = engine
                .handle(key_pressed(Key::Space), &[], now)
                .unwrap_or_else(|| panic!("space must toggle a focused picker"));
            assert_eq!(toggled.outcome, Outcome::captured());
            assert_eq!(
                engine.picker_snapshot(path).map(|snapshot| snapshot.open),
                Some(expected)
            );
            assert_eq!(
                engine
                    .handle(key_released(Key::Space), &[], now)
                    .map(|emission| emission.outcome),
                Some(Outcome::captured())
            );
        }
    }

    #[kithara::test]
    fn picker_toggle_and_commit_repeats_are_inert_until_release_or_blur() {
        let mut engine = Engine::default();
        let path = "library/scope";
        let now = Instant::now();
        let anchor = target(path, 50.0, 50.0);
        engine.reconcile([Descriptor::picker(path.to_owned(), 2, Some(0))]);
        let _ = engine.handle(pointer_input(PointerPhase::Down, None), &[anchor], now);

        let selected = engine
            .handle(key_pressed(Key::Enter), &[], now)
            .unwrap_or_else(|| panic!("the first Enter must commit the highlighted option"));
        assert_eq!(selected.outcome, Outcome::set(EngineEvent::Index(0)));
        let repeated = engine
            .handle(key_pressed(Key::Enter), &[], now)
            .unwrap_or_else(|| panic!("the repeated Enter must remain owned"));
        assert_eq!(repeated.outcome, Outcome::captured());
        assert_eq!(
            engine.picker_snapshot(path).map(|snapshot| snapshot.open),
            Some(false)
        );

        let _ = engine.handle(key_released(Key::Enter), &[], now);
        let _ = engine.handle(key_pressed(Key::Space), &[], now);
        let _ = engine.handle(key_pressed(Key::Space), &[], now);
        assert_eq!(
            engine.picker_snapshot(path).map(|snapshot| snapshot.open),
            Some(true)
        );

        let _ = engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[target(path, 150.0, 150.0)],
            now,
        );
        let _ = engine.handle(pointer_input(PointerPhase::Down, None), &[anchor], now);
        let selected = engine
            .handle(key_pressed(Key::Enter), &[], now)
            .unwrap_or_else(|| panic!("blur must release retained key-down state"));
        assert_eq!(selected.outcome, Outcome::set(EngineEvent::Index(0)));
    }

    #[kithara::test]
    fn picker_focus_survives_reorder_and_layout_swap_but_not_removal() {
        let mut engine = Engine::default();
        let path = "library/scope";
        let other = "library/action";
        let now = Instant::now();
        engine.reconcile([
            Descriptor::activation(other.to_owned()),
            Descriptor::picker(path.to_owned(), 2, Some(0)),
        ]);
        let _ = engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[target(other, 150.0, 150.0), target(path, 50.0, 50.0)],
            now,
        );
        let _ = engine.handle(key_pressed(Key::Escape), &[target(path, 50.0, 50.0)], now);

        engine.reconcile([
            Descriptor::picker(path.to_owned(), 2, Some(0)),
            Descriptor::activation(other.to_owned()),
        ]);
        let opened = engine.handle(
            key_pressed(Key::Enter),
            &[target(path, 25.0, 25.0), target(other, 75.0, 75.0)],
            now,
        );
        assert_eq!(
            opened.map(|emission| emission.outcome),
            Some(Outcome::captured())
        );
        assert_eq!(
            engine.picker_snapshot(path).map(|snapshot| snapshot.open),
            Some(true)
        );

        engine.reconcile([Descriptor::activation(other.to_owned())]);
        engine.reconcile([
            Descriptor::picker(path.to_owned(), 2, Some(0)),
            Descriptor::activation(other.to_owned()),
        ]);
        assert!(
            engine
                .handle(key_pressed(Key::Enter), &[target(path, 50.0, 50.0)], now)
                .is_none(),
            "removing the focused path must clear focus before it is re-added"
        );
        assert_eq!(
            engine.picker_snapshot(path).map(|snapshot| snapshot.open),
            Some(false)
        );
    }

    #[kithara::test]
    fn outside_pointer_press_clears_picker_focus() {
        let mut engine = Engine::default();
        let path = "library/scope";
        let now = Instant::now();
        engine.reconcile([Descriptor::picker(path.to_owned(), 2, Some(0))]);
        let _ = engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[target(path, 50.0, 50.0)],
            now,
        );
        let _ = engine.handle(key_pressed(Key::Escape), &[target(path, 50.0, 50.0)], now);

        assert!(
            engine
                .handle(
                    pointer_input(PointerPhase::Down, None),
                    &[target(path, 150.0, 150.0)],
                    now
                )
                .is_none()
        );
        assert!(
            engine
                .handle(key_pressed(Key::Enter), &[target(path, 50.0, 50.0)], now)
                .is_none()
        );
        assert_eq!(
            engine.picker_snapshot(path).map(|snapshot| snapshot.open),
            Some(false)
        );
    }

    #[kithara::test]
    fn pointer_capture_and_keyboard_focus_are_independent() {
        let mut engine = Engine::default();
        let picker = "library/scope";
        let knob = "studio/gain";
        let now = Instant::now();
        engine.reconcile([
            Descriptor::picker(picker.to_owned(), 2, Some(0)),
            Descriptor::knob(knob.to_owned(), 0.5, 100.0, 0.1),
        ]);
        let _ = engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[target(knob, 50.0, 50.0)],
            now,
        );
        assert!(engine.captures(knob));

        let _ = engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[target(knob, 150.0, 150.0), target(picker, 50.0, 50.0)],
            now,
        );
        let opened = engine
            .handle(
                key_pressed(Key::Enter),
                &[target(knob, 150.0, 150.0), target(picker, 50.0, 50.0)],
                now,
            )
            .unwrap_or_else(|| panic!("keyboard focus must route before pointer capture"));
        assert_eq!(opened.path, picker);
        assert_eq!(opened.outcome, Outcome::captured());
        assert!(engine.captures(knob));

        let _ = engine.handle(
            pointer_input(PointerPhase::Up, None),
            &[target(knob, 150.0, 150.0)],
            now,
        );
        assert!(!engine.captures_pointer());
        let closed = engine
            .handle(key_pressed(Key::Escape), &[target(picker, 50.0, 50.0)], now)
            .unwrap_or_else(|| panic!("releasing capture must not change focus"));
        assert_eq!(closed.path, picker);
        assert_eq!(closed.outcome, Outcome::captured());
    }

    #[kithara::test]
    fn picker_pointer_hover_select_and_dismiss_keep_committed_value_silent() {
        let mut engine = Engine::default();
        let path = "library/scope";
        let now = Instant::now();
        engine.reconcile([Descriptor::picker(path.to_owned(), 4, Some(1))]);
        let anchor = target(path, 50.0, 50.0);
        let option = item_target(path, 3, 50.0, 50.0);
        let _ = engine.handle(pointer_input(PointerPhase::Down, None), &[anchor], now);

        let hovered = engine
            .handle(
                pointer_input(PointerPhase::Move, Some(Pt { x: 50.0, y: 50.0 })),
                &[option],
                now,
            )
            .unwrap_or_else(|| panic!("moving over an open option must be consumed"));
        assert_eq!(hovered.outcome, Outcome::captured());
        assert_eq!(
            engine
                .picker_snapshot(path)
                .and_then(|snapshot| snapshot.highlighted),
            Some(3)
        );

        let selected = engine
            .handle(pointer_input(PointerPhase::Down, None), &[option], now)
            .unwrap_or_else(|| panic!("pressing an open option must select it"));
        assert_eq!(selected.outcome, Outcome::set(EngineEvent::Index(3)));
        let _ = engine.handle(pointer_input(PointerPhase::Down, None), &[anchor], now);
        let dismissed = engine
            .handle(
                pointer_input(PointerPhase::Down, None),
                &[target(path, 150.0, 150.0)],
                now,
            )
            .unwrap_or_else(|| panic!("an outside press must dismiss an open picker"));
        assert_eq!(dismissed.outcome, Outcome::captured());
        assert_eq!(dismissed.outcome.value(), None);
        assert_eq!(
            engine.picker_snapshot(path),
            Some(PickerSnapshot {
                open: false,
                highlighted: Some(3),
            })
        );

        engine.reconcile([Descriptor::picker(path.to_owned(), 2, Some(1))]);
        assert_eq!(
            engine.picker_snapshot(path),
            Some(PickerSnapshot {
                open: false,
                highlighted: Some(1),
            })
        );
    }

    #[kithara::test]
    fn focused_picker_consumes_owned_shortcuts_and_releases_only() {
        let mut engine = Engine::default();
        let path = "library/scope";
        let now = Instant::now();
        let target = target(path, 50.0, 50.0);
        engine.reconcile([Descriptor::picker(path.to_owned(), 2, Some(0))]);

        for key in [Key::Delete, Key::Backspace] {
            assert!(
                engine.handle(key_pressed(key), &[target], now).is_none(),
                "an unfocused picker must not consume {key:?}"
            );
        }
        let _ = engine.handle(pointer_input(PointerPhase::Down, None), &[target], now);
        for key in [
            Key::ArrowDown,
            Key::ArrowUp,
            Key::Backspace,
            Key::Delete,
            Key::Enter,
            Key::Escape,
            Key::Space,
        ] {
            let emission = engine
                .handle(key_released(key), &[target], now)
                .unwrap_or_else(|| panic!("a focused picker must consume the {key:?} release"));
            assert_eq!(emission.outcome, Outcome::captured());
        }
        for key in [Key::Delete, Key::Backspace] {
            for input in [key_pressed(key), key_released(key)] {
                let emission = engine
                    .handle(input, &[target], now)
                    .unwrap_or_else(|| panic!("a focused picker must consume {key:?}"));
                assert_eq!(emission.outcome, Outcome::captured());
                assert_eq!(emission.outcome.value(), None);
            }
        }
        assert!(
            engine
                .handle(key_pressed(Key::Character("x")), &[target], now)
                .is_none()
        );
        assert!(
            engine
                .handle(key_pressed(Key::Other), &[target], now)
                .is_none()
        );

        let escaped = engine
            .handle(key_pressed(Key::Escape), &[target], now)
            .unwrap_or_else(|| panic!("escape must close a focused picker"));
        assert_eq!(escaped.outcome, Outcome::captured());
        assert_eq!(escaped.outcome.value(), None);
        assert_eq!(
            engine.picker_snapshot(path).map(|snapshot| snapshot.open),
            Some(false)
        );
    }

    #[kithara::test]
    fn picker_reconcile_retains_open_state_and_clamps_highlight() {
        let mut engine = Engine::default();
        let path = "library/scope";
        let now = Instant::now();
        engine.reconcile([Descriptor::picker(path.to_owned(), 4, Some(0))]);
        let _ = engine.handle(
            pointer_input(PointerPhase::Down, None),
            &[target(path, 50.0, 50.0)],
            now,
        );
        let _ = engine.handle(
            pointer_input(PointerPhase::Move, Some(Pt { x: 50.0, y: 50.0 })),
            &[item_target(path, 3, 50.0, 50.0)],
            now,
        );

        engine.reconcile([Descriptor::picker(path.to_owned(), 2, Some(1))]);

        assert_eq!(
            engine.picker_snapshot(path),
            Some(PickerSnapshot {
                open: true,
                highlighted: Some(1),
            })
        );
    }

    #[kithara::test]
    fn picker_routes_past_later_option_misses_to_the_hit_option_and_anchor() {
        let mut engine = Engine::default();
        let path = "library/scope";
        let now = Instant::now();
        engine.reconcile([Descriptor::picker(path.to_owned(), 3, Some(0))]);

        for (index, pointer_y) in [(0, 30.0), (1, 50.0)] {
            let anchor_targets = [
                picker_target(path, None, 10.0, 0.0),
                picker_target(path, Some(0), 10.0, 20.0),
                picker_target(path, Some(1), 10.0, 40.0),
                picker_target(path, Some(2), 10.0, 60.0),
            ];
            let _ = engine.handle(
                pointer_input(PointerPhase::Down, None),
                &anchor_targets,
                now,
            );
            assert_eq!(
                engine.picker_snapshot(path).map(|snapshot| snapshot.open),
                Some(true)
            );

            let option_targets = [
                picker_target(path, None, pointer_y, 0.0),
                picker_target(path, Some(0), pointer_y, 20.0),
                picker_target(path, Some(1), pointer_y, 40.0),
                picker_target(path, Some(2), pointer_y, 60.0),
            ];
            let selected = engine
                .handle(
                    pointer_input(PointerPhase::Down, None),
                    &option_targets,
                    now,
                )
                .unwrap_or_else(|| panic!("option {index} must route past later misses"));
            assert_eq!(selected.outcome, Outcome::set(EngineEvent::Index(index)));
        }

        let anchor_targets = [
            picker_target(path, None, 10.0, 0.0),
            picker_target(path, Some(0), 10.0, 20.0),
            picker_target(path, Some(1), 10.0, 40.0),
            picker_target(path, Some(2), 10.0, 60.0),
        ];
        let _ = engine.handle(
            pointer_input(PointerPhase::Down, None),
            &anchor_targets,
            now,
        );
        let closed = engine
            .handle(
                pointer_input(PointerPhase::Down, None),
                &anchor_targets,
                now,
            )
            .unwrap_or_else(|| panic!("the open anchor must route past option misses"));
        assert_eq!(closed.outcome, Outcome::captured());
        assert_eq!(
            engine.picker_snapshot(path).map(|snapshot| snapshot.open),
            Some(false)
        );
    }
}
