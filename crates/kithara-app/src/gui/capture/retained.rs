//! The studio photographed through the retained host into a Vello scene.

use std::rc::Rc;

use kithara::ui::{
    app::{Config, Ui},
    backends::paint_color,
    capture::{Geometry, Offscreen, Stage},
};

use super::{
    fixture::Fixture,
    page::{Page, PoolSample, pooled},
};
use crate::gui::ui::package::Package;

/// The studio drawn by the retained host into a Vello scene, one mounted
/// document at a time.
pub(super) struct Retained<'config> {
    pub(super) pools: String,
    config: Config<'config>,
    geometry: Geometry,
    off: Offscreen,
    open: Option<(Page, Ui<'config, Fixture>)>,
    package: Rc<Package>,
    pixels: Vec<u8>,
}

impl<'config> Retained<'config> {
    pub(super) fn new(
        package: Rc<Package>,
        config: Config<'config>,
        geometry: Geometry,
    ) -> Result<Self, String> {
        Ok(Self {
            config,
            geometry,
            off: Offscreen::new(geometry.width, geometry.height)?,
            open: None,
            package,
            pixels: Vec::new(),
            pools: String::new(),
        })
    }
}

impl Stage for Retained<'_> {
    type Page = Page;

    fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn shoot(&mut self) -> Result<&[u8], String> {
        let (page, ui) = self
            .open
            .as_mut()
            .ok_or_else(|| "no layout is open: turn to one before photographing".to_owned())?;
        let frame = ui
            .render()
            .map_err(|error| format!("draw {page}: {error}"))?;
        let background = paint_color(ui.background());
        let first = ui.draw_pool_stats();
        drop(
            ui.render()
                .map_err(|error| format!("second draw {page}: {error}"))?,
        );
        let sample = PoolSample {
            first,
            second: ui.draw_pool_stats(),
        };
        let page = *page;
        self.off
            .rasterise(&frame, self.geometry.scale, background, &mut self.pixels)?;
        pooled(&mut self.pools, page, &sample)?;
        Ok(&self.pixels)
    }

    fn tick(&mut self) {}

    fn turn(&mut self, page: &Page) -> Result<(), String> {
        let ui = Ui::new(
            Fixture::new(page.0, Rc::clone(&self.package)),
            self.config,
            (self.geometry.width, self.geometry.height),
            self.geometry.scale,
        )
        .map_err(|error| format!("mount {}: {error}", self.package.document(page.0)))?;
        self.open = Some((*page, ui));
        Ok(())
    }
}
