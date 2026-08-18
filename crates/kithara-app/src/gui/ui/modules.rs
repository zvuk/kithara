use std::collections::BTreeSet;

/// Panes the menu can switch off. The bar and the decks carry the controls the
/// app cannot run without, so only these three leave the layout.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(in crate::gui) struct Modules {
    off: BTreeSet<&'static str>,
    #[field(get, vis = "pub(in crate::gui)")]
    count: String,
}

impl Modules {
    const ALL: [&'static str; 4] = ["ov", "mix", "lib", "cpu"];

    pub(in crate::gui) fn on(&self) -> usize {
        Self::ALL.len() - self.off.len()
    }

    pub(in crate::gui) fn is_on(&self, module: &str) -> bool {
        Self::key(module).is_some_and(|key| !self.off.contains(key))
    }

    pub(in crate::gui) fn toggle(&mut self, module: &str) {
        let Some(key) = Self::key(module) else {
            return;
        };
        if !self.off.remove(key) {
            self.off.insert(key);
        }
        self.count = Self::count_label(self.on());
    }

    fn key(module: &str) -> Option<&'static str> {
        Self::ALL.into_iter().find(|key| *key == module)
    }

    fn count_label(on: usize) -> String {
        format!("{on} OF {}", Self::ALL.len())
    }
}

impl Default for Modules {
    fn default() -> Self {
        Self {
            off: BTreeSet::new(),
            count: Self::count_label(Self::ALL.len()),
        }
    }
}
