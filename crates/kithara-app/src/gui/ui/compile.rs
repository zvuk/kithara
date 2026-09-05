use std::rc::Rc;

use iced::{Element, Size};
use kithara::{
    platform::time::Duration,
    ui::{
        compile::{CompiledUi, compile},
        error::UiDocError,
        ids::SourceUri,
        render::{Clock, Walk, tree},
        source::UiConfig,
        view::ViewState,
    },
};

use super::{
    cache::{DeckLayout, ViewCache},
    endpoints::Registry,
    package::Package,
};
use crate::gui::{app::Kithara, message::Message, reads::ReadRoot};

/// The compiled UI plus the host-owned view state it reads back. Both
/// deck layouts are compiled once; the top bar picks which one renders.
pub(crate) struct AppUi {
    pub(crate) cache: ViewCache,
    /// The package every page here was read, dressed and worded by. A host
    /// that has to build its own window reads it from here rather than
    /// loading a second copy.
    pub(in crate::gui) package: Rc<Package>,
    /// This host's own reading of time, advanced once per tick so a document
    /// bound to it animates without the application keeping a timer of its own.
    clock: Clock,
    dual: CompiledUi,
    single: CompiledUi,
    /// State the documents keep for themselves, which no endpoint of this
    /// application declares or answers.
    view: ViewState,
}

impl AppUi {
    pub(crate) fn new(package: Rc<Package>, doc: &UiConfig) -> Result<Self, UiDocError> {
        let view = ViewState::default();
        Ok(Self {
            single: compile_screen(&package, DeckLayout::Single, doc, &view)?,
            dual: compile_screen(&package, DeckLayout::Dual, doc, &view)?,
            cache: ViewCache::default(),
            clock: Clock::default(),
            package,
            view,
        })
    }

    /// Moves this host's clock on by one tick of `step`.
    pub(crate) fn advance(&mut self, step: Duration) {
        self.clock = self.clock.advance(step);
    }

    const fn compiled(&self, layout: DeckLayout) -> &CompiledUi {
        screen(&self.single, &self.dual, layout)
    }

    /// Applies whatever the press at `path` writes to the screen's own state.
    ///
    /// The application is told about the press all the same; this is only the
    /// part of it no application declared an endpoint for.
    pub(super) fn press(&mut self, path: &str) {
        let Self {
            dual,
            single,
            view,
            cache,
            ..
        } = self;
        if let Some((state, write)) = screen(single, dual, cache.layout()).views().at(path) {
            view.apply(state, write);
        }
    }

    pub(crate) fn window_min(&self) -> Size {
        Size::new(
            self.single.min.w.min().max(self.dual.min.w.min()),
            self.single.min.h.min().max(self.dual.min.h.min()),
        )
    }
}

const fn screen<'a>(
    single: &'a CompiledUi,
    dual: &'a CompiledUi,
    layout: DeckLayout,
) -> &'a CompiledUi {
    match layout {
        DeckLayout::Single => single,
        DeckLayout::Dual => dual,
    }
}

#[cfg(test)]
pub(in crate::gui) fn compile_ui(layout: DeckLayout) -> Result<CompiledUi, UiDocError> {
    compile_screen(
        Package::load(None)?.as_ref(),
        layout,
        &UiConfig::default(),
        &kithara::ui::view::EMPTY,
    )
}

fn compile_screen(
    package: &Package,
    layout: DeckLayout,
    doc: &UiConfig,
    view: &ViewState,
) -> Result<CompiledUi, UiDocError> {
    let document = package.document(layout);
    let ui = compile(
        document,
        package.resolver(),
        &Registry::default(),
        package.skin().document(),
        package.text(),
        doc,
        view,
    )?;
    ui.require_paths(Package::REQUIRED, &SourceUri(document.to_owned()))?;
    Ok(ui)
}

pub(crate) fn view(state: &Kithara) -> Element<'_, Message> {
    let root = ReadRoot::new(state);
    let reads = Walk::new(&root);
    let compiled = state.ui.compiled(state.ui.cache.layout());
    tree::render(
        &compiled.root,
        compiled,
        &reads,
        &state.ui.view,
        state.ui.package.skin(),
        state.ui.clock,
        None,
    )
    .map(Message::Ui)
}
