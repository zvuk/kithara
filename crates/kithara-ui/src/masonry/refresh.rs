use crate::{
    atoms::{
        bar::preset::PresetData, deck::summary::Loaded, icon::glyph::GlyphData,
        painter::ControlPainter, wave::face::Drawn,
    },
    hosts::controls::{Draws, Reading},
    masonry::retained::{
        MasonryHost, MasonryNode,
        mount::{Cx, NodeControl, painted, refreshing},
    },
    mount::{
        Preset,
        deck::{
            summary::host::{Summary, snapshot},
            wave::host::Wave,
        },
        label::glyph::host::Glyph,
        panel::{lottie::host::Lottie, sprite::host::Sprite},
    },
    render::document::Ctx,
};

pub(crate) type DataRefresh<Data> = Box<dyn Fn(&mut Data, Ctx<'_, '_>) -> bool>;

/// The half of a drawn control only a host that keeps its widgets needs.
///
/// An immediate host asks the document again every frame and never holds a
/// leaf across two. A retained one mounts the leaf once, so a control whose
/// picture follows more than its own endpoint says here how that leaf steps
/// itself afterwards, given the endpoint its reading came from.
pub(crate) trait Refresh: Draws {
    /// How a leaf that is mounted once steps itself afterwards, given the
    /// endpoint its reading came from.
    ///
    /// The name is passed rather than kept on the reading because only a
    /// retained host has anything to do with it: an immediate one asks the
    /// document again every frame and never holds a leaf across two.
    fn refresh(
        &self,
        read: Reading<'_>,
        endpoint: Option<&str>,
    ) -> Option<DataRefresh<<Self::Painter as ControlPainter>::Data>>;
}

impl Refresh for Wave<'_> {
    fn refresh(&self, read: Reading<'_>, _endpoint: Option<&str>) -> Option<DataRefresh<Drawn>> {
        let scope = read.scope.to_owned();
        let zoom = self
            .zoom
            .map(|binding| read.ctx.ui.resolve(binding.key).to_owned());
        Some(Box::new(move |data, ctx| {
            data.refresh(&ctx, &scope, zoom.as_deref())
        }))
    }
}

impl NodeControl for Wave<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        refreshing(self, host, cx)
    }
}

impl Refresh for Preset {
    fn refresh(
        &self,
        _read: Reading<'_>,
        _endpoint: Option<&str>,
    ) -> Option<DataRefresh<PresetData>> {
        Some(Box::new(refresh))
    }
}

impl NodeControl for Preset {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        refreshing(self, host, cx)
    }
}

fn refresh(data: &mut PresetData, ctx: Ctx<'_, '_>) -> bool {
    let active = Preset::active(data.items, ctx);
    std::mem::replace(&mut data.active, active) != active
}

impl Refresh for Summary {
    fn refresh(&self, read: Reading<'_>, _endpoint: Option<&str>) -> Option<DataRefresh<Loaded>> {
        let scope = read.scope.to_owned();
        Some(Box::new(move |data, ctx| {
            let next = snapshot(None, &ctx, &scope);
            std::mem::replace(data, next) != *data
        }))
    }
}

impl NodeControl for Summary {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        refreshing(self, host, cx)
    }
}

mod lottie {
    use super::{DataRefresh, Reading, Refresh};
    use crate::{
        atoms::picture::lottie::Standing,
        mount::panel::lottie::host::{Lottie, flagged, seconds, standing},
    };

    impl Refresh for Lottie<'_> {
        /// A retained host mounts a leaf once and then only hears about it
        /// again if something says the leaf changed, so the artwork is stepped
        /// by asking its endpoint afresh rather than by the mount that built it.
        fn refresh(
            &self,
            read: Reading<'_>,
            endpoint: Option<&str>,
        ) -> Option<DataRefresh<Standing>> {
            let artwork = read
                .skin
                .artwork(read.ctx.ui.resolve(self.artwork))
                .cloned();
            let active_artwork = self
                .active_artwork
                .and_then(|name| read.skin.artwork(read.ctx.ui.resolve(name)).cloned());
            let flag = read.ctx.endpoint(self.active).map(ToOwned::to_owned);
            let endpoint = endpoint?.to_owned();
            let pass = self.seconds;
            Some(Box::new(move |data, ctx| {
                let showing = active_artwork
                    .as_ref()
                    .filter(|_| flag.as_deref().is_some_and(|flag| flagged(ctx.get(flag))))
                    .cloned()
                    .or_else(|| artwork.clone());
                let next = standing(showing, pass, seconds(ctx.get(&endpoint).as_ref()));
                if next == *data {
                    return false;
                }
                *data = next;
                true
            }))
        }
    }
}

mod sprite {
    use super::{DataRefresh, Reading, Refresh};
    use crate::{
        draw::Image,
        mount::panel::sprite::host::{Sprite, frame, seconds},
    };

    impl Refresh for Sprite {
        /// A retained host mounts a leaf once and then only hears about it
        /// again if something says the leaf changed, so the sheet is stepped by
        /// asking its endpoint afresh rather than by the mount that built it.
        fn refresh(
            &self,
            read: Reading<'_>,
            endpoint: Option<&str>,
        ) -> Option<DataRefresh<Option<Image>>> {
            let sheet = read.skin.sheet(read.ctx.ui.resolve(self.sheet)).cloned();
            let endpoint = endpoint?.to_owned();
            let pass = self.seconds;
            Some(Box::new(move |data, ctx| {
                let next = frame(sheet.as_ref(), pass, seconds(ctx.get(&endpoint).as_ref()));
                if next.as_ref().map(Image::id) == data.as_ref().map(Image::id) {
                    return false;
                }
                *data = next;
                true
            }))
        }
    }
}

impl NodeControl for Lottie<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        refreshing(self, host, cx)
    }
}

impl NodeControl for Sprite {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        refreshing(self, host, cx)
    }
}

impl Refresh for Glyph<'_> {
    fn refresh(
        &self,
        read: Reading<'_>,
        _endpoint: Option<&str>,
    ) -> Option<DataRefresh<GlyphData>> {
        let appearance = self.appearance(read.skin)?;
        let palette = kithara_platform::sync::Arc::clone(&read.skin.palette);
        let color = self.color.cloned();
        let active = self.active.cloned();
        Some(Box::new(move |data, ctx| {
            let next = appearance.data(
                ctx.flag(active.as_ref()),
                ctx.color_role(color.as_ref()),
                &palette,
            );
            std::mem::replace(data, next) != *data
        }))
    }
}

impl NodeControl for Glyph<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        if self.active.is_some() || self.color.and_then(|color| color.binding()).is_some() {
            refreshing(self, host, cx)
        } else {
            painted(self, host, cx)
        }
    }
}
