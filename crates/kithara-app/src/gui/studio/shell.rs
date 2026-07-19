use iced::{
    Element, Length,
    widget::{column, container, row},
};

use super::{
    deck::{DeckProps, playhead_progress, view_deck},
    library::{LibMsg, LibProps, LibRow, view_library},
    mixer::{CrossMsg, CrossProps, StripMsg, StripProps, view_crossfader, view_strip},
    status::{StatusProps, view_status_bar},
    styles::{horizontal_divider, shell_style, vertical_divider},
    timestretch::TsProps,
    tokens::studio_size,
    topbar::view_topbar,
};
use crate::{
    catalog::is_loaded,
    deck::DeckId,
    gui::{
        app::Kithara,
        deck::DeckUi,
        message::Message,
        mix::MixMsg,
        view::{format_time, track_subtitle},
    },
};

/// Channel letters, in deck order. Decks past the alphabet fall back to a dot;
/// the studio layout is built for two.
const CHANNELS: [char; 4] = ['A', 'B', 'C', 'D'];

/// Studio layout. Every block is self-contained: this is the only place that
/// decides what stands where, how wide it is, and which deck it addresses.
pub(crate) fn view_dj_studio(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;

    container(
        column![
            view_topbar(p).map(|_exit| Message::ToggleStudio),
            horizontal_divider(studio_size::DIVIDER, p.line),
            deck_row(state),
            horizontal_divider(studio_size::DIVIDER, p.line),
            library_panel(state),
            horizontal_divider(studio_size::DIVIDER, p.line_soft),
            view_status_bar(status_props(state), p),
        ]
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .height(Length::Fill)
    .style(shell_style(p))
    .into()
}

/// `A | MIXER | B` — decks flank the mixer, as in the reference layout.
fn deck_row(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let mut lane = row![].width(Length::Fill).height(Length::Fill);

    for (index, id) in state.decks.ids().into_iter().enumerate() {
        let Some(deck) = state.decks.get(id) else {
            continue;
        };
        // The mixer sits between the first and second deck.
        if index == 1 {
            lane = lane
                .push(vertical_divider(
                    studio_size::DIVIDER,
                    f32::INFINITY,
                    p.line,
                ))
                .push(mixer_panel(state))
                .push(vertical_divider(
                    studio_size::DIVIDER,
                    f32::INFINITY,
                    p.line,
                ));
        } else if index > 1 {
            lane = lane.push(vertical_divider(
                studio_size::DIVIDER,
                f32::INFINITY,
                p.line,
            ));
        }
        lane = lane.push(view_deck(deck_props(deck), p).map(move |msg| Message::Deck(id, msg)));
    }

    lane.into()
}

/// The mixer column: one strip per deck over the crossfader.
fn mixer_panel(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let mix = state.session.mix();

    let strips = state
        .decks
        .ids()
        .into_iter()
        .fold(row![].height(Length::Fill), |strips, id| {
            let strip = state
                .session
                .position(id)
                .and_then(|at| mix.strips.get(at))
                .copied();
            let props = StripProps {
                label: channel_of(state, id),
                trim: strip.map_or(1.0, |s| s.trim),
                muted: strip.is_some_and(|s| s.muted),
                focused: state.decks.focus() == id,
            };
            strips.push(view_strip(props, p).map(move |msg| strip_msg(id, msg)))
        });

    container(column![
        strips,
        view_crossfader(
            CrossProps {
                position: mix.position,
                master: mix.group_master,
            },
            p,
        )
        .map(cross_msg),
    ])
    .width(Length::Fixed(studio_size::MIXER_WIDTH))
    .height(Length::Fill)
    .into()
}

/// The shared track list: one row per catalog entry, marked with the decks it
/// sits on and carrying a load button per deck.
fn library_panel(state: &Kithara) -> Element<'_, Message> {
    let rows = state
        .catalog
        .entries()
        .iter()
        .enumerate()
        .map(|(index, entry)| LibRow {
            title: &entry.name,
            url: Some(entry.url.as_str()),
            loaded_on: state
                .decks
                .ids()
                .into_iter()
                .filter(|id| {
                    state
                        .decks
                        .get(*id)
                        .is_some_and(|deck| is_loaded(deck.controller.queue(), entry))
                })
                .map(|id| channel_of(state, id))
                .collect(),
            selected: state.selected_track == Some(index),
        })
        .collect();

    let ids = state.decks.ids();
    let props = LibProps {
        rows,
        targets: ids.iter().map(|id| channel_of(state, *id)).collect(),
    };

    container(view_library(&props, state.palette).map(move |msg| lib_msg(msg, &ids)))
        .width(Length::Fill)
        .height(Length::Fixed(studio_size::LIBRARY_HEIGHT))
        .into()
}

