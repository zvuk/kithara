/// The app menu's own view state: the surface and the one group it expands.
#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(in crate::gui) struct MenuState {
    #[field(get = is_open, vis = "pub(in crate::gui)", copy)]
    open: bool,
    #[field(get = are_layouts_open, vis = "pub(in crate::gui)", copy)]
    layouts_open: bool,
    #[field(get = are_modules_open, vis = "pub(in crate::gui)", copy)]
    modules_open: bool,
}

impl MenuState {
    pub(in crate::gui) const fn close(&mut self) {
        self.open = false;
    }

    pub(in crate::gui) const fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub(in crate::gui) const fn toggle_layouts(&mut self) {
        self.layouts_open = !self.layouts_open;
    }

    pub(in crate::gui) const fn toggle_modules(&mut self) {
        self.modules_open = !self.modules_open;
    }
}
