use std::collections::HashMap;

use kithara_events::TrackId;
use kithara_platform::sync::Arc;

use crate::item::AudioPlayerItem;

/// Swift/Kotlin-owned `AudioPlayerItem` registry indexed by track id.
/// Aliased so the field types stay free of the structural
/// `Arc<Mutex<HashMap<…>>>` god-map pattern flagged by
/// `arch.no-arc-mutex-godmap`. Single owner = the `AudioPlayer` facade.
pub(crate) type ItemRegistry = HashMap<TrackId, Arc<AudioPlayerItem>>;