/// Channel letter by the deck's position in the session, not by its id — ids
/// are stable across removals, channels are not.
fn channel_of(state: &Kithara, id: DeckId) -> char {
    state
        .session
        .position(id)
        .and_then(|at| CHANNELS.get(at).copied())
        .unwrap_or('\u{00b7}')
}

fn strip_msg(id: DeckId, msg: StripMsg) -> Message {
    match msg {
        StripMsg::Trim(trim) => Message::Mix(MixMsg::Trim(id, trim)),
        StripMsg::Mute(muted) => Message::Mix(MixMsg::Mute(id, muted)),
        StripMsg::Focus => Message::FocusDeck(id),
    }
}

fn cross_msg(msg: CrossMsg) -> Message {
    match msg {
        CrossMsg::Position(position) => Message::Mix(MixMsg::Crossfader(position)),
        CrossMsg::Master(master) => Message::Mix(MixMsg::GroupMaster(master)),
    }
}

/// The library reports load targets by position; the composer resolves them to
/// the decks currently in the session.
fn lib_msg(msg: LibMsg, ids: &[DeckId]) -> Message {
    match msg {
        LibMsg::Select(index) => Message::SelectCatalogTrack(index),
        LibMsg::Load(index, target) => ids
            .get(target)
            .map_or(Message::SelectCatalogTrack(index), |id| {
                Message::LoadOntoDeck(index, *id)
            }),
    }
}

fn status_props(state: &Kithara) -> StatusProps {
    let deck = state.decks.focused();
    StatusProps {
        playing: state.decks.iter().any(|deck| deck.ui.playing),
        track_count: deck.ui.tracks.len(),
        load: deck.ui.engine_load,
    }
}

fn waveform_of(deck: &DeckUi) -> Option<&kithara::audio::Waveform> {
    deck.ui
        .analysis
        .as_ref()
        .and_then(|analysis| analysis.waveform.as_ref())
        .filter(|wave| !wave.is_empty())
}

/// Fold one deck's snapshot and view state into what its blocks draw.
fn deck_props(deck: &DeckUi) -> DeckProps<'_> {
    let ui = &deck.ui;
    let duration = ui.duration.max(0.0);
    // While scrubbing the waveform, follow the seek target instead of the
    // engine position so the playhead and timer track the pointer.
    let head = if ui.is_seeking {
        ui.seek_position
    } else {
        ui.position
    };
    let title = if ui.track_name.trim().is_empty() {
        "No track loaded"
    } else {
        ui.track_name.as_str()
    };

    DeckProps {
        title,
        subtitle: track_subtitle(ui),
        elapsed: format_time(head.max(0.0)),
        total: format_time(duration),
        wave: waveform_of(deck),
        beats: ui.beat_marks.clone(),
        downbeats: ui.downbeat_marks.clone(),
        view: deck.view.wave,
        eq_bands: &ui.eq_bands,
        timestretch: ts_props(deck),
        progress: playhead_progress(head, duration),
        duration,
        playing: ui.playing,
    }
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
fn ts_props(deck: &DeckUi) -> TsProps {
    TsProps {
        state: deck.view.timestretch,
        backend: deck.controller.stretch().backend(),
        keylock: deck.controller.stretch().keylock(),
    }
}

#[cfg(not(any(feature = "stretch-signalsmith", feature = "stretch-bungee")))]
fn ts_props(deck: &DeckUi) -> TsProps {
    TsProps {
        state: deck.view.timestretch,
    }
}
