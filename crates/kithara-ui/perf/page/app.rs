use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash as _, Hasher as _},
};

use kithara_ui::{
    app::App,
    builtin,
    render::{ReadValue, Reads, Skin, UiEvent},
};

use crate::{Page, Run, demo::DemoReads};

/// The gallery as an application, with the page it shows fixed by the harness.
///
/// The document never changes under measurement, so no frame is spent
/// recompiling: a page that turned itself mid-run would measure the mount.
pub(crate) struct PageApp {
    entry: &'static str,
    reads: DemoReads,
    /// Whether what the document publishes is fed back into the demo model.
    ///
    /// A drag is a closed loop: the pointer moves, the fader publishes a value,
    /// and the next frame draws the fader at it. Left open, the run measures a
    /// still page under a moving pointer. A wheel is not a loop - the viewport
    /// holds its own offset inside the host - and closing it there would change
    /// what the scroll runs measure: the retained library page publishes a
    /// selection with its scroll, and applying that scrolls the list back to it.
    closes_loop: bool,
    pub(crate) published: usize,
}

impl PageApp {
    pub(crate) fn new(page: &Page, run: Run) -> Self {
        let mut reads = DemoReads::default();
        reads.show(page.tab);
        if let Run::Buckets(count) = run {
            reads.set_wave_buckets(count);
        }
        Self {
            reads,
            closes_loop: matches!(run, Run::Moves(_)),
            entry: page.document(),
            published: 0,
        }
    }

    /// A fingerprint of one reading, so a run can show that the value under it
    /// really moved. Scalars and waveforms are the two shapes a gallery page
    /// animates.
    pub(crate) fn digest_of(&self, endpoint: &str) -> Option<u64> {
        let mut hasher = DefaultHasher::new();
        match self.reads.get(endpoint)? {
            ReadValue::Scalar(value) => value.to_bits().hash(&mut hasher),
            ReadValue::Waveform(view) => {
                view.buckets.len().hash(&mut hasher);
                for bucket in view.buckets {
                    bucket.low.to_bits().hash(&mut hasher);
                    bucket.mid.to_bits().hash(&mut hasher);
                    bucket.high.to_bits().hash(&mut hasher);
                }
            }
            _ => return None,
        }
        Some(hasher.finish())
    }
}

impl App for PageApp {
    fn document(&self) -> &str {
        self.entry
    }

    fn reads<R>(&self, with: impl FnOnce(&dyn Reads) -> R) -> R {
        with(&self.reads)
    }

    fn skin(&self) -> &Skin {
        builtin::skin()
    }

    fn tick(&mut self) {
        self.reads.tick();
    }

    fn update(&mut self, event: UiEvent) {
        self.published += 1;
        if self.closes_loop
            && let UiEvent::Control { path, action } = event
        {
            self.reads.apply(&path, &action);
        }
    }
}
