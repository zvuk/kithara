use masonry::vello::{
    util::RenderSurface,
    wgpu::{
        Device, Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
        TextureViewDescriptor,
    },
};

pub(super) const FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;

pub(super) fn replace(surface: &mut RenderSurface<'_>, device: &Device, width: u32, height: u32) {
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("kithara-ui-vello-target"),
        size: Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: FORMAT,
        usage: TextureUsages::STORAGE_BINDING
            | TextureUsages::RENDER_ATTACHMENT
            | TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    surface.target_view = texture.create_view(&TextureViewDescriptor::default());
    surface.target_texture = texture;
}
