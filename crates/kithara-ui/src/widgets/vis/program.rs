use iced::{
    Element, Length, Rectangle,
    widget::{Space, shader},
};
use num_traits::ToPrimitive;

use super::pipeline::VisPrimitive;
use crate::{
    render::{ReadValue, Reads, UiEvent},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Vis<'value, 'data, 'reads> {
    preset: Option<&'value ReadValue<'data>>,
    reads: &'reads dyn Reads,
}

struct Consts;

impl Consts {
    const CLOCK: &'static str = "vis.time";
    const MASTER_LEVEL: &'static str = "player.output.levels";
    const PRESET_COUNT: u32 = 3;
}

impl<'a> Widget<'a> for Vis<'_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Scalar(preset)) = self.preset else {
            return Space::new().into();
        };
        let Some(preset) = preset
            .round()
            .to_u32()
            .filter(|preset| *preset < Consts::PRESET_COUNT)
        else {
            return Space::new().into();
        };
        let Some(ReadValue::Stereo(levels)) = self.reads.get(Consts::MASTER_LEVEL) else {
            return Space::new().into();
        };
        let level = levels.l.max(levels.r) * levels.volume;
        if !level.is_finite() {
            return Space::new().into();
        }
        let time = match self.reads.get(Consts::CLOCK) {
            Some(ReadValue::Scalar(seconds)) if seconds.is_finite() => seconds.to_f32(),
            _ => None,
        }
        .unwrap_or_default();

        shader::Shader::new(VisProgram {
            preset,
            level: level.clamp(0.0, 1.0),
            time,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

#[derive(Clone, Copy, Debug)]
struct VisProgram {
    preset: u32,
    level: f32,
    time: f32,
}

impl shader::Program<UiEvent> for VisProgram {
    type State = ();
    type Primitive = VisPrimitive;

    fn draw(
        &self,
        _state: &Self::State,
        _cursor: iced::mouse::Cursor,
        _bounds: Rectangle,
    ) -> Self::Primitive {
        VisPrimitive::new(self.level, self.preset, self.time)
    }
}
