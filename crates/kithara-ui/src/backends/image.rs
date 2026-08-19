use vello::{
    Scene,
    kurbo::{Affine, Rect as KurboRect},
    peniko::{ImageBrush, ImageData},
};

use super::vello::has_system_text;
use crate::{
    backends::VelloBackend,
    draw::{
        Backend, Caps, DrawList, Geom, Image, ImageId, Paint, Pen, Rect, Rgba, Transform, replay,
    },
    shaping::GlyphRun,
};

/// A Vello encoder with one externally owned image available to the list.
pub(crate) struct VelloImageBackend<'scene, 'image> {
    image: &'image ImageData,
    image_id: &'image ImageId,
    scene: &'scene mut Scene,
}

impl<'scene, 'image> VelloImageBackend<'scene, 'image> {
    pub(crate) const fn new(
        scene: &'scene mut Scene,
        image_id: &'image ImageId,
        image: &'image ImageData,
    ) -> Self {
        Self {
            image,
            image_id,
            scene,
        }
    }
}

impl Backend for VelloImageBackend<'_, '_> {
    const CAPS: Caps = Caps::EVERYTHING;

    fn clip(&mut self, region: Rect, list: &DrawList) {
        if has_system_text(list) {
            tracing::warn!(
                issue = "vello#1198",
                ?region,
                "system-backed text inside a Vello clip may paint incorrectly"
            );
        }
        self.scene.push_clip_layer(
            Affine::IDENTITY,
            &KurboRect::new(
                f64::from(region.x),
                f64::from(region.y),
                f64::from(region.x + region.w),
                f64::from(region.y + region.h),
            ),
        );
        replay(list, self);
        self.scene.pop_layer();
    }

    // Everything but the image is the ordinary Vello encoding, so the plain
    // backend does it against the same scene.
    delegate::delegate! {
        to VelloBackend::new(self.scene) {
            fn fill(&mut self, geom: &Geom, paint: Paint);
            fn stroke(&mut self, geom: &Geom, color: Rgba, pen: Pen);
            fn text(&mut self, run: &GlyphRun, content: &str, transform: Transform, color: Rgba);
        }
    }

    fn image(&mut self, image: &Image, rect: Rect, _turn: f32) {
        if image.id() != self.image_id {
            tracing::error!(?image, "Vello image registry does not own this image");
            return;
        }
        let natural = [f64::from(self.image.width), f64::from(self.image.height)];
        if natural[0] == 0.0 || natural[1] == 0.0 {
            tracing::error!(?image, "Vello image registry contains an empty image");
            return;
        }
        let transform = Affine::translate((f64::from(rect.x), f64::from(rect.y)))
            * Affine::scale_non_uniform(
                f64::from(rect.w) / natural[0],
                f64::from(rect.h) / natural[1],
            );
        let brush = ImageBrush::new(self.image.clone());
        self.scene.draw_image(&brush, transform);
    }
}
