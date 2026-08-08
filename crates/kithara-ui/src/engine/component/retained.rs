use kithara_platform::time::Instant;

use super::{
    activation::ActivationComponent,
    crossing::CrossingComponent,
    item::ItemComponent,
    picker::{PickerComponent, PickerSnapshot},
    scalar::ScalarComponent,
    scroll::ScrollComponent,
    segmented::SegmentedComponent,
    text_input::{TextInputComponent, TextInputSnapshot},
    wave::HeroWaveComponent,
};
use crate::{
    engine::model::{Descriptor, EngineEvent, Identity, Kind},
    interact::{
        CursorShape, Hit, Hover, Input, InputMethodRequest, Outcome, PointerOwnership,
        PointerPhase, Rect,
        recognizers::{Scalar, Track, WheelStep},
    },
};

pub(super) trait Component {
    fn path(&self) -> &str;
    fn event_path(&self) -> &str {
        self.path()
    }
    fn kind(&self) -> Kind;
    fn handle(
        &mut self,
        input: Input<'_>,
        hit: &Hit,
        index: Option<usize>,
        now: Instant,
    ) -> (Outcome<EngineEvent>, Option<&'static str>);
    fn handle_key(&mut self, _input: Input<'_>) -> (Outcome<EngineEvent>, Option<&'static str>) {
        (Outcome::IGNORED, None)
    }
    fn cursor(&self, hit: &Hit) -> CursorShape;
    fn captures_pointer(&self) -> bool;
    fn cancel_pointer(&mut self) {}
    fn focusable(&self) -> bool {
        false
    }
    fn blur(&mut self) {}
}

pub(in crate::engine) enum RetainedComponent {
    Scalar(ScalarComponent),
    Activation(ActivationComponent),
    Crossing(CrossingComponent),
    Segmented(SegmentedComponent),
    Picker(PickerComponent),
    TextInput(TextInputComponent),
    Scroll(ScrollComponent),
    Item(ItemComponent),
    HeroWave(HeroWaveComponent),
}

impl RetainedComponent {
    pub(in crate::engine) fn reconcile(self, descriptor: Descriptor) -> Self {
        let next = descriptor.into();
        match (self, next) {
            (Self::Scalar(component), Self::Scalar(next)) => {
                Self::Scalar(component.reconcile(next))
            }
            (Self::HeroWave(component), Self::HeroWave(next)) => {
                Self::HeroWave(component.reconcile(next))
            }
            (Self::Scroll(component), Self::Scroll(next)) => {
                Self::Scroll(component.reconcile(next))
            }
            (Self::Picker(component), Self::Picker(next)) => {
                Self::Picker(component.reconcile(next))
            }
            (Self::TextInput(component), Self::TextInput(next)) => {
                Self::TextInput(component.reconcile(next))
            }
            (Self::Item(component), Self::Item(next)) => Self::Item(component.reconcile(next)),
            (Self::Crossing(component), Self::Crossing(_)) => Self::Crossing(component),
            (_, next) => next,
        }
    }

    pub(in crate::engine) fn identity(&self) -> Identity {
        Identity {
            path: self.path().to_owned(),
            kind: self.kind(),
        }
    }

    pub(in crate::engine) fn has_identity(&self, identity: &Identity) -> bool {
        self.kind() == identity.kind && self.path() == identity.path
    }

    delegate::delegate! {
        to self {
            #[expr($.path())]
            #[call(component)]
            pub(in crate::engine) fn path(&self) -> &str;
            #[expr($.kind())]
            #[call(component)]
            pub(in crate::engine) fn kind(&self) -> Kind;
            #[expr($.event_path())]
            #[call(component)]
            pub(in crate::engine) fn event_path(&self) -> &str;
            #[expr($.captures_pointer())]
            #[call(component)]
            pub(in crate::engine) fn captures_pointer(&self) -> bool;
            #[expr($.focusable())]
            #[call(component)]
            pub(in crate::engine) fn focusable(&self) -> bool;
        }
    }

    pub(in crate::engine) fn handle(
        &mut self,
        input: Input<'_>,
        hit: &Hit,
        index: Option<usize>,
        now: Instant,
    ) -> (Outcome<EngineEvent>, Option<&'static str>) {
        let had_pointer = self.captures_pointer();
        let (outcome, child) = self.component_mut().handle(input, hit, index, now);
        if matches!(
            input,
            Input::Pointer(pointer)
                if matches!(pointer.phase, PointerPhase::Cancel | PointerPhase::DoubleClick)
        ) {
            self.component_mut().cancel_pointer();
        }
        let ownership = match (had_pointer, self.captures_pointer()) {
            (false, true) => PointerOwnership::Claim,
            (true, false) => PointerOwnership::Release,
            _ => PointerOwnership::Unchanged,
        };
        (outcome.with_ownership(ownership), child)
    }

    pub(in crate::engine) fn cursor(&self, hit: &Hit) -> CursorShape {
        self.component().cursor(hit)
    }

    pub(in crate::engine) fn handle_key(
        &mut self,
        input: Input<'_>,
    ) -> (Outcome<EngineEvent>, Option<&'static str>) {
        self.component_mut().handle_key(input)
    }

    pub(in crate::engine) fn blur(&mut self) {
        self.component_mut().blur();
    }

