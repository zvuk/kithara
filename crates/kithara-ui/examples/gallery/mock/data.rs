use std::sync::LazyLock;

use kithara_ui::render::{TableCell, TableRow, TreeIcon, TreeRow};
use serde::Deserialize;

#[derive(Deserialize)]
struct MockData {
    vis_indices: (String, String, String),
    vis_presets: (String, String, String),
    artist: String,
    breadcrumb: String,
    footer_tokens_anatomy: String,
    title: String,
    pivot: PivotCopy,
    tracks: Vec<MockTrack>,
    tree: Vec<MockTreeRow>,
}

/// Pivot copy lives in the asset so the sources stay ASCII; `{name}` slots are
/// filled by `mock::pivot`.
#[derive(Deserialize)]
pub(crate) struct PivotCopy {
    pub(crate) pulse: String,
    pub(crate) pulse_empty: String,
    pub(crate) duration: String,
    pub(crate) hint: String,
    pub(crate) hint_empty: String,
    pub(crate) tracks: String,
    pub(crate) tracks_empty: String,
}

#[derive(Deserialize)]
struct MockTreeRow {
    icon: MockTreeIcon,
    #[serde(default)]
    count: Option<u32>,
    #[serde(default)]
    expanded: Option<bool>,
    label: String,
    #[serde(default)]
    muted: bool,
    #[serde(default)]
    selected: bool,
    #[serde(default)]
    depth: u8,
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
    artist: String,
    bpm: String,
    deck: String,
    key: String,
    search: String,
    time: String,
    title: String,
    transition: String,
    energy: u8,
}

pub(crate) struct Catalog {
    pub(crate) rows: &'static [TableRow<'static>],
    pub(crate) tree: &'static [TreeRow<'static>],
    pub(crate) artist: &'static str,
    pub(crate) breadcrumb: &'static str,
    pub(crate) footer_tokens_anatomy: &'static str,
    pub(crate) title: &'static str,
    pub(crate) pivot: &'static PivotCopy,
    pub(crate) vis_indices: [&'static str; 3],
    pub(crate) vis_presets: [&'static str; 3],
}

pub(crate) static CATALOG: LazyLock<Catalog> = LazyLock::new(load_catalog);

fn load_catalog() -> Catalog {
    let data: MockData = ron::from_str(include_str!("../assets/mock-data.ron"))
        .expect("embedded gallery mock data must parse");
    let data: &'static MockData = Box::leak(Box::new(data));
    let rows: Vec<TableRow<'static>> = data
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            TableRow::new(
                vec![
                    TableCell::text("title", &track.title),
                    TableCell::text("artist", &track.artist),
                    TableCell::text("time", &track.time),
                    TableCell::text("search", &track.search),
                    TableCell::text("deck", &track.deck),
                    TableCell::text("bpm", &track.bpm),
                    TableCell::text("key", &track.key),
                    TableCell::number("energy", track.energy),
                    TableCell::text("transition", &track.transition),
                ],
                index == 0,
            )
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
        footer_tokens_anatomy: &data.footer_tokens_anatomy,
        pivot: &data.pivot,
        vis_indices: [
            data.vis_indices.0.as_str(),
            data.vis_indices.1.as_str(),
            data.vis_indices.2.as_str(),
        ],
        vis_presets: [
            data.vis_presets.0.as_str(),
            data.vis_presets.1.as_str(),
            data.vis_presets.2.as_str(),
        ],
        rows: Box::leak(rows.into_boxed_slice()),
        tree: Box::leak(tree.into_boxed_slice()),
    }
}
