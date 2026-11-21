use std::rc::Rc;

use kithara_platform::time::Instant;
use masonry::core::EventCtx;

use crate::{
    draw::{DrawListBuilder, Rect},
    hosts::{
        event::control_event,
        surface::{Entry, SteppingSurface},
    },
    interact::{CursorShape, Hit, Input, PointerOwnership},
    masonry::retained::custom::HostAction,
    render::{Published, ReadValue, Skin},
};

/// The stepping surface a flow declares over itself: a detent anywhere on it
/// moves the value the document named, and so does a held drag.
///
/// The gesture is the one the immediate host puts over the same flow, so the
/// two hosts step alike; only the way it is mounted differs. Where the document
/// declares an entry, `entry` holds the text the number field opens with and
/// the skin it is drawn in.
pub(crate) struct Detent {
    pub(crate) surface: SteppingSurface,
    entry: Option<(String, Skin)>,
    map_event: Rc<dyn Fn(Published) -> HostAction>,
    path: String,
}

impl Detent {
    pub(crate) fn new(
        path: String,
        map_event: Rc<dyn Fn(Published) -> HostAction>,
        entry: Option<(String, Skin)>,
    ) -> Self {
        Self {
            surface: SteppingSurface::default(),
            entry,
            map_event,
            path,
        }
    }

    pub(crate) const fn declares_entry(&self) -> bool {
        self.entry.is_some()
    }

    pub(crate) fn paint(&self, bounds: Rect, list: &mut DrawListBuilder) {
        if let Some((_, skin)) = &self.entry {
            self.surface.paint(list, bounds, skin);
        }
    }

    /// Keeps the text the next double-click opens the field holding.
    pub(crate) fn show(&mut self, value: &ReadValue<'_>) {
        if let (Some((held, _)), ReadValue::Text(text)) = (&mut self.entry, value)
            && held.as_str() != *text
        {
            (*text).clone_into(held);
        }
    }

    /// Offers one input to this surface, answering whether the surface took it.
    ///
    /// The surface holds the pointer for as long as the drag lasts. Routing
    /// here is hit-tested, so without that a drag walking off the flow would
    /// stop arriving and the release ending it would land on whatever the
    /// pointer wandered onto, leaving this one armed for the next hover.
    pub(crate) fn step(&mut self, ctx: &mut EventCtx<'_>, input: Input<'_>, hit: &Hit) -> bool {
        let held = self.surface.dragging();
        let was_editing = self.surface.editing();
        let entry = self
            .entry
            .as_ref()
            .map(|(value, skin)| Entry { value, skin });
        let outcome = self.surface.on_input(input, hit, Instant::now(), entry);
        sync_surface(
            ctx,
            held,
            was_editing,
            self.surface.editing(),
            outcome.ownership(),
        );
        let taken = outcome.is_captured() || held;
        if let Some(action) = outcome.value() {
            ctx.submit_action::<HostAction>((self.map_event)(control_event(&self.path, action)));
        }
        if taken {
            ctx.set_handled();
        }
        taken
    }

    delegate::delegate! {
        to self.surface {
            /// Closes the number field unchanged, answering whether one stood open.
            pub(crate) fn close(&mut self) -> bool;
            /// The hand this surface asks for, matched to the immediate host.
            pub(crate) fn cursor(&self, hit: &Hit) -> CursorShape;
        }
    }
}

fn sync_surface(
    ctx: &mut EventCtx<'_>,
    held: bool,
    was_editing: bool,
    now_editing: bool,
    ownership: PointerOwnership,
) {
    if was_editing || now_editing {
        ctx.request_paint_only();
    }
    match (was_editing, now_editing) {
        (false, true) => ctx.request_focus(),
        (true, false) => ctx.resign_focus(),
        _ => {}
    }
    match ownership {
        PointerOwnership::Claim => ctx.capture_pointer(),
        PointerOwnership::Release if held => ctx.release_pointer(),
        PointerOwnership::Unchanged | PointerOwnership::Release => {}
    }
}
