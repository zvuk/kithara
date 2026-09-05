//! Re-reading a standing tree, which is what this host does instead of
//! rebuilding one.
//!
//! The other host rebuilds its whole element tree every frame, so a value that
//! moved reaches it for free. This one keeps the tree it mounted, so every
//! value the document reads has to be carried into the standing widget by
//! hand, and everything that is carried is named here.

use masonry::core::WidgetId;

use super::{MasonryRoot, Node, Watched, WindowLayer};
use crate::render::document::{Ctx, placements};

impl<Action> MasonryRoot<Action>
where
    Action: std::fmt::Debug + Send + 'static,
{
    /// Shows what the pointer is carrying now.
    ///
    /// The ghost is a value the window layer draws, not shape the layer was
    /// mounted with: the layer stands for the life of the window, and what the
    /// pointer carries changes under it.
    fn carry_ghost(&mut self, ctx: Ctx<'_, '_>) -> bool {
        let Some(window) = &mut self.window else {
            return false;
        };
        let Some(layer) = window.layer else {
            return false;
        };
        let label = ctx.label(window.carried.as_ref());
        window.carrying = label.is_some();
        self.root.edit_widget(layer, |mut widget| {
            let mut window = widget.downcast::<WindowLayer>();
            let carried = window.widget.carry(label);
            if carried {
                window.ctx.request_paint_only();
            }
            carried
        })
    }

    /// Opens the surfaces the document now holds open, and shuts the rest.
    ///
    /// This is the one thing a mounted surface cannot answer for itself. Every
    /// other read reaches a leaf that is already standing, and re-reading it
    /// changes what that leaf shows; a popover opening changes nothing inside
    /// its content, only whether the content stands in the picture. So the flag
    /// is read here, against the layer the content was mounted into.
    fn open_surfaces(&mut self, ctx: Ctx<'_, '_>) {
        let changed: Vec<WidgetId> = self
            .popovers
            .iter()
            .filter(|popover| ctx.flag(Some(&popover.flag)) != popover.state.is_open())
            .map(|popover| {
                popover.state.latch(!popover.state.is_open());
                popover.layer
            })
            .collect();
        for layer in changed {
            self.root.edit_widget(layer, |mut layer| {
                layer.ctx.request_layout();
            });
        }
    }

    /// Walks the document again for the poses alone, and moves whatever the
    /// walk now puts somewhere else.
    ///
    /// Nothing is watched this way unless the document declares an object an
    /// endpoint drives, so a page that never moves pays for none of this.
    fn place_objects(&mut self, ctx: Ctx<'_, '_>) -> bool {
        if !ctx.ui.driven {
            return false;
        }
        let placed = placements(&ctx.ui.root, ctx);
        let mut moved = false;
        for watched in &self.watched {
            let Watched::Placed { id, path } = watched else {
                continue;
            };
            let Some(transform) = placed.get(path).copied() else {
                continue;
            };
            moved |= self.root.edit_widget(*id, |mut widget| {
                let mut node = widget.downcast::<Node>();
                let moved = node.widget.place(transform);
                if moved {
                    node.ctx.request_paint_only();
                }
                moved
            });
        }
        moved
    }

    /// Re-reads everything the mounted document shows and hands it to the
    /// widget that draws it.
    ///
    /// This is what a rebuild was doing, minus the rebuild: the tree stays, so a
    /// gesture in flight and the pointer capture that feeds it both survive, and
    /// every control bound to the same endpoint moves together rather than one
    /// of them being poked by hand.
    ///
    /// Two kinds of thing change between frames without the document changing.
    /// A control's *value* comes from an endpoint the control names, and is
    /// re-read one control at a time. A control's *pose* comes from the objects
    /// around it, and is worked out by the document walk rather than named
    /// anywhere, so it takes a walk to re-read — one for the whole document.
    pub fn refresh(&mut self, ctx: Ctx<'_, '_>) {
        let shown = self.show_values(ctx);
        self.reread_plans(ctx);
        let placed = self.place_objects(ctx);
        self.open_surfaces(ctx);
        self.stand_blocks(ctx);
        let carried = self.carry_ghost(ctx);
        self.moved = shown || placed || carried;
    }

    /// Carries the frame just read into the gestures already mounted.
    ///
    /// A control answers a hand against what it is showing, and what it is
    /// showing changes without the tree changing shape. The immediate host
    /// resolves that afresh every frame because it rebuilds; this one re-reads
    /// it in place.
    fn reread_plans(&mut self, ctx: Ctx<'_, '_>) {
        for engine in &self.engines {
            engine.reread(ctx);
        }
    }

    fn show_values(&mut self, ctx: Ctx<'_, '_>) -> bool {
        let mut moved = false;
        for watched in &self.watched {
            match watched {
                Watched::Read { id, binding } => {
                    let Some(value) = ctx.read(binding) else {
                        continue;
                    };
                    moved |= self.root.edit_widget(*id, |mut widget| {
                        let mut node = widget.downcast::<Node>();
                        let shown = node.widget.show_live(&value);
                        if shown {
                            node.ctx.request_paint_only();
                        }
                        shown
                    });
                }
                Watched::Snapshot { id } => {
                    moved |= self.root.edit_widget(*id, |mut widget| {
                        let mut node = widget.downcast::<Node>();
                        let shown = node.widget.refresh(ctx);
                        if shown {
                            node.ctx.request_paint_only();
                        }
                        shown
                    });
                }
                Watched::Spot { id, binding } => {
                    let Some(at) = ctx.point(Some(binding)) else {
                        continue;
                    };
                    moved |= self.root.edit_widget(*id, |mut widget| {
                        let mut node = widget.downcast::<Node>();
                        let moved = node.widget.move_spot(at);
                        if moved {
                            node.ctx.request_layout();
                        }
                        moved
                    });
                }
                Watched::Lit { id, flag } => {
                    let on = ctx.flag(Some(flag));
                    moved |= self.root.edit_widget(*id, |mut widget| {
                        let mut node = widget.downcast::<Node>();
                        let lit = node.widget.light(on);
                        if lit {
                            node.ctx.request_paint_only();
                        }
                        lit
                    });
                }
                Watched::Placed { .. } => {}
            }
        }
        moved
    }

    /// Shows the blocks the document now shows, and hides the rest.
    ///
    /// A block is the same kind of thing as a surface opening: re-reading a
    /// leaf changes what that leaf shows, while a block changes whether a
    /// whole subtree stands in the picture at all. The flow above it hides it
    /// the way it hides a child the room did not reach, so all this does is
    /// tell the flow to lay itself out again once the answer has changed.
    fn stand_blocks(&mut self, ctx: Ctx<'_, '_>) {
        let changed: Vec<WidgetId> = self
            .blocks
            .iter()
            .filter(|block| block.state.latch(ctx.flag(Some(&block.hidden))))
            .map(|block| block.flow)
            .collect();
        for flow in changed {
            self.root.edit_widget(flow, |mut flow| {
                flow.ctx.request_layout();
            });
        }
    }
}
