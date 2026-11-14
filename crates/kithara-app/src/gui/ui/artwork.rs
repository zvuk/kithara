use std::{collections::hash_map::DefaultHasher, hash::Hasher, io::Cursor};

use kithara::{
    platform::{
        sync::Arc,
        tokio::{
            runtime::Handle,
            sync::mpsc::{self, UnboundedReceiver, error::TryRecvError},
            task,
        },
    },
    ui::draw::{Image, ImageId},
};

mod consts {
    /// Most memory one cover decode may allocate.
    pub(super) const DECODE_ALLOC_LIMIT: u64 = 32 * 1024 * 1024;
    /// Longest side a decoded cover keeps; a larger cover is thumbnailed.
    pub(super) const COVER_SIDE: u32 = 300;
}

/// The current encoded cover and its asynchronously decoded picture.
#[derive(Default)]
pub(in crate::gui) struct Artwork {
    source: Option<Arc<Vec<u8>>>,
    pending: Option<UnboundedReceiver<Option<Image>>>,
    image: Option<Image>,
}

impl Artwork {
    pub(in crate::gui) fn image(&self) -> Option<&Image> {
        self.image.as_ref()
    }

    pub(super) fn refresh(&mut self, source: Option<&Arc<Vec<u8>>>, runtime: &Handle) {
        if self.source.as_ref().map(Arc::as_ptr) != source.map(Arc::as_ptr) {
            self.source = source.cloned();
            self.image = None;
            self.pending = None;
            if let Some(bytes) = source {
                let bytes = Arc::clone(bytes);
                let (ready, pending) = mpsc::unbounded_channel();
                self.pending = Some(pending);
                drop(task::spawn_blocking_on(runtime, move || {
                    let image = decode(&bytes).unwrap_or_else(|error| {
                        tracing::warn!(%error, "track artwork could not be decoded");
                        None
                    });
                    let _ = ready.send(image);
                }));
            }
        }
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        match pending.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(image) => self.image = image,
            Err(TryRecvError::Disconnected) => {}
        }
        self.pending = None;
    }
}

fn decode(bytes: &[u8]) -> image::ImageResult<Option<Image>> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(consts::DECODE_ALLOC_LIMIT);
    reader.limits(limits);
    let decoded = reader.decode()?;
    let rgba = if decoded.width() > consts::COVER_SIDE || decoded.height() > consts::COVER_SIDE {
        decoded
            .thumbnail(consts::COVER_SIDE, consts::COVER_SIDE)
            .into_rgba8()
    } else {
        decoded.into_rgba8()
    };
    let (width, height) = rgba.dimensions();
    let mut identity = DefaultHasher::new();
    identity.write(bytes);
    Ok(Image::pixels(
        ImageId::new(&format!("track-artwork/{:016x}", identity.finish())),
        width,
        height,
        rgba.into_raw().into(),
    ))
}

#[cfg(test)]
mod tests {
    use image::ImageFormat;
    use kithara_test_utils::{kithara, wait_until};

    use super::*;
    use crate::gui::test_fixture::cover;

    #[kithara::test(native)]
    #[case(ImageFormat::Jpeg)]
    #[case(ImageFormat::Png)]
    fn supported_covers_decode_to_shared_rgba_with_content_identity(#[case] format: ImageFormat) {
        let first = cover([255, 0, 0], format);
        let second = cover([0, 0, 255], format);
        let image = decode(&first)
            .expect("valid image")
            .expect("nonempty pixels");

        assert_eq!((image.width(), image.height()), (4, 2));
        assert_eq!(image.rgba().expect("RGBA pixels").len(), 4 * 2 * 4);
        assert_eq!(decode(&first).expect("same bytes"), Some(image.clone()));
        assert_ne!(
            decode(&second)
                .expect("new bytes")
                .expect("new pixels")
                .id(),
            image.id(),
        );
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn clearing_or_replacing_a_cover_rejects_a_late_previous_completion() {
        let runtime = Handle::current();
        let first = cover([255, 0, 0], ImageFormat::Png);
        let second = cover([0, 0, 255], ImageFormat::Png);
        let old = decode(&first).expect("valid cover");
        for next in [None, Some(&second)] {
            let (finished, pending) = mpsc::unbounded_channel();
            let mut cache = Artwork {
                source: Some(Arc::clone(&first)),
                pending: Some(pending),
                image: old.clone(),
            };
            cache.refresh(next, &runtime);
            assert!(
                finished.send(old.clone()).is_err(),
                "the old result has no receiver"
            );
            if let Some(next) = next {
                wait_until(
                    kithara_test_utils::TestBudget::Quick.timeout(false),
                    "new cover finishes",
                    || {
                        cache.refresh(Some(next), &runtime);
                        cache.pending.is_none()
                    },
                )
                .await
                .expect("cover decodes");
                assert_eq!(cache.image, decode(next).expect("new cover"));
            } else {
                assert!(cache.image().is_none());
            }
        }
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn failed_bytes_are_not_resubmitted_on_every_tick() {
        let runtime = Handle::current();
        let bytes = Arc::new(vec![1, 2, 3]);
        let mut cache = Artwork::default();
        cache.refresh(Some(&bytes), &runtime);
        wait_until(
            kithara_test_utils::TestBudget::Quick.timeout(false),
            "invalid cover finishes",
            || {
                cache.refresh(Some(&bytes), &runtime);
                cache.pending.is_none()
            },
        )
        .await
        .expect("failure finishes");

        assert!(cache.image().is_none());
        for _ in 0..3 {
            cache.refresh(Some(&bytes), &runtime);
            assert!(cache.pending.is_none());
        }
    }
}
