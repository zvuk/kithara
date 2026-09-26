use kithara_ui_shaping::GlyphRun;

use super::{Caps, DrawCmd, DrawList, Geom, Image, Needs, Paint, Pen, Rect, Rgba, Transform};

/// Consumes toolkit-neutral retained drawing commands.
pub trait Backend {
    /// What this backend is able to draw. A list that asks for more is refused
    /// whole — see [`Caps::accepts`] — rather than drawn
    /// with the parts it understands.
    const CAPS: Caps;

    /// Encodes a nested list inside a rectangular clip region.
    fn clip(&mut self, region: Rect, list: &DrawList);

    fn fill(&mut self, geom: &Geom, paint: Paint);

    /// Draws one picture into `rect`, turned by `turn` radians about that
    /// rectangle's centre.
    fn image(&mut self, image: &Image, rect: Rect, turn: f32);

    fn stroke(&mut self, geom: &Geom, color: Rgba, pen: Pen);

    fn text(&mut self, run: &GlyphRun, content: &str, transform: Transform, color: Rgba);
}

/// Replays a retained list into a rendering backend.
///
/// A backend that cannot draw part of the list is given none of it. Painting
/// the rest would put a picture on the screen that nobody described: a clip-less
/// backend handed a clipped document would spill its contents, and one without
/// gradients would flatten a ramp to a colour that appears nowhere in the
/// document. Refusing whole is the only answer that stays true to the list.
pub fn replay<B: Backend>(list: &DrawList, backend: &mut B) {
    if let Err(error) = B::CAPS.accepts(Needs::from(list)) {
        tracing::error!(%error, "a backend refused a draw list it cannot draw");
        return;
    }
    draw(list, backend);
}

fn draw<B: Backend>(list: &DrawList, backend: &mut B) {
    for command in list.commands() {
        match command {
            DrawCmd::Clip { region, list } => backend.clip(*region, list),
            DrawCmd::Fill { geom, paint } => backend.fill(geom, *paint),
            DrawCmd::Image { image, rect, turn } => backend.image(image, *rect, *turn),
            DrawCmd::Stroke { geom, color, pen } => backend.stroke(geom, *color, *pen),
            DrawCmd::Text {
                run,
                content,
                transform,
                color,
            } => backend.text(run, content.as_str(), *transform, *color),
        }
    }
}
