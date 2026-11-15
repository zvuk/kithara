use kithara_platform::time::Instant;

use super::retained::Component;
use crate::{
    engine::model::{EngineEvent, Kind, Press, Presses},
    interact::{
        CursorShape, Hit, Hover, Input, Outcome, PointerButton, PointerPhase,
        recognizers::DoubleClick,
    },
};

pub(crate) struct ActivationComponent {
    clicks: DoubleClick,
    double: bool,
    hover: Hover,
    path: String,
    press: Press,
}

impl ActivationComponent {
    pub(super) fn new(path: String, press: Press) -> Self {
        Self {
            path,
            press,
            clicks: DoubleClick::default(),
            double: false,
            hover: Hover::new(CursorShape::Pointer),
        }
    }

    pub(super) fn reconcile(mut self, next: Self) -> Self {
        self.path = next.path;
        self.press = next.press;
        self
    }

    fn press(
        &mut self,
        button: Option<PointerButton>,
        hit: &Hit,
        index: Option<usize>,
        now: Instant,
    ) -> Outcome<EngineEvent> {
        let Some(at) = hit.inside() else {
            return Outcome::IGNORED;
        };
        let declared = match &self.press {
            Press::Activate(presses) => *presses,
            Press::Texts(_) => Presses::default(),
        };
        match button {
            Some(PointerButton::Primary) | None => {
                self.double = self.clicks.register(at, now) && declared.double;
                match &self.press {
                    Press::Activate(_) => Outcome::set(EngineEvent::Activate),
                    Press::Texts(texts) => index
                        .and_then(|index| texts.get(index))
                        .and_then(Option::as_ref)
                        .map_or(Outcome::IGNORED, |text| {
                            Outcome::set(EngineEvent::Text(text.clone()))
                        }),
                }
            }
            Some(PointerButton::Secondary) if declared.secondary => {
                Outcome::set(EngineEvent::SecondaryActivate)
            }
            Some(
                PointerButton::Secondary
                | PointerButton::Auxiliary
                | PointerButton::Back
                | PointerButton::Forward
                | PointerButton::Other(_),
            ) => Outcome::IGNORED,
        }
    }
}

impl Component for ActivationComponent {
    fn captures_pointer(&self) -> bool {
        false
    }

    fn cursor(&self, hit: &Hit) -> CursorShape {
        self.hover.cursor(false, hit)
    }

    fn handle(
        &mut self,
        input: Input<'_>,
        hit: &Hit,
        index: Option<usize>,
        now: Instant,
    ) -> (Outcome<EngineEvent>, Option<&'static str>) {
        let Input::Pointer(pointer) = input else {
            return (Outcome::IGNORED, None);
        };
        let outcome = match pointer.phase {
            PointerPhase::Down => self.press(pointer.button, hit, index, now),
            PointerPhase::Up | PointerPhase::DoubleClick if std::mem::take(&mut self.double) => {
                Outcome::set(EngineEvent::DoubleActivate)
            }
            PointerPhase::Cancel => {
                self.double = false;
                Outcome::IGNORED
            }
            PointerPhase::Up
            | PointerPhase::DoubleClick
            | PointerPhase::Leave
            | PointerPhase::LongPress
            | PointerPhase::Move
            | PointerPhase::MoveLongPress => Outcome::IGNORED,
        };
        (outcome, None)
    }

    fn kind(&self) -> Kind {
        Kind::Activation
    }

    fn path(&self) -> &str {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::{Duration, Instant};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        draw::{Pt, Rect},
        interact::{MOUSE, PointerButton, PointerInput, PointerPhase},
    };

    fn inside() -> Hit {
        Hit::new(
            Some(Pt { x: 20.0, y: 10.0 }),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 40.0,
                h: 20.0,
            },
        )
    }

    fn pressable() -> ActivationComponent {
        ActivationComponent::new(
            "demo/chip".to_owned(),
            Press::Activate(Presses {
                secondary: true,
                double: true,
            }),
        )
    }

    fn written(
        component: &mut ActivationComponent,
        button: PointerButton,
        phases: [PointerPhase; 2],
        now: Instant,
    ) -> Vec<EngineEvent> {
        phases
            .into_iter()
            .filter_map(|phase| {
                let input = Input::Pointer(PointerInput::new(MOUSE, Some(button), phase, None, 1));
                component.handle(input, &inside(), None, now).0.value()
            })
            .collect()
    }

    fn click(component: &mut ActivationComponent, now: Instant) -> Vec<EngineEvent> {
        written(
            component,
            PointerButton::Primary,
            [PointerPhase::Down, PointerPhase::Up],
            now,
        )
    }

    #[kithara::test]
    fn a_primary_press_writes_press_once() {
        assert_eq!(
            click(&mut pressable(), Instant::now()),
            [EngineEvent::Activate]
        );
    }

    #[kithara::test]
    fn a_secondary_press_writes_secondary() {
        assert_eq!(
            written(
                &mut pressable(),
                PointerButton::Secondary,
                [PointerPhase::Down, PointerPhase::Up],
                Instant::now()
            ),
            [EngineEvent::SecondaryActivate]
        );
    }

    #[kithara::test]
    #[case::released(PointerPhase::Up)]
    #[case::released_as_double_click(PointerPhase::DoubleClick)]
    fn two_quick_presses_write_press_twice_and_double_once_after_the_second(
        #[case] release: PointerPhase,
    ) {
        let mut component = pressable();
        let first = Instant::now();
        let mut events = click(&mut component, first);
        events.extend(written(
            &mut component,
            PointerButton::Primary,
            [PointerPhase::Down, release],
            first + Duration::from_millis(120),
        ));

        assert_eq!(
            events,
            [
                EngineEvent::Activate,
                EngineEvent::Activate,
                EngineEvent::DoubleActivate
            ]
        );
    }

    #[kithara::test]
    fn a_slow_second_press_is_not_a_double_click() {
        let mut component = pressable();
        let first = Instant::now();
        let mut events = click(&mut component, first);
        events.extend(click(&mut component, first + Duration::from_millis(600)));

        assert_eq!(events, [EngineEvent::Activate, EngineEvent::Activate]);
    }

    #[kithara::test]
    fn an_undeclared_double_click_writes_only_its_presses() {
        let mut component =
            ActivationComponent::new("demo/chip".to_owned(), Press::Activate(Presses::default()));
        let first = Instant::now();
        let mut events = click(&mut component, first);
        events.extend(click(&mut component, first + Duration::from_millis(120)));
        events.extend(written(
            &mut component,
            PointerButton::Secondary,
            [PointerPhase::Down, PointerPhase::Up],
            first + Duration::from_millis(900),
        ));

        assert_eq!(events, [EngineEvent::Activate, EngineEvent::Activate]);
    }
}
