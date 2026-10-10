use kithara_bufpool::HasPool;
use kithara_events::TrackId;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_play::ResourceConfig;

use crate::error::QueueError;

/// What a task beside a track's load reports to the queue that owns the
/// track.
#[doc(hidden)]
pub enum LoadReport {
    /// The downloader found the load's transfer slow. `watch` ends with the
    /// load, so a report from a load that ended since is past news.
    Slow { id: TrackId, watch: CancelToken },
    /// The track's cover, read beside its audio. `load` is the track's token,
    /// which outlives the load in the resource it built: the audio never
    /// waits for the cover.
    Cover {
        id: TrackId,
        load: CancelToken,
        cover: Arc<Vec<u8>>,
    },
}

/// A track's live load: the token its open and its resource are cancelled
/// with, and the watch that ends the tasks beside it. Dropping it armed
/// cancels the track's token, so removing a track aborts its load; dropping
/// it always ends the tasks that watch the load.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct TrackLoad {
    /// The track's token; once the load opens it belongs to the resource.
    #[field(get, vis = "pub(crate)")]
    token: CancelToken,
    /// Ends the tasks watching the load, however the load ends.
    #[field(get, vis = "pub(crate)")]
    watch: CancelToken,
    /// Whether dropping the load cancels `token`.
    armed: bool,
}

impl TrackLoad {
    /// The load of `config`, owning its per-track token.
    ///
    /// # Errors
    /// [`QueueError::Resource`] when `config` carries no per-track token.
    pub(crate) fn new<S>(config: &ResourceConfig<S>) -> Result<Self, QueueError>
    where
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        let Some(token) = config.cancel().cloned() else {
            return Err(QueueError::Resource(
                "resource config missing per-track cancel".to_owned(),
            ));
        };
        Ok(Self {
            watch: token.child(),
            token,
            armed: true,
        })
    }

    /// Give the track's token up to its next owner; dropping then cancels
    /// nothing but the load's watches.
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TrackLoad {
    fn drop(&mut self) {
        if self.armed {
            self.token.cancel();
        }
        self.watch.cancel();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use kithara_assets::AssetStore;
    use kithara_play::ResourceSrc;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::{TestPools, pools};

    /// A config for `url` carrying `token` as its per-track token.
    pub(crate) fn config(url: &str, token: CancelToken) -> ResourceConfig<TestPools> {
        let mut config =
            ResourceConfig::for_src(ResourceSrc::parse(url).expect("BUG: test URL is valid"))
                .store(AssetStore::builder(pools()).build())
                .build();
        config.set_cancel(token);
        config
    }

    fn load(token: &CancelToken) -> TrackLoad {
        TrackLoad::new(&config("https://x/a.mp3", token.clone()))
            .expect("the config carries its token")
    }

    #[kithara::test]
    fn dropping_an_armed_load_cancels_its_track() {
        let token = CancelToken::never().child();
        drop(load(&token));
        assert!(token.is_cancelled());
    }

    #[kithara::test]
    fn dropping_a_disarmed_load_leaves_its_track_to_the_resource() {
        let token = CancelToken::never().child();
        let mut load = load(&token);
        let watch = load.watch().clone();
        load.disarm();
        drop(load);
        assert!(!token.is_cancelled());
        assert!(watch.is_cancelled(), "a load's watches end with it");
    }

    #[kithara::test]
    fn a_config_without_a_track_token_is_refused() {
        let config = ResourceConfig::<TestPools>::for_src(
            ResourceSrc::parse("https://x/a.mp3").expect("BUG: test URL is valid"),
        )
        .store(AssetStore::builder(pools()).build())
        .build();
        assert!(matches!(
            TrackLoad::new(&config),
            Err(QueueError::Resource(_))
        ));
    }
}
