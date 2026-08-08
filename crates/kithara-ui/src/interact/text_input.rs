use std::ops::Range;

use super::Hit;
use crate::draw::Rect;

#[derive(Clone, Copy, Debug, PartialEq)]
struct CaretStop {
    index: usize,
    x: f32,
}

#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct TextInputLayout {
    carets: Vec<CaretStop>,
    line_height: f32,
    line_y: f32,
    #[field(get, vis = "pub(crate)")]
    text_size: f32,
}

impl Default for TextInputLayout {
    fn default() -> Self {
        Self::new([(0, 0.0)], 0.0, 0.0, 0.0)
    }
}

impl TextInputLayout {
    pub(crate) fn new(
        carets: impl IntoIterator<Item = (usize, f32)>,
        line_y: f32,
        line_height: f32,
        text_size: f32,
    ) -> Self {
        Self {
            carets: carets
                .into_iter()
                .map(|(index, x)| CaretStop { index, x })
                .collect(),
            line_height,
            line_y,
            text_size,
        }
    }

    pub(crate) fn clamp(&self, index: usize) -> usize {
        self.carets
            .iter()
            .min_by_key(|caret| caret.index.abs_diff(index))
            .map_or(0, |caret| caret.index)
    }

    pub(crate) fn index_at(&self, hit: Hit) -> usize {
        let Some(point) = hit.at() else {
            return 0;
        };
        let x = point.x - hit.area().x;
        self.carets
            .iter()
            .min_by(|left, right| (left.x - x).abs().total_cmp(&(right.x - x).abs()))
            .map_or(0, |caret| caret.index)
    }

    pub(crate) fn x(&self, index: usize) -> f32 {
        let index = self.clamp(index);
        self.carets
            .iter()
            .find(|caret| caret.index == index)
            .map_or(0.0, |caret| caret.x)
    }

    pub(crate) fn caret(&self, index: usize, area: Rect) -> Rect {
        Rect {
            h: self.line_height,
            w: 1.0,
            x: area.x + self.x(index).floor(),
            y: area.y + self.line_y,
        }
    }
}

pub(crate) struct PreeditRef<'a> {
    pub(crate) content: &'a str,
    pub(crate) selection: Option<Range<usize>>,
}

pub(crate) struct InputMethodRequest<'a> {
    pub(crate) caret: Rect,
    pub(crate) preedit: Option<PreeditRef<'a>>,
    pub(crate) text_size: f32,
}
