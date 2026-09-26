use kithara_test_utils::kithara;
use kithara_ui::{
    app::Ui,
    draw::Pt,
    interact::{Input, MOUSE, PointerInput, PointerPhase, Scroll},
    render::Skin,
};
use num_traits::cast::AsPrimitive;

use super::{
    fixture::{CONTROL, Census, Fixture, HEIGHT, WIDTH, skin},
    named::{Answer, KEYS, Named, ROWS, handed_over},
    table::CONTROL_CENSUS,
};
use crate::immediate::Immediate;

/// The retained host with one row mounted, and what of it has already been
/// seen: how much the document published, and the picture it last drew.
struct Retained<'a> {
    ui: Ui<'a, Census<'a>>,
    counted: usize,
    picture: Vec<u32>,
}

impl<'a> Retained<'a> {
    fn new(mut ui: Ui<'a, Census<'a>>) -> Self {
        let picture = picture(&mut ui);
        Self {
            ui,
            counted: 0,
            picture,
        }
    }

    /// Hands the host one event and says what came of it that anything
    /// outside the host can see.
    ///
    /// Neither observable is sound alone: a control can take an event and
    /// change only what it shows - a table scrolled, a knob gripped - and a
    /// control can publish without drawing anything new.
    fn feed(&mut self, input: Input<'_>) -> Answer {
        self.ui.input(input);
        let published = self.ui.app().published().len();
        let acted = published > self.counted;
        self.counted = published;
        let picture = picture(&mut self.ui);
        let took = picture != self.picture;
        self.picture = picture;
        Answer { acted, took }
    }

    fn pointer(&mut self, at: Pt, phase: PointerPhase, clicks: u8) -> Answer {
        self.feed(Input::Pointer(PointerInput::new(
            MOUSE,
            None,
            phase,
            Some(at),
            clicks,
        )))
    }

    /// A press and its release at one point.
    fn click(&mut self, at: Pt, clicks: u8) -> Answer {
        let down = self.pointer(at, PointerPhase::Down, clicks);
        down.or(self.pointer(at, PointerPhase::Up, clicks))
    }

    /// The point a fraction of the way across and down the box the control
    /// was actually laid out into.
    ///
    /// Window chrome answers before the tree and has no document leaf to
    /// address; the middle of the window is on it, because it is the only
    /// thing mounted.
    fn aim(&self, across: f32, down: f32) -> Pt {
        self.ui.rect_of(CONTROL).map_or_else(
            || {
                let (width, height): (f32, f32) = (WIDTH.as_(), HEIGHT.as_());
                Pt {
                    x: width * across,
                    y: height * down,
                }
            },
            |rect| Pt {
                x: rect.x + rect.w * across,
                y: rect.y + rect.h * down,
            },
        )
    }

    /// Plays one gesture at one point and says what the document did with it.
    ///
    /// The hand arrives at the point before the gesture starts, so what the
    /// control draws for a hand over it is not read as an answer.
    fn play(&mut self, named: Named, at: Pt) -> Answer {
        self.pointer(at, PointerPhase::Move, 1);
        match named {
            Named::Press => self.click(at, 1),
            Named::Drag => {
                self.pointer(at, PointerPhase::Down, 1);
                let first = self.pointer(
                    Pt {
                        x: at.x + 2.0,
                        y: at.y,
                    },
                    PointerPhase::Move,
                    1,
                );
                first.or(self.pointer(
                    Pt {
                        x: at.x + 24.0,
                        y: at.y,
                    },
                    PointerPhase::Move,
                    1,
                ))
            }
            Named::Wheel => {
                self.pointer(at, PointerPhase::Move, 1);
                self.feed(Input::Wheel(Scroll::Lines { x: 0.0, y: -2.0 }))
            }
            Named::DoubleClick => {
                let first = self.click(at, 1);
                let second = self.click(at, 2);
                Answer {
                    acted: second.acted && !first.acted,
                    took: false,
                }
            }
            Named::Keyboard => {
                self.click(at, 1);
                KEYS.iter().fold(Answer::default(), |answer, (stroke, _)| {
                    answer.or(self.feed(stroke.retained()))
                })
            }
        }
    }
}

/// What the retained host draws now, as one stream of the scene's path and
/// draw data and the transforms placing them.
fn picture(ui: &mut Ui<'_, Census<'_>>) -> Vec<u32> {
    let frame = ui
        .render()
        .unwrap_or_else(|error| panic!("the census row must draw: {error}"));
    ui.complete_frame();
    let encoding = frame.scene().encoding();
    let placed = encoding.transforms.iter().flat_map(|transform| {
        transform
            .matrix
            .iter()
            .chain(&transform.translation)
            .map(|value| value.to_bits())
    });
    encoding
        .path_data
        .iter()
        .chain(&encoding.draw_data)
        .copied()
        .chain(placed)
        .collect()
}

/// Drives one gesture at the box the retained host laid the control into.
fn retained(named: Named, control: &str, skin: &Skin) -> Answer {
    /// The interior of a box, in fractions of its own width and height.
    ///
    /// A control is not uniformly live: a strip answers on its crumbs and not
    /// in the gap between them, and a table answers on a row. Aiming at one
    /// point measures where the aim landed, not what the control takes, so
    /// every point gets its own host and the control answers if any does.
    const AIMS: &[f32] = &[0.25, 0.5, 0.75];

    let fixture = Fixture::new(control);
    let mut answer = Answer::default();
    for across in AIMS {
        for down in AIMS {
            let mut host = Retained::new(fixture.mount(skin));
            let at = host.aim(*across, *down);
            answer = answer.or(host.play(named, at));
        }
    }
    answer
}

