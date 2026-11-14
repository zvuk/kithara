use super::{
    ViewWrite, ViewWrites,
    census::{Declared, Target},
};
use crate::{
    interact::recognizers::Edge,
    module::ViewSet,
    render::{ControlAction, Published, ReadValue, Reads, UiEvent, WriteValue},
    view::ViewState,
};

impl ViewWrites {
    /// Settles what the document published into `view` and the host's event.
    /// `reads` supplies the interval's other end.
    pub fn settle(
        &self,
        published: Published,
        reads: &dyn Reads,
        view: &mut ViewState,
    ) -> Option<UiEvent> {
        let (path, action) = match published {
            Published::Gesture { path, action } => (path, action),
            Published::Host(event) => return Some(event),
            Published::Carry { .. } => return None,
        };
        match self.target(&path, action.gesture())? {
            Target::View(state, write) => {
                view.apply(state, write.into());
                None
            }
            Target::Endpoint(declared) => {
                if let Some(state) = declared.close.as_deref() {
                    view.apply(state, ViewWrite::Flag(ViewSet::Off));
                }
                value(declared, action, reads).map(|value| UiEvent::Write {
                    value,
                    key: declared.key.clone(),
                })
            }
        }
    }
}

fn value(declared: &Declared, action: ControlAction, reads: &dyn Reads) -> Option<WriteValue> {
    Some(match action {
        ControlAction::Activate
        | ControlAction::SecondaryActivate
        | ControlAction::DoubleActivate => WriteValue::Trigger,
        ControlAction::SetScalar(value) => match &declared.edge {
            Some((edge, interval)) => interval_with(*edge, value, reads.get(interval)?)?,
            None => WriteValue::Scalar(value),
        },
        ControlAction::StepScalar(steps) => WriteValue::Step(steps),
        ControlAction::SelectIndex(index) => WriteValue::Index(index),
        ControlAction::Text(text) => WriteValue::Text(text),
        ControlAction::Record(record) => WriteValue::Record(record),
        ControlAction::Place(at) => WriteValue::Point(at),
    })
}

fn interval_with(edge: Edge, value: f64, read: ReadValue<'_>) -> Option<WriteValue> {
    let ReadValue::Range(range) = read else {
        return None;
    };
    let (min, max) = (f64::from(range.min), f64::from(range.max));
    Some(match edge {
        Edge::Min => WriteValue::Range(value, max),
        Edge::Max => WriteValue::Range(min, value),
    })
}
