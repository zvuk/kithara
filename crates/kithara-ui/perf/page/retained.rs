use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash as _, Hasher as _},
};

use hotpath::measure_block;
use kithara_platform::time::Duration;
use kithara_ui::{
    app::{Config, Ui},
    draw::Pt,
    interact::{Input, MOUSE, PointerButton, PointerInput, PointerPhase, Scroll},
};

use crate::{
    Page, PageHost,
    app::PageApp,
    census::{Census, Natives, Pool, Scene},
    fixture::Consts,
    gpu::{RetainedGpu, height, readback_vello, width},
};

/// The retained host: the tree stays mounted, input walks it, and a frame is a
/// refresh, a paint into a Vello scene, and the two native passes around it.
pub(crate) struct Retained<'a> {
    gpu: &'a mut RetainedGpu,
    pointer_at: Pt,
    ui: Ui<'a, PageApp>,
    pub(crate) scheduled: bool,
    picture: u64,
}

impl<'a> Retained<'a> {
    pub(crate) fn new(
        config: Config<'a>,
        page: &Page,
        app: PageApp,
        gpu: &'a mut RetainedGpu,
    ) -> Self {
        let mut ui = Ui::new(app, config, (width(), height()), 1.0)
            .unwrap_or_else(|error| panic!("the page-perf fixture must mount: {error}"));
        page.open(&mut ui);
        Self {
            gpu,
            ui,
            picture: 0,
            scheduled: false,
            pointer_at: page.pointer_at,
        }
    }

    /// The seam the window runner draws through: settle, ask whether the
    /// picture would change, paint, run the passes, complete. The frame is
    /// painted either way so there is always a cost to report, and whether it
    /// was wanted is counted separately.
    fn draw(&mut self, fence: bool) -> Census {
        let before = self.ui.draw_pool_stats();
        measure_block!(
            "vello.frame",
            self.ui.frame(Duration::from_millis(Consts::STRESS_TICK_MS))
        );
        self.scheduled = self.ui.needs_frame();
        let frame = measure_block!(
            "vello.render",
            self.ui
                .render()
                .unwrap_or_else(|error| panic!("the retained host must draw: {error}"))
        );
        let encoding = frame.scene().encoding();
        let scene = Scene {
            draw_data: encoding.draw_data.len(),
            draw_tags: encoding.draw_tags.len(),
            path_data: encoding.path_data.len(),
            transforms: encoding.transforms.len(),
        };
        // The scene is where this host's own drawing ends: the coordinates, what
        // is painted with them, where each is placed, and the counts that say
        // how they are grouped. Past here the picture belongs to the rasteriser.
        self.picture = {
            let mut hasher = DefaultHasher::new();
            encoding.path_data.hash(&mut hasher);
            encoding.draw_data.hash(&mut hasher);
            for placed in &encoding.transforms {
                placed.matrix.map(f32::to_bits).hash(&mut hasher);
                placed.translation.map(f32::to_bits).hash(&mut hasher);
            }
            encoding.n_paths.hash(&mut hasher);
            encoding.n_path_segments.hash(&mut hasher);
            encoding.n_clips.hash(&mut hasher);
            hasher.finish()
        };
        let natives = Natives {
            shaders: frame.shaders().len(),
            vis: frame.vis().len(),
        };
        if fence {
            self.gpu.fenced_passes(&frame);
        } else {
            self.gpu.passes(&frame);
        }
        self.ui.complete_frame();
        Census {
            scene: Some(scene),
            natives: Some(natives),
            pool: Pool::delta(&before, &self.ui.draw_pool_stats()),
            scheduled: self.scheduled,
        }
    }
}

impl PageHost for Retained<'_> {
    fn drawn_as(&self) -> &'static str {
        "scene"
    }

    fn fenced_frame(&mut self) -> Census {
        self.draw(true)
    }

    fn frame(&mut self) -> Census {
        self.draw(false)
    }

    fn move_by(&mut self, dx: f32) -> f32 {
        self.pointer_at.x += dx;
        let at = self.pointer_at;
        self.ui.input(Input::Pointer(PointerInput::new(
            MOUSE,
            None,
            PointerPhase::Move,
            Some(at),
            1,
        )));
        at.x
    }

    /// The scene of the last frame drawn, fingerprinted where it was handed
    /// over, rather than the pixels Vello then made of it.
    fn picture(&mut self) -> u64 {
        self.picture
    }

    fn pixels(&mut self) -> Vec<u8> {
        readback_vello(&self.gpu.device, &self.gpu.queue, &self.gpu.texture)
    }

    fn place_pointer(&mut self) {
        let at = self.pointer_at;
        self.ui.input(Input::Pointer(PointerInput::new(
            MOUSE,
            None,
            PointerPhase::Move,
            Some(at),
            1,
        )));
    }

    fn press(&mut self) {
        let at = self.pointer_at;
        self.ui.input(Input::Pointer(PointerInput::new(
            MOUSE,
            Some(PointerButton::Primary),
            PointerPhase::Down,
            Some(at),
            1,
        )));
    }

    fn published(&self) -> usize {
        self.ui.app().published
    }

    fn reading(&self, endpoint: Option<&str>) -> Option<u64> {
        self.ui.app().digest_of(endpoint?)
    }

    fn release(&mut self) {
        let at = self.pointer_at;
        self.ui.input(Input::Pointer(PointerInput::new(
            MOUSE,
            Some(PointerButton::Primary),
            PointerPhase::Up,
            Some(at),
            1,
        )));
    }

    fn wheel(&mut self, lines: f32) {
        self.ui
            .input(Input::Wheel(Scroll::Lines { x: 0.0, y: lines }));
    }
}
