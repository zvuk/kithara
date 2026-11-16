//! The cache group every canvas in this toolkit draws into.
//!
//! A geometry cache carries a group, and the renderer keeps one text atlas per
//! group. A cache built with `Cache::new` gets a group of its own, so a window
//! with a dozen canvases would stand up a dozen atlases, each growing to its
//! own full size and holding it for the life of the process — the same glyphs,
//! paid for once per canvas.
//!
//! One shared group is therefore not a micro-optimisation but the difference
//! between one atlas and one per canvas. Nothing here decides *when* a picture
//! is rebuilt: that stays with each canvas, which knows what its own picture
//! is made of.

use std::sync::OnceLock;

use iced::{
    Rectangle, Renderer, Size,
    advanced::graphics::cache::{Cache as ValueCache, Cached, State},
    widget::canvas::{Frame, Geometry, Group},
};

use crate::backends::finish;

#[derive(Debug, Clone)]
struct Prepared {
    bounds: Rectangle,
    geometry: <Geometry as Cached>::Cache,
}

/// Geometry is linearized once before entering the shared renderer cache.
#[derive(Debug)]
pub(crate) struct Cache {
    inner: ValueCache<Prepared>,
}

impl Cache {
    pub(crate) fn clear(&self) {
        self.inner.clear();
    }

    pub(crate) fn draw(
        &self,
        renderer: &Renderer,
        size: Size,
        draw: impl FnOnce(&mut Frame),
    ) -> Geometry {
        self.draw_with_bounds(renderer, Rectangle::with_size(size), draw)
    }

    pub(crate) fn draw_with_bounds(
        &self,
        renderer: &Renderer,
        bounds: Rectangle,
        draw: impl FnOnce(&mut Frame),
    ) -> Geometry {
        let previous = match &*self.inner.state().borrow() {
            State::Filled { current } if current.bounds == bounds => {
                return <Geometry as Cached>::load(&current.geometry);
            }
            State::Filled { current } => Some(current.geometry.clone()),
            State::Empty { previous } => previous.as_ref().map(|value| value.geometry.clone()),
        };
        let mut frame = Frame::with_bounds(renderer, bounds);
        draw(&mut frame);
        let geometry = finish(frame.into_geometry()).cache(self.inner.group(), previous);
        let result = <Geometry as Cached>::load(&geometry);
        self.inner.put(Prepared { bounds, geometry });
        result
    }
}

/// The group shared by every canvas cache this toolkit builds.
fn group() -> Group {
    static GROUP: OnceLock<Group> = OnceLock::new();
    *GROUP.get_or_init(Group::unique)
}

/// An empty canvas cache in the shared group.
pub(crate) fn canvas() -> Cache {
    Cache {
        inner: ValueCache::with_group(group()),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use iced::{Color, Font, Pixels, Point, Renderer, Size, widget::canvas::Frame};
    use kithara_test_utils::kithara;

    use super::canvas;

    #[kithara::test]
    fn geometry_is_prepared_only_after_invalidation_or_bounds_change() {
        let renderer =
            Renderer::Secondary(iced_tiny_skia::Renderer::new(Font::DEFAULT, Pixels(16.0)));
        let cache = canvas();
        let count = Cell::new(0);
        let draw = |frame: &mut Frame| {
            count.set(count.get() + 1);
            frame.fill_rectangle(Point::ORIGIN, Size::new(8.0, 8.0), Color::WHITE);
        };
        cache.draw(&renderer, Size::new(32.0, 32.0), draw);
        cache.draw(&renderer, Size::new(32.0, 32.0), |_| {
            panic!("a cache hit must not prepare geometry")
        });
        cache.clear();
        cache.draw(&renderer, Size::new(32.0, 32.0), draw);
        cache.draw(&renderer, Size::new(64.0, 32.0), draw);
        assert_eq!(count.get(), 3);
    }
}
