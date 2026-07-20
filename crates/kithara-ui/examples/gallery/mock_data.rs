use std::sync::LazyLock;

use kithara_ui::render::{TrackRow, TreeIcon, TreeRow};
use serde::Deserialize;

#[derive(Deserialize)]
struct MockData {
    title: String,
    artist: String,
    breadcrumb: String,
    tree: Vec<MockTreeRow>,
    tracks: Vec<MockTrack>,
}

#[derive(Deserialize)]
struct MockTreeRow {
    #[serde(default)]
    depth: u8,
    label: String,
    icon: MockTreeIcon,
    #[serde(default)]
    count: Option<u32>,
    #[serde(default)]
    expanded: Option<bool>,
    #[serde(default)]
    selected: bool,
    #[serde(default)]
    muted: bool,
}

#[derive(Clone, Copy, Deserialize)]
enum MockTreeIcon {
    Collection,
    Playlist,
    Folder,
    Plus,
    Zvuk,
    Search,
    Charts,
    Monitor,
    Home,
    Usb,
    Instrument,
    Waveform,
    Clock,
}

impl From<MockTreeIcon> for TreeIcon {
    fn from(value: MockTreeIcon) -> Self {
        match value {
            MockTreeIcon::Collection => Self::Collection,
            MockTreeIcon::Playlist => Self::Playlist,
            MockTreeIcon::Folder => Self::Folder,
            MockTreeIcon::Plus => Self::Plus,
            MockTreeIcon::Zvuk => Self::Zvuk,
            MockTreeIcon::Search => Self::Search,
            MockTreeIcon::Charts => Self::Charts,
            MockTreeIcon::Monitor => Self::Monitor,
            MockTreeIcon::Home => Self::Home,
            MockTreeIcon::Usb => Self::Usb,
            MockTreeIcon::Instrument => Self::Instrument,
            MockTreeIcon::Waveform => Self::Waveform,
            MockTreeIcon::Clock => Self::Clock,
        }
    }
}

#[derive(Deserialize)]
struct MockTrack {
    title: String,
    artist: String,
    time: String,
    search: String,
    deck: String,
    bpm: String,
    key: String,
    energy: u8,
    transition: String,
}

pub(crate) struct Catalog {
    pub(crate) title: &'static str,
    pub(crate) artist: &'static str,
    pub(crate) breadcrumb: &'static str,
    pub(crate) rows: &'static [TrackRow<'static>],
    pub(crate) tree: &'static [TreeRow<'static>],
}

pub(crate) static CATALOG: LazyLock<Catalog> = LazyLock::new(load_catalog);

fn load_catalog() -> Catalog {
    let data: MockData = ron::from_str(include_str!("assets/mock-data.ron"))
        .expect("embedded gallery mock data must parse");
    let data: &'static MockData = Box::leak(Box::new(data));
    let rows: Vec<TrackRow<'static>> = data
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| TrackRow {
            title: &track.title,
            artist: Some(&track.artist),
            time: Some(&track.time),
            search: Some(&track.search),
            deck: Some(&track.deck),
            bpm: Some(&track.bpm),
            key: Some(&track.key),
            energy: Some(track.energy),
            transition: Some(&track.transition),
            current: index == 0,
            selected: index == 0,
        })
        .collect();
    let tree: Vec<TreeRow<'static>> = data
        .tree
        .iter()
        .map(|row| TreeRow {
            depth: row.depth,
            label: &row.label,
            icon: row.icon.into(),
            count: row.count,
            expanded: row.expanded,
            selected: row.selected,
            muted: row.muted,
        })
        .collect();
    Catalog {
        title: &data.title,
        artist: &data.artist,
        breadcrumb: &data.breadcrumb,
        rows: Box::leak(rows.into_boxed_slice()),
        tree: Box::leak(tree.into_boxed_slice()),
    }
}
