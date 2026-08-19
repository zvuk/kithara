use crate::{
    draw::{DrawList, Pt, Rect},
    interact::{CursorShape, Hit, Input, Outcome, PointerPhase},
};

#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get, vis = "pub(crate)")]
pub(crate) struct HostLayer<A> {
    #[field(get(copy))]
    bounds: Rect,
    draw: DrawList,
    hits: Vec<LayerHit<A>>,
}

impl<A> HostLayer<A> {
    pub(crate) const fn new(bounds: Rect, draw: DrawList, hits: Vec<LayerHit<A>>) -> Self {
        Self { bounds, draw, hits }
    }

    pub(crate) fn handle(&self, input: Input<'_>, pointer: Option<Pt>) -> Outcome<A>
    where
        A: Copy,
    {
        if !matches!(
            input,
            Input::Pointer(pointer) if pointer.phase == PointerPhase::Down
        ) {
            return Outcome::IGNORED;
        }
        self.hit(pointer)
            .map_or(Outcome::IGNORED, |hit| Outcome::set(*hit.action()))
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

    fn hit(&self, pointer: Option<Pt>) -> Option<&LayerHit<A>> {
        self.hits()
            .iter()
            .rev()
            .find(|region| Hit::new(pointer, region.area).over())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get, vis = "pub(crate)")]
pub(crate) struct LayerHit<A> {
    #[field(get(copy))]
    area: Rect,
    #[field(get(copy))]
    cursor: CursorShape,
    action: A,
}

impl<A> LayerHit<A> {
    pub(crate) const fn new(area: Rect, cursor: CursorShape, action: A) -> Self {
        Self {
            area,
            cursor,
            action,
        }
    }
}

pub(crate) fn handle<A: Copy>(
    layers: &[HostLayer<A>],
    input: Input<'_>,
    pointer: Option<Pt>,
) -> Outcome<A> {
    layers
        .iter()
        .rev()
        .map(|layer| layer.handle(input, pointer))
        .find(Outcome::is_captured)
        .unwrap_or(Outcome::IGNORED)
}

pub(crate) fn cursor<A>(layers: &[HostLayer<A>], pointer: Option<Pt>) -> CursorShape {
    layers
        .iter()
        .rev()
        .map(|layer| layer.cursor_at(pointer))
        .find(|shape| *shape != CursorShape::None)
        .unwrap_or(CursorShape::None)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        draw::{DrawList, Pt, Rect},
        interact::{CursorShape, Input, Outcome, mouse as mouse_input},
    };

    fn pointer_input(phase: PointerPhase, at: Option<Pt>) -> Input<'static> {
        Input::Pointer(mouse_input(phase, at))
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { h, w, x, y }
    }

    #[kithara::test]
    fn the_last_layer_is_topmost_for_input_and_cursor() {
        let lower = HostLayer::new(
            rect(0.0, 0.0, 100.0, 60.0),
            DrawList::default(),
            vec![LayerHit::new(
                rect(0.0, 0.0, 8.0, 60.0),
                CursorShape::ResizeH,
                1_u8,
            )],
        );
        let upper = HostLayer::new(
            rect(0.0, 0.0, 100.0, 60.0),
            DrawList::default(),
            vec![LayerHit::new(
                rect(0.0, 0.0, 8.0, 60.0),
                CursorShape::Pointer,
                2_u8,
            )],
        );
        let layers = [lower, upper];
        let pointer = Some(Pt { x: 3.0, y: 30.0 });

        assert_eq!(
            handle(&layers, pointer_input(PointerPhase::Down, None), pointer),
            Outcome::set(2)
        );
        assert_eq!(cursor(&layers, pointer), CursorShape::Pointer);
    }

    #[kithara::test]
    fn a_hit_is_half_open_and_a_non_press_is_ignored() {
        let layer = HostLayer::new(
            rect(0.0, 0.0, 100.0, 60.0),
            DrawList::default(),
            vec![LayerHit::new(
                rect(0.0, 0.0, 4.0, 60.0),
                CursorShape::ResizeH,
                7_u8,
            )],
        );
        let layers = [layer];

        assert_eq!(
            handle(
                &layers,
                pointer_input(PointerPhase::Down, None),
                Some(Pt { x: 3.0, y: 30.0 })
            ),
            Outcome::set(7)
        );
        assert_eq!(
            handle(
                &layers,
                pointer_input(PointerPhase::Down, None),
                Some(Pt { x: 4.0, y: 30.0 })
            ),
            Outcome::IGNORED
        );
        assert_eq!(
            handle(
                &layers,
                pointer_input(PointerPhase::Move, Some(Pt { x: 3.0, y: 30.0 })),
                Some(Pt { x: 3.0, y: 30.0 })
            ),
            Outcome::IGNORED
        );
    }
}