/// Plays one gesture at one point and says what the document did with it.
///
/// A drag is measured by its travel, not by the press that starts it: a
/// control that answers the press and drops every move would otherwise
/// pass as a control that drags. The retained driver discards the same
/// press for the same reason.
///
/// The travel is two moves rather than one because a recognizer may spend
/// the first fixing what the rest are measured from, and a drag played as
/// a single move is then below every threshold by construction.
fn played(named: Named, host: &mut Immediate<'_, Census<'_>>, at: Pt) -> Answer {
    match named {
        Named::Press => {
            let took = host.click_at(at);
            Answer {
                took,
                acted: !host.app().published().is_empty(),
            }
        }
        Named::Drag => {
            host.press_at(at);
            let started = host.app().published().len();
            let first = host.hover_at(Pt {
                x: at.x + 2.0,
                y: at.y,
            });
            let second = host.hover_at(Pt {
                x: at.x + 24.0,
                y: at.y,
            });
            Answer {
                acted: host.app().published().len() > started,
                took: first || second,
            }
        }
        Named::Wheel => {
            let took = host.wheel_at(at, -2.0);
            Answer {
                took,
                acted: !host.app().published().is_empty(),
            }
        }
        Named::DoubleClick => {
            host.click_at(at);
            let single = host.app().published().len();
            host.click_at(at);
            Answer {
                acted: single == 0 && host.app().published().len() > single,
                took: false,
            }
        }
        Named::Keyboard => {
            host.click_at(at);
            let focused = host.app().published().len();
            let took = KEYS.iter().fold(false, |taken, (stroke, code)| {
                host.key_at(at, stroke.immediate(), *code) || taken
            });
            Answer {
                took,
                acted: host.app().published().len() > focused,
            }
        }
    }
}

/// Drives the same gesture over the whole window on the immediate host.
///
/// The retained driver aims at the box it laid the control into, because
/// that host keeps a tree that can be asked. This host keeps none, and borrowing the
/// other host's box would make a control that answers on both look silent
/// here the moment the two lay it out differently - a question about
/// geometry, answered as if it were one about gestures. So this sweeps the
/// window instead, and the control answers if any point of it does.
fn driven_immediate(named: Named, control: &str, skin: &Skin) -> Answer {
    /// How far apart the points the immediate census drives are.
    ///
    /// Four pixels is under the smallest box any control in the census was
    /// laid out into, so a control that answers anywhere is reached.
    const SWEEP: f32 = 4.0;

    let ui = Fixture::new(control).compiled();
    let (width, height): (f32, f32) = (WIDTH.as_(), HEIGHT.as_());
    let mut y = SWEEP / 2.0;
    while y < height {
        let mut x = SWEEP / 2.0;
        while x < width {
            let mut host = Immediate::mount(Census::new(skin), &ui, skin, (WIDTH, HEIGHT));
            let answer = played(named, &mut host, Pt { x, y });
            if !answer.silent() {
                return answer;
            }
            x += SWEEP;
        }
        y += SWEEP;
    }
    Answer::default()
}

/// Drives, on the retained host, the pointer gesture each control names.
///
/// The census beside this one compares the two hosts' declarations. Saying
/// a gesture is not answering it: a control declared a drag both hosts
/// agreed on while the retained one dropped every move, and no test could
/// see it, because the words matched. This mounts each control alone,
/// finds the box it was laid out into, and pushes real input at it.
#[kithara::test]
fn every_control_answers_the_pointer_gesture_it_names_on_the_retained_host() {
    let skin = skin();

    let mut observed = Vec::new();
    let mut expected = Vec::new();
    for (row, (_, _, control)) in ROWS.iter().zip(CONTROL_CENSUS) {
        for named in Named::ALL {
            if !named.declared_by(row.gestures) {
                continue;
            }
            let answers = !retained(named, control, &skin).silent();
            observed.push(format!("{} {named:?}: {answers}", row.name));
            expected.push(format!(
                "{} {named:?}: {}",
                row.name,
                !handed_over(row.name, named)
            ));
        }
    }

    assert_eq!(
        observed, expected,
        "the retained host answers a different set of pointer gestures than the controls name"
    );
}

/// Drives, on the immediate host, the pointer gesture each control names.
///
/// The twin of the census above. The two hosts route a pointer through
/// machinery with nothing in common - one against boxes read out of a tree
/// it keeps, the other by letting iced walk a tree it rebuilt - and a
/// control that answers on one and not the other draws exactly the same
/// picture. Driving both against the one table each declared its gestures
/// in is what makes that visible.
#[kithara::test]
fn every_control_answers_the_pointer_gesture_it_names_on_the_immediate_host() {
    let skin = skin();

    let mut observed = Vec::new();
    let mut expected = Vec::new();
    for (row, (_, _, control)) in ROWS.iter().zip(CONTROL_CENSUS) {
        for named in Named::ALL {
            if !named.declared_by(row.gestures) {
                continue;
            }
            let answers = !driven_immediate(named, control, &skin).silent();
            observed.push(format!("{} {named:?}: {answers}", row.name));
            expected.push(format!(
                "{} {named:?}: {}",
                row.name,
                !handed_over(row.name, named)
            ));
        }
    }

    assert_eq!(
        observed, expected,
        "the immediate host answers a different set of pointer gestures than the controls name"
    );
}
