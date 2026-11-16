//! Finalize native geometry before it enters Iced's cache.

use iced::{
    Renderer, advanced::graphics::geometry::Renderer as GeometryRenderer, widget::canvas::Geometry,
};
use iced_renderer::fallback::Renderer as EitherRenderer;
use iced_wgpu::{Renderer as GpuRenderer, geometry::Geometry as GpuGeometry};

use super::linear::linearize;

pub(crate) fn finish(geometry: Geometry) -> Geometry {
    Renderer::linearize(geometry)
}

trait LinearGeometry: GeometryRenderer {
    fn linearize(geometry: Self::Geometry) -> Self::Geometry;
}

impl LinearGeometry for GpuRenderer {
    fn linearize(mut geometry: GpuGeometry) -> GpuGeometry {
        if let GpuGeometry::Live { meshes, .. } = &mut geometry {
            for mesh in meshes {
                linearize(mesh);
            }
        }
        geometry
    }
}

impl<Other: GeometryRenderer> LinearGeometry for EitherRenderer<GpuRenderer, Other> {
    fn linearize(geometry: Self::Geometry) -> Self::Geometry {
        type Either<Other> = <EitherRenderer<GpuRenderer, Other> as GeometryRenderer>::Geometry;
        match geometry {
            Either::<Other>::Primary(geometry) => {
                Either::<Other>::Primary(GpuRenderer::linearize(geometry))
            }
            Either::<Other>::Secondary(geometry) => Either::<Other>::Secondary(geometry),
        }
    }
}
