//! How often the retained route paints a custom widget an application outside
//! this workspace mounts: never while nothing changes, once when the
//! application says its own state changed, and on every frame of an
//! animation only while the widget still asks for one.

use std::{cell::Cell, rc::Rc};

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use kithara_ui::{
    draw::{DrawListBuilder, Rect},
    render::{
        CustomSkin,
        masonry::{CustomWidget, MasonryRoot, Repaint, Size2, SizeLimits, TextMeasurer},
    },
};
use masonry::core::WindowEvent;

use super::custom_route::{Out, Probe, SLOT, Squash, document, mounted, skin};

/// One frame at sixty frames a second.
const FRAME: Duration = Duration::from_millis(16);

/// The state the application owns and the widget only shows, and what the
/// widget has done with it so far.
#[derive(Default)]
struct Gauge {
    /// The value the application last set.
    level: Rc<Cell<u32>>,
    /// Frames of animation the application asked for and the widget has yet
    /// to play.
    glide: Rc<Cell<u32>>,
    /// The level shown by every paint, oldest first.
    painted: Rc<Cell<Vec<u32>>>,
    /// How many frames the widget was handed.
    framed: Rc<Cell<u32>>,
}

impl Gauge {
    fn share(&self) -> Self {
        Self {
            level: Rc::clone(&self.level),
            glide: Rc::clone(&self.glide),
            painted: Rc::clone(&self.painted),
            framed: Rc::clone(&self.framed),
        }
    }

    fn paints(&self) -> Vec<u32> {
        let painted = self.painted.take();
        self.painted.set(painted.clone());
        painted
    }
}

impl CustomWidget for Gauge {
    type Action = Probe;

    fn frame(&mut self, _elapsed: Duration) -> Option<Probe> {
        self.framed.set(self.framed.get() + 1);
        self.glide.set(self.glide.get().saturating_sub(1));
        None
    }

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn paint(
        &mut self,
        _list: &mut DrawListBuilder,
        _text: &mut TextMeasurer<'_>,
        _bounds: Rect,
        _skin: &CustomSkin,
    ) {
        let mut painted = self.painted.take();
        painted.push(self.level.get());
        self.painted.set(painted);
    }

    fn repaint(&self) -> Repaint {
        if self.glide.get() > 0 {
            Repaint::Continuous
        } else {
            Repaint::None
        }
    }
}

fn redraw(root: &mut MasonryRoot<Out>) {
    root.redraw()
        .unwrap_or_else(|error| panic!("the route must redraw: {error}"));
}

/// One pass of a window's frame loop, made the way the window runner makes
/// it: the frame is handed out, the picture is drawn only if something
/// changed, and the frame is completed. Returns whether the root asked for
/// another pass.
fn pass(root: &mut MasonryRoot<Out>) -> bool {
    root.handle_window_event(WindowEvent::AnimFrame(FRAME))
        .unwrap_or_else(|error| panic!("a frame must remain typed: {error}"));
    if !root.needs_frame() {
        return false;
    }
    redraw(root);
    root.complete_frame()
}

/// More passes than anything in these fixtures asks for in a row; a root
/// still asking after this many is animating forever.
const RUNAWAY: u32 = 16;

/// Runs the window for as long as the root wants frames, starting only if it
/// wants one now. Returns how many passes it made.
fn run(root: &mut MasonryRoot<Out>) -> u32 {
    let mut passes = 0;
    let mut wanted = root.needs_frame();
    while wanted {
        assert!(passes < RUNAWAY, "the root must stop asking for frames");
        wanted = pass(root);
        passes += 1;
    }
    passes
}

/// A mounted gauge with every frame its mount asked for drawn, so what
/// follows starts from rest.
fn settled(gauge: &Gauge) -> MasonryRoot<Out> {
    let (ui, skin) = (document(SLOT), skin());
    let mut root = mounted(&ui, &skin, &Squash::default(), gauge.share());
    let _ = run(&mut root);
    gauge.framed.set(0);
    root
}

fn invalidate(root: &mut MasonryRoot<Out>, path: &str) -> bool {
    root.invalidate(path)
        .unwrap_or_else(|error| panic!("invalidation must remain typed: {error}"))
}

#[kithara::test]
fn a_widget_nothing_changes_goes_idle() {
    let gauge = Gauge::default();
    let mut root = settled(&gauge);

    assert_eq!(gauge.paints(), vec![0], "the mount paints the widget once");
    assert!(!root.needs_frame(), "a widget at rest asks for no frame");
    assert!(
        !pass(&mut root),
        "a frame handed out anyway changes nothing"
    );
    assert_eq!(
        gauge.paints(),
        vec![0],
        "an unchanged widget is not repainted"
    );
    assert_eq!(gauge.framed.get(), 0, "a widget at rest is handed no frame");
}

#[kithara::test]
fn a_state_change_the_application_reports_paints_exactly_once() {
    let gauge = Gauge::default();
    let mut root = settled(&gauge);

    gauge.level.set(7);
    assert!(
        invalidate(&mut root, "demo/custom"),
        "the widget installed at the path is the one invalidated"
    );

    assert_eq!(run(&mut root), 1, "one frame draws the change");
    assert_eq!(gauge.paints(), vec![0, 7], "the new state is painted once");
    assert_eq!(gauge.framed.get(), 0, "a repaint is not an animation");
    assert!(!root.needs_frame(), "the widget is at rest again");
}

#[kithara::test]
fn invalidating_a_path_with_no_custom_widget_schedules_nothing() {
    let gauge = Gauge::default();
    let mut root = settled(&gauge);

    assert!(
        !invalidate(&mut root, "demo/elsewhere"),
        "nothing is installed there"
    );
    assert!(!root.needs_frame());
    assert_eq!(gauge.paints(), vec![0]);
}

#[kithara::test]
fn a_finite_animation_is_framed_only_while_it_asks() {
    const GLIDE: u32 = 3;
    let gauge = Gauge::default();
    let mut root = settled(&gauge);

    gauge.glide.set(GLIDE);
    let _ = invalidate(&mut root, "demo/custom");

    assert_eq!(run(&mut root), GLIDE, "one pass per frame of animation");
    assert_eq!(gauge.framed.get(), GLIDE, "each frame reaches the widget");
    assert_eq!(gauge.glide.get(), 0);
    assert!(!root.needs_frame(), "a finished animation comes to rest");
}
