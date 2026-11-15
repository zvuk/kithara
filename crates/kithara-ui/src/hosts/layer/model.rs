use crate::{
    draw::{DrawList, Pt, Rect},
    interact::{CursorShape, Hit, Input, Outcome},
};

#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get, vis = "pub(crate)")]
pub(crate) struct HostLayer<A> {
    draw: DrawList,
    #[field(get(copy))]
    bounds: Rect,
    hits: Vec<LayerHit<A>>,
}

impl<A> HostLayer<A> {
    pub(crate) const fn new(bounds: Rect, draw: DrawList, hits: Vec<LayerHit<A>>) -> Self {
        Self { draw, bounds, hits }
    }

    pub(crate) fn handle(&self, input: Input<'_>, pointer: Option<Pt>) -> Outcome<A>
    where
        A: Copy,
    {
        if !matches!(
            input,
            Input::Pointer(pointer) if pointer.presses_primary()
        ) {
            return Outcome::IGNORED;
        }
        self.hit(pointer)
            .map_or(Outcome::IGNORED, |hit| Outcome::set(*hit.action()))
    }

    fn hit(&self, pointer: Option<Pt>) -> Option<&LayerHit<A>> {
        self.hits()
            .iter()
            .rev()
            .find(|region| Hit::new(pointer, region.area).over())
    }

    delegate::delegate! {
        to self {
            #[expr($.map_or(CursorShape::None, LayerHit::cursor))]
            #[call(hit)]
            pub(crate) fn cursor_at(&self, pointer: Option<Pt>) -> CursorShape;
            #[expr($.map(LayerHit::action))]
            #[call(hit)]
            pub(crate) fn action_at(&self, pointer: Option<Pt>) -> Option<&A>;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get, vis = "pub(crate)")]
pub(crate) struct LayerHit<A> {
    action: A,
    #[field(get(copy))]
    cursor: CursorShape,
    #[field(get(copy))]
    area: Rect,
}

impl<A> LayerHit<A> {
    pub(crate) const fn new(area: Rect, cursor: CursorShape, action: A) -> Self {
        Self {
            action,
            cursor,
            area,
        }
    }
}