    pub(in crate::engine) fn picker_snapshot(&self) -> Option<PickerSnapshot> {
        if let Self::Picker(component) = self {
            Some(component.snapshot())
        } else {
            None
        }
    }

    pub(in crate::engine) fn text_input_snapshot(
        &self,
        focused: bool,
    ) -> Option<TextInputSnapshot> {
        if let Self::TextInput(component) = self {
            Some(component.snapshot(focused))
        } else {
            None
        }
    }

    pub(in crate::engine) fn input_method(&self, area: Rect) -> Option<InputMethodRequest<'_>> {
        if let Self::TextInput(component) = self {
            Some(component.input_method(area))
        } else {
            None
        }
    }

    pub(in crate::engine) fn scroll_offset(&self) -> Option<f32> {
        if let Self::Scroll(component) = self {
            Some(component.offset())
        } else {
            None
        }
    }

    pub(in crate::engine) fn pressed_item_index(&self) -> Option<usize> {
        if let Self::Item(component) = self {
            component.pressed_index()
        } else {
            None
        }
    }

    pub(in crate::engine) fn set_scroll_viewport(&mut self, area: Rect) {
        if let Self::Scroll(component) = self {
            component.set_viewport(area);
        }
    }

    fn component(&self) -> &dyn Component {
        match self {
            Self::Scalar(component) => component,
            Self::Activation(component) => component,
            Self::Crossing(component) => component,
            Self::Segmented(component) => component,
            Self::Picker(component) => component,
            Self::TextInput(component) => component,
            Self::Scroll(component) => component,
            Self::Item(component) => component,
            Self::HeroWave(component) => component,
        }
    }

    fn component_mut(&mut self) -> &mut dyn Component {
        match self {
            Self::Scalar(component) => component,
            Self::Activation(component) => component,
            Self::Crossing(component) => component,
            Self::Segmented(component) => component,
            Self::Picker(component) => component,
            Self::TextInput(component) => component,
            Self::Scroll(component) => component,
            Self::Item(component) => component,
            Self::HeroWave(component) => component,
        }
    }
}

impl From<Descriptor> for RetainedComponent {
    fn from(descriptor: Descriptor) -> Self {
        match descriptor {
            Descriptor::Activation { path } => Self::Activation(ActivationComponent::new(path)),
            Descriptor::Crossing { path } => Self::Crossing(CrossingComponent::new(path)),
            Descriptor::Segmented { path, item_count } => {
                Self::Segmented(SegmentedComponent::new(path, item_count))
            }
            Descriptor::Picker {
                path,
                item_count,
                selected,
            } => Self::Picker(PickerComponent::new(path, item_count, selected)),
            Descriptor::TextInput {
                path,
                query,
                layout,
            } => Self::TextInput(TextInputComponent::new(path, query, layout)),
            Descriptor::Scroll { path, config } => Self::Scroll(ScrollComponent::new(path, config)),
            Descriptor::Item {
                target,
                path,
                count,
            } => Self::Item(ItemComponent::new(target, path, count)),
            Descriptor::ColumnDivider { path, scalar } => Self::Scalar(ScalarComponent::new(
                path,
                Kind::ColumnDivider,
                scalar,
                None,
            )),
            Descriptor::Fader {
                path,
                scalar,
                drag_step,
            } => Self::Scalar(ScalarComponent::new(path, Kind::Fader, scalar, drag_step)),
            Descriptor::Crossfader { path } => Self::Scalar(ScalarComponent::new(
                path,
                Kind::Crossfader,
                Scalar::builder()
                    .track(Track::AbsoluteHorizontal)
                    .hover(Hover::new(CursorShape::ResizeH))
                    .build(),
                None,
            )),
            Descriptor::Knob {
                path,
                current,
                drag_range,
                wheel_step,
            } => Self::Scalar(ScalarComponent::new(
                path,
                Kind::Knob,
                Scalar::builder()
                    .track(Track::RelativeVertical {
                        range: drag_range,
                        value: current,
                    })
                    .hover(Hover::new(CursorShape::ResizeV))
                    .reset(0.5)
                    .wheel(WheelStep {
                        value: current,
                        step: wheel_step,
                    })
                    .build(),
                None,
            )),
            Descriptor::StereoMeter { path } => Self::Scalar(ScalarComponent::new(
                path,
                Kind::StereoMeter,
                Scalar::builder()
                    .track(Track::AbsoluteHorizontal)
                    .hover(Hover::new(CursorShape::ResizeH))
                    .build(),
                None,
            )),
            Descriptor::VerticalVu { path } => Self::Scalar(ScalarComponent::new(
                path,
                Kind::VerticalVu,
                Scalar::builder()
                    .track(Track::AbsoluteVertical)
                    .hover(Hover::new(CursorShape::ResizeV))
                    .build(),
                None,
            )),
            Descriptor::Wave { path } => Self::Scalar(ScalarComponent::new(
                path,
                Kind::Wave,
                Scalar::builder()
                    .track(Track::HorizontalClick)
                    .hover(Hover::new(CursorShape::Pointer))
                    .build(),
                None,
            )),
            Descriptor::HeroWave {
                path,
                scale,
                progress,
                visible,
                wheel_positive,
                wheel_non_positive,
            } => Self::HeroWave(HeroWaveComponent::new(
                path,
                scale,
                progress,
                visible,
                wheel_positive,
                wheel_non_positive,
            )),
        }
    }
}
