use std::ops::Range;

use kithara_ui_draw::Rect;

use super::Hit;

#[derive(Clone, Copy, Debug, PartialEq)]
struct CaretStop {
    x: f32,
    index: usize,
}

#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct TextInputLayout {
    carets: Vec<CaretStop>,
    line_height: f32,
    line_y: f32,
    #[field(get)]
    text_size: f32,
}

impl Default for TextInputLayout {
    fn default() -> Self {
        Self::new([(0, 0.0)], 0.0, 0.0, 0.0)
    }
}

impl TextInputLayout {
    pub fn new<C>(carets: C, line_y: f32, line_height: f32, text_size: f32) -> Self
    where
        C: IntoIterator<Item = (usize, f32)>,
    {
        Self {
            line_height,
            line_y,
            text_size,
            carets: carets
                .into_iter()
                .map(|(index, x)| CaretStop { x, index })
                .collect(),
        }
    }

    #[must_use]
    pub fn caret(&self, index: usize, area: Rect) -> Rect {
        Rect {
            h: self.line_height,
            w: 1.0,
            x: area.x + self.x(index).floor(),
            y: area.y + self.line_y,
        }
    }

    #[must_use]
    pub fn clamp(&self, index: usize) -> usize {
        self.carets
            .iter()
            .min_by_key(|caret| caret.index.abs_diff(index))
            .map_or(0, |caret| caret.index)
    }

    #[must_use]
    pub fn index_at(&self, hit: Hit) -> usize {
        let Some(point) = hit.at() else {
            return 0;
        };
        let x = point.x - hit.area().x;
        self.carets
            .iter()
            .min_by(|left, right| (left.x - x).abs().total_cmp(&(right.x - x).abs()))
            .map_or(0, |caret| caret.index)
    }

    #[must_use]
    pub fn x(&self, index: usize) -> f32 {
        let index = self.clamp(index);
        self.carets
            .iter()
            .find(|caret| caret.index == index)
            .map_or(0.0, |caret| caret.x)
    }
}

pub struct PreeditRef<'a> {
    pub content: &'a str,
    pub selection: Option<Range<usize>>,
}

pub struct InputMethodRequest<'a> {
    pub preedit: Option<PreeditRef<'a>>,
    pub caret: Rect,
    pub text_size: f32,
}
