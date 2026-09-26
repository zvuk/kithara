use kithara_platform::time::Instant;

use super::super::{
    component::{PickerSnapshot, RetainedComponent, TextInputSnapshot},
    model::{Descriptor, Emission, Kind, Target},
    router::Router,
};
use crate::{
    draw::Rect,
    interact::{CursorShape, Input, InputMethodRequest},
};

#[derive(Default)]
pub(crate) struct Engine {
    router: Router,
    components: Vec<RetainedComponent>,
}

impl Engine {
    #[cfg(feature = "masonry")]
    pub(crate) fn clear_focus(&mut self) {
        self.router.clear_focus(&mut self.components);
    }

    #[cfg(feature = "masonry")]
    pub(crate) fn column_divider_value(&self, path: &str) -> Option<f32> {
        self.components
            .iter()
            .find(|component| component.path() == path && component.kind() == Kind::ColumnDivider)
            .and_then(RetainedComponent::column_divider_value)
    }

    pub(crate) fn cursor(&self, targets: &[Target<'_>]) -> CursorShape {
        self.router.cursor(&self.components, targets)
    }

    pub(crate) fn handle(
        &mut self,
        input: Input<'_>,
        targets: &[Target<'_>],
        now: Instant,
    ) -> Option<Emission> {
        self.router
            .handle(&mut self.components, input, targets, now)
    }

    pub(crate) fn has_pressed_item(&self) -> bool {
        self.components
            .iter()
            .any(|component| component.pressed_item_index().is_some())
    }

    pub(crate) fn input_method<'a>(
        &'a self,
        targets: &[Target<'_>],
    ) -> Option<InputMethodRequest<'a>> {
        let path = self.router.focused_path()?;
        let component = self
            .components
            .iter()
            .find(|component| component.path() == path && component.kind() == Kind::TextInput)?;
        let area = targets
            .iter()
            .find(|target| target.path == path)?
            .hit
            .area();
        component.input_method(area)
    }

    pub(crate) fn item_pressed(&self, path: &str) -> Option<Option<usize>> {
        self.components
            .iter()
            .find(|component| component.kind() == Kind::Item && component.event_path() == path)
            .map(RetainedComponent::pressed_item_index)
    }

    pub(crate) fn picker_snapshot(&self, path: &str) -> Option<PickerSnapshot> {
        self.components
            .iter()
            .find(|component| component.path() == path && component.kind() == Kind::Picker)
            .and_then(RetainedComponent::picker_snapshot)
    }

    pub(crate) fn pressed_item_index(&self, path: &str) -> Option<usize> {
        self.item_pressed(path).flatten()
    }

    pub(crate) fn reconcile(&mut self, descriptors: impl IntoIterator<Item = Descriptor>) {
        let mut retained = std::mem::take(&mut self.components);
        self.components = descriptors
            .into_iter()
            .map(|descriptor| {
                let retained_index = retained.iter().position(|component| {
                    component.path() == descriptor.path() && component.kind() == descriptor.kind()
                });
                match retained_index {
                    Some(index) => retained.remove(index).reconcile(descriptor),
                    None => descriptor.into(),
                }
            })
            .collect();
        self.router.reconcile(&self.components);
    }

    pub(crate) fn scroll_offset(&self, path: &str) -> Option<f32> {
        self.components
            .iter()
            .find(|component| component.path() == path && component.kind() == Kind::Scroll)
            .and_then(RetainedComponent::scroll_offset)
    }

    pub(crate) fn set_scroll_viewport(&mut self, path: &str, area: Rect) {
        if let Some(component) = self
            .components
            .iter_mut()
            .find(|component| component.path() == path && component.kind() == Kind::Scroll)
        {
            component.set_scroll_viewport(area);
        }
    }

    pub(crate) fn text_input_snapshot(&self, path: &str) -> Option<TextInputSnapshot> {
        let focused = self.router.focused_path() == Some(path);
        self.components
            .iter()
            .find(|component| component.path() == path && component.kind() == Kind::TextInput)
            .and_then(|component| component.text_input_snapshot(focused))
    }

    pub(crate) fn text_input_snapshots(&self) -> Vec<(String, TextInputSnapshot)> {
        let focused = self.router.focused_path();
        self.components
            .iter()
            .filter_map(|component| {
                component
                    .text_input_snapshot(focused == Some(component.path()))
                    .map(|snapshot| (component.path().to_owned(), snapshot))
            })
            .collect()
    }

    delegate::delegate! {
        to self.router {
            pub(crate) const fn captures_pointer(&self) -> bool;
            pub(crate) fn captures(&self, path: &str) -> bool;
            pub(crate) fn focused_path(&self) -> Option<&str>;
        }
    }
}
