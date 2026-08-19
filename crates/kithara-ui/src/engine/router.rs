use kithara_platform::time::Instant;

use super::{
    component::RetainedComponent,
    model::{Emission, EngineEvent, Identity, Target},
};
use crate::interact::{CursorShape, Input, Outcome, PointerOwnership, PointerPhase};

#[derive(Default)]
pub(super) struct Router {
    capture: Option<Identity>,
    focus: Option<String>,
}

impl Router {
    pub(super) const fn captures_pointer(&self) -> bool {
        self.capture.is_some()
    }

    pub(super) fn captures(&self, path: &str) -> bool {
        self.capture
            .as_ref()
            .is_some_and(|identity| identity.path == path)
    }

    pub(super) fn focused_path(&self) -> Option<&str> {
        self.focus.as_deref()
    }

    pub(super) fn reconcile(&mut self, components: &[RetainedComponent]) {
        let missing = self.capture.as_ref().is_some_and(|identity| {
            !components
                .iter()
                .any(|component| component.has_identity(identity))
        });
        if missing {
            self.capture = None;
        }
        let missing = self.focus.as_ref().is_some_and(|path| {
            !components
                .iter()
                .any(|component| component.path() == path && component.focusable())
        });
        if missing {
            self.focus = None;
        }
    }

    pub(super) fn clear_focus(&mut self, components: &mut [RetainedComponent]) {
        let focus = self.focus.take();
        if let Some(component) = focus.as_deref().and_then(|path| {
            components
                .iter_mut()
                .find(|component| component.path() == path && component.focusable())
        }) {
            component.blur();
        }
        if self
            .capture
            .as_ref()
            .is_some_and(|identity| Some(identity.path.as_str()) == focus.as_deref())
        {
            self.capture = None;
        }
    }

    pub(super) fn handle(
        &mut self,
        components: &mut [RetainedComponent],
        input: Input<'_>,
        targets: &[Target<'_>],
        now: Instant,
    ) -> Option<Emission> {
        if matches!(
            input,
            Input::InputMethod(_) | Input::KeyPressed { .. } | Input::KeyReleased { .. }
        ) {
            let path = self.focus.as_deref()?;
            let component = components
                .iter_mut()
                .find(|component| component.path() == path && component.focusable())?;
            let (outcome, child) = component.handle_key(input);
            return emission(component.event_path().to_owned(), child, outcome);
        }
        if matches!(input, Input::ModifiersChanged(_)) {
            for target in targets {
                if let Some(component) = components
                    .iter_mut()
                    .find(|component| component.path() == target.path)
                {
                    let _ = component.handle(input, &target.hit, target.index, now);
                }
            }
            return None;
        }
        if matches!(
            input,
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Down
        ) {
            let focus = targets.iter().rev().find_map(|target| {
                if !target.hit.over() {
                    return None;
                }
                components
                    .iter()
                    .find(|component| component.path() == target.path && component.focusable())
                    .map(|component| component.path().to_owned())
            });
            if self.focus != focus {
                self.clear_focus(components);
                self.focus = focus;
            }
        }
        if let Some(identity) = &self.capture {
            let component_index = components
                .iter()
                .position(|component| component.has_identity(identity))?;
            let target = targets.iter().find(|target| target.path == identity.path)?;
            let component = &mut components[component_index];
            let (outcome, child) = component.handle(input, &target.hit, target.index, now);
            let path = component.event_path().to_owned();
            match outcome.ownership() {
                PointerOwnership::Release => self.capture = None,
                PointerOwnership::Claim | PointerOwnership::Unchanged => {}
            }
            return emission(path, child, outcome);
        }

        for target in targets.iter().rev() {
            let Some(component) = components
                .iter_mut()
                .find(|component| component.path() == target.path)
            else {
                continue;
            };
            let (outcome, child) = component.handle(input, &target.hit, target.index, now);
            if outcome == Outcome::IGNORED {
                continue;
            }
            match outcome.ownership() {
                PointerOwnership::Claim => self.capture = Some(component.identity()),
                PointerOwnership::Release | PointerOwnership::Unchanged => {}
            }
            return emission(component.event_path().to_owned(), child, outcome);
        }
        None
    }

    pub(super) fn cursor(
        &self,
        components: &[RetainedComponent],
        targets: &[Target<'_>],
    ) -> CursorShape {
        if let Some(identity) = &self.capture {
            let component = components
                .iter()
                .find(|component| component.has_identity(identity));
            let target = targets.iter().find(|target| target.path == identity.path);
            return component
                .zip(target)
                .map_or(CursorShape::None, |(component, target)| {
                    component.cursor(&target.hit)
                });
        }

        targets
            .iter()
            .rev()
            .filter_map(|target| {
                components
                    .iter()
                    .find(|component| component.path() == target.path)
                    .map(|component| component.cursor(&target.hit))
            })
            .find(|cursor| *cursor != CursorShape::None)
            .unwrap_or(CursorShape::None)
    }
}

fn emission(
    path: String,
    child: Option<&'static str>,
    outcome: Outcome<EngineEvent>,
) -> Option<Emission> {
    (outcome != Outcome::IGNORED).then_some(Emission {
        path,
        child,
        outcome,
    })
}
