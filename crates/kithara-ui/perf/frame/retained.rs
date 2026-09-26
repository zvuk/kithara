use std::{cell::Cell, rc::Rc};

use kithara_platform::time::Duration;
use kithara_ui::{
    app::{App, Ui},
    builtin,
    interact::PointerPhase,
    render::{Reads, Skin, UiEvent},
};

use crate::{
    FrameHost, Step,
    fixture::{CensusReads, Fixture},
    packet,
    scenarios::Consts,
};

/// The retained host: the tree stays mounted, input walks it, and a frame is a
/// refresh plus a paint pass into a Vello scene.
pub(crate) struct Retained<'fixture> {
    published: Rc<Cell<usize>>,
    reads: Rc<CensusReads>,
    ui: Ui<'fixture, CensusApp>,
    scheduled: bool,
}

impl<'fixture> Retained<'fixture> {
    pub(crate) fn new(fixture: &'fixture Fixture, reads: Rc<CensusReads>) -> Self {
        let published = Rc::new(Cell::new(0));
        let app = CensusApp {
            published: Rc::clone(&published),
            reads: Rc::clone(&reads),
        };
        let size = (u32::from(Consts::WIDTH), u32::from(Consts::HEIGHT));
        let ui = Ui::new(app, fixture.config(), size, 1.0)
            .unwrap_or_else(|error| panic!("the frame-perf fixture must mount: {error}"));
        Self {
            published,
            reads,
            ui,
            scheduled: false,
        }
    }
}

impl FrameHost for Retained<'_> {
    /// The seam the window runner draws through: settle, ask whether the
    /// picture would change, paint. The frame is painted either way here so
    /// there is always a cost to report, and whether it was wanted is
    /// [`Self::scheduled`].
    fn frame(&mut self) -> usize {
        self.ui.frame(Duration::from_millis(16));
        self.scheduled = self.ui.needs_frame();
        let frame = self
            .ui
            .render()
            .unwrap_or_else(|error| panic!("the retained host must draw: {error}"));
        let encoding = frame.scene().encoding();
        let size = encoding.path_data.len() + encoding.draw_data.len();
        self.ui.complete_frame();
        size
    }

    fn interact(&mut self, step: Step) {
        match step {
            Step::Move(at) => self.ui.input(packet(PointerPhase::Move, Some(at))),
            Step::Press => self.ui.input(packet(PointerPhase::Down, None)),
            Step::Release => self.ui.input(packet(PointerPhase::Up, None)),
            Step::Data => self.reads.bump(),
        }
    }

    fn receipts(&self) -> usize {
        self.published.get()
    }

    fn scheduled(&self) -> bool {
        self.scheduled
    }
}

/// The application the retained host drives. It shows one document and answers
/// the same readings the immediate host is handed directly.
struct CensusApp {
    published: Rc<Cell<usize>>,
    reads: Rc<CensusReads>,
}

impl App for CensusApp {
    fn document(&self) -> &str {
        Consts::LAYOUT
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(self.reads.as_ref())
    }

    fn skin(&self) -> &Skin {
        builtin::skin()
    }

    fn update(&mut self, _event: UiEvent) {
        self.published.set(self.published.get() + 1);
    }
}
