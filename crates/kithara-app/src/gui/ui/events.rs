use kithara::{
    effects::GainDb,
    ui::render::{
        DEFAULT_ZOOM, Scope, UiEvent, WindowCommand, WriteValue, Zoom, zoom_in, zoom_out,
    },
};
use kithara_app_library::Playable;
use num_traits::cast::AsPrimitive;

use super::{
    cache::{DeckLayout, ViewCache},
    scope::deck_index,
};
use crate::{
    deck::{DeckId, EqMode, TempoPercent},
    engine::{Command, MixCmd},
    gui::{
        app::Kithara,
        deck::{DeckMsg, consts::TEMPO_STEP},
        message::Message,
    },
};

pub(crate) fn translate(state: &mut Kithara, event: UiEvent) -> Option<Message> {
    match event {
        UiEvent::Write { key, value } => write(state, &key, value),
        UiEvent::Window(command) => Some(Message::Window(command)),
        _ => None,
    }
}

/// Dispatches a declared write by its endpoint domain.
fn write(state: &mut Kithara, key: &str, value: WriteValue) -> Option<Message> {
    let (id, scope) = Scope::split(key);
    match id.split_once('.')?.0 {
        "deck" => deck_write(state, id, scope, value),
        "mix" | "mixer" => mixer_write(state, id, scope, &value),
        "ui" => ui_write(&mut state.ui.cache, id, scope, &value),
        "broadcast" => broadcast_write(id, &value),
        "library" => {
            library_write(state, id, &value);
            None
        }
        "source" => source_write(state, id, scope, &value),
        _ => None,
    }
}

fn deck_write(
    state: &mut Kithara,
    id: &str,
    scope: Scope<'_>,
    value: WriteValue,
) -> Option<Message> {
    let index = deck_index(scope.get("deck")?)?;
    match id {
        "deck.view.zoom_in" | "deck.view.zoom_out" | "deck.view.zoom" | "deck.queue.load"
        | "deck.eq.mode" => return deck_view_write(state, id, index, scope, value),
        _ => {}
    }
    let msg = match (id, value) {
        ("deck.stream.select_variant", WriteValue::Trigger) => {
            quality_msg(state, index, scope.get("variant")?)?
        }
        ("deck.transport.toggle_play", WriteValue::Trigger) => DeckMsg::TogglePlayPause,
        ("deck.transport.prev", WriteValue::Trigger) => DeckMsg::Prev,
        ("deck.transport.next", WriteValue::Trigger) => DeckMsg::Next,
        ("deck.transport.seek_normalized", WriteValue::Scalar(position)) => {
            DeckMsg::SeekTo(position.clamp(0.0, 1.0))
        }
        ("deck.tempo.rate", WriteValue::Step(steps)) => {
            let tempo = f32::from(state.snapshot.deck(deck_id(state, index)?)?.tempo);
            DeckMsg::SetTempo(TempoPercent::from(steps.mul_add(TEMPO_STEP, tempo)))
        }
        ("deck.tempo.reset", WriteValue::Trigger) => DeckMsg::SetTempo(TempoPercent::DEFAULT),
        (id, WriteValue::Scalar(knob)) => eq_band(state, id.strip_prefix("deck.eq.")?, knob)?,
        _ => return None,
    };
    Some(Message::Deck(deck_id(state, index)?, msg))
}

fn deck_view_write(
    state: &mut Kithara,
    id: &str,
    index: usize,
    scope: Scope<'_>,
    value: WriteValue,
) -> Option<Message> {
    match (id, value) {
        ("deck.view.zoom_in", WriteValue::Trigger) => {
            step_zoom(&mut state.ui.cache, index, zoom_in)
        }
        ("deck.view.zoom_out", WriteValue::Trigger) => {
            step_zoom(&mut state.ui.cache, index, zoom_out)
        }
        ("deck.view.zoom", WriteValue::Scalar(zoom)) => {
            let zoom: f32 = zoom.as_();
            state.ui.cache.deck_mut(index)?.view.zoom =
                Some(f64::from(f32::from(Zoom::from(zoom))));
            None
        }
        ("deck.queue.load", WriteValue::Record(record)) => {
            let track = Playable::try_from(record).ok()?;
            let deck = deck_id(state, index)?;
            state.ui.cache.focus(index);
            state.send(Command::LoadOntoDeck { deck, track });
            None
        }
        ("deck.eq.mode", WriteValue::Trigger) => {
            let mode = match scope.get("bands")? {
                "3" => EqMode::ThreeBand,
                "4" => EqMode::FourBand,
                _ => return None,
            };
            Some(Message::SetEqMode(mode))
        }
        _ => None,
    }
}

fn eq_band(state: &Kithara, name: &str, knob: f64) -> Option<DeckMsg> {
    let band = state.snapshot.eq_mode.band(name)?;
    Some(DeckMsg::EqBandChanged(band, GainDb::at_knob(knob.as_())))
}

fn quality_msg(state: &Kithara, index: usize, variant: &str) -> Option<DeckMsg> {
    if variant == "auto" {
        return Some(DeckMsg::SetQuality(None));
    }
    let slot: usize = variant.parse().ok()?;
    let id = deck_id(state, index)?;
    let rung = state.snapshot.deck(id)?.stream.variants.get(slot)?.index;
    Some(DeckMsg::SetQuality(Some(rung)))
}

fn step_zoom(cache: &mut ViewCache, index: usize, step: fn(Zoom) -> Zoom) -> Option<Message> {
    let deck = cache.deck_mut(index)?;
    let current: f32 = deck.view.zoom.map_or(DEFAULT_ZOOM, AsPrimitive::as_);
    deck.view.zoom = Some(f64::from(f32::from(step(current.into()))));
    None
}

fn mixer_write(state: &Kithara, id: &str, scope: Scope<'_>, value: &WriteValue) -> Option<Message> {
    let cmd = match (id, value) {
        ("mix.crossfader", WriteValue::Scalar(position)) => {
            MixCmd::Crossfader(position.clamp(0.0, 1.0).as_())
        }
        ("mixer.trim", WriteValue::Scalar(trim)) => MixCmd::Trim(
            deck_id(state, deck_index(scope.get("deck")?)?)?,
            trim.clamp(0.0, 1.0).as_(),
        ),
        _ => return None,
    };
    Some(Message::Mix(cmd))
}

fn ui_write(
    cache: &mut ViewCache,
    id: &str,
    scope: Scope<'_>,
    value: &WriteValue,
) -> Option<Message> {
    if *value != WriteValue::Trigger {
        return None;
    }
    match id {
        "ui.window.toggle_full_screen" => Some(Message::Window(WindowCommand::ToggleFullScreen)),
        #[cfg(not(target_arch = "wasm32"))]
        "ui.library.add_folder" => Some(Message::AddMusicFolder),
        "ui.module.toggle" => {
            cache.modules.toggle(scope.get("module")?);
            None
        }
        "ui.layout.apply" => {
            cache.set_layout(DeckLayout::from_decks(scope.get("layout")?.parse().ok()?)?);
            Some(Message::PauseHiddenDecks)
        }
        _ => None,
    }
}

fn broadcast_write(id: &str, value: &WriteValue) -> Option<Message> {
    match (id, value) {
        ("broadcast.toggle", WriteValue::Trigger) => Some(Message::BroadcastToggle),
        _ => None,
    }
}

fn library_write(state: &mut Kithara, id: &str, value: &WriteValue) {
    match (id, value) {
        ("library.select", WriteValue::Index(row)) => state.library.select(*row),
        ("library.toggle", WriteValue::Index(row)) => state.library.toggle(*row),
        _ => {}
    }
}

/// The shell answers its own source writes; the rest are the scoped
/// source's, by their name under `source.`.
fn source_write(
    state: &mut Kithara,
    id: &str,
    scope: Scope<'_>,
    value: &WriteValue,
) -> Option<Message> {
    let source = scope.get("source")?;
    match (id.strip_prefix("source.")?, value) {
        ("select", WriteValue::Index(row)) => state.library.select_row(source, *row),
        ("column.width", WriteValue::Scalar(width)) => {
            state
                .library
                .set_column_width(source, scope.get("column")?, *width);
        }
        (name, value) => state.library.write(source, name, value),
    }
    None
}

fn deck_id(state: &Kithara, index: usize) -> Option<DeckId> {
    state.snapshot.decks.get(index).map(|deck| deck.id)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn press(cache: &mut ViewCache, key: &str) -> Option<Message> {
        let (id, scope) = Scope::split(key);
        ui_write(cache, id, scope, &WriteValue::Trigger)
    }

    fn select_layout(cache: &mut ViewCache, layout: DeckLayout) -> Option<Message> {
        press(cache, &format!("ui.layout.apply@layout={}", layout.decks()))
    }

    fn press_zoom(cache: &mut ViewCache, step: fn(Zoom) -> Zoom) -> f64 {
        step_zoom(cache, 0, step);
        cache.deck_mut(0).and_then(|deck| deck.view.zoom).unwrap()
    }

    #[kithara::test]
    fn the_zoom_buttons_step_the_wave_window_and_stop_at_its_bounds() {
        const PRESSES: usize = 40;

        let mut cache = ViewCache::with_decks(1);

        let narrowed = press_zoom(&mut cache, zoom_in);
        let widened = press_zoom(&mut cache, zoom_out);
        assert!(narrowed < f64::from(DEFAULT_ZOOM), "zoom in must narrow");
        assert!(widened > narrowed, "zoom out must widen");

        for _ in 0..PRESSES {
            press_zoom(&mut cache, zoom_in);
        }
        let floor = press_zoom(&mut cache, zoom_in);
        assert!(floor > 0.0, "the window never closes");
        assert_eq!(press_zoom(&mut cache, zoom_in), floor);

        for _ in 0..PRESSES {
            press_zoom(&mut cache, zoom_out);
        }
        let ceiling = press_zoom(&mut cache, zoom_out);
        assert!(ceiling < 1.0, "the window never spans the whole track");
        assert_eq!(press_zoom(&mut cache, zoom_out), ceiling);
    }

    #[kithara::test]
    fn a_layout_row_applies_the_deck_layout_it_names() {
        let mut cache = ViewCache::default();

        assert!(matches!(
            select_layout(&mut cache, DeckLayout::Single),
            Some(Message::PauseHiddenDecks)
        ));
        assert_eq!(cache.layout(), DeckLayout::Single);

        select_layout(&mut cache, DeckLayout::Dual);
        assert_eq!(cache.layout(), DeckLayout::Dual);
        assert!(press(&mut cache, "ui.layout.apply@layout=3").is_none());
        assert_eq!(cache.layout(), DeckLayout::Dual);
    }

    #[kithara::test]
    fn a_module_cell_switches_its_own_pane() {
        let mut cache = ViewCache::default();

        assert!(cache.modules.is_on("ov"));
        press(&mut cache, "ui.module.toggle@module=ov");
        assert!(!cache.modules.is_on("ov"));
        assert!(cache.modules.is_on("mix"), "one cell switches one pane");

        press(&mut cache, "ui.module.toggle@module=ov");
        assert!(cache.modules.is_on("ov"));
    }

    #[kithara::test]
    fn narrowing_the_layout_moves_a_focus_it_stops_laying_out() {
        let mut cache = ViewCache::default();
        cache.focus(1);
        assert_eq!(cache.focus_deck(), 1);

        select_layout(&mut cache, DeckLayout::Single);

        assert_eq!(cache.focus_deck(), 0, "the key must reach a deck on screen");
    }

    #[cfg(not(feature = "broadcast"))]
    mod writes {
        use std::convert::Infallible;

        use ::kithara::{
            platform::tokio::sync::broadcast::error::TryRecvError,
            play::{CrossfadeCurve, CrossfadeSettings, PlayerStatus},
            queue::QueueEvent,
            ui::render::{ControlAction, DEFAULT_ZOOM, UiEvent, WindowCommand, WriteValue},
        };
        use kithara_test_utils::{kithara, off_thread::OffThread};

        use super::super::translate;
        use crate::{
            analysis::fixtures::{long_wav, short_wav, tone_mp3},
            deck::{DeckId, EqMode, TempoPercent},
            engine::MixCmd,
            gui::{app::Kithara, deck::DeckMsg, message::Message, rig::Rig, ui::cache::DeckLayout},
            state::AbrVariant,
        };

        fn write(state: &mut Kithara, key: &str, value: WriteValue) -> Option<Message> {
            translate(
                state,
                UiEvent::Write {
                    value,
                    key: key.to_owned(),
                },
            )
        }

        fn press(state: &mut Kithara, key: &str) -> Option<Message> {
            write(state, key, WriteValue::Trigger)
        }

        #[kithara::test(native, flash(false))]
        fn the_transport_answers_the_writes_it_declares() {
            let mut rig = Rig::offline();
            rig.message(Message::Deck(
                DeckId(0),
                DeckMsg::SetTempo(TempoPercent::from(3.0)),
            ));
            let state = &mut rig.ui;

            assert!(matches!(
                press(state, "deck.transport.toggle_play@deck=a"),
                Some(Message::Deck(DeckId(0), DeckMsg::TogglePlayPause))
            ));
            assert!(matches!(
                press(state, "deck.transport.prev@deck=a"),
                Some(Message::Deck(DeckId(0), DeckMsg::Prev))
            ));
            assert!(matches!(
                press(state, "deck.transport.next@deck=b"),
                Some(Message::Deck(DeckId(1), DeckMsg::Next))
            ));
            assert!(matches!(
                write(state, "deck.transport.seek_normalized@deck=a", WriteValue::Scalar(0.25)),
                Some(Message::Deck(DeckId(0), DeckMsg::SeekTo(fraction)))
                    if (fraction - 0.25).abs() < f64::EPSILON
            ));
            assert!(matches!(
                write(state, "deck.tempo.rate@deck=a", WriteValue::Step(2.0)),
                Some(Message::Deck(DeckId(0), DeckMsg::SetTempo(tempo)))
                    if (f32::from(tempo) - 6.0).abs() < f32::EPSILON
            ));
            assert!(matches!(
                press(state, "deck.tempo.reset@deck=a"),
                Some(Message::Deck(DeckId(0), DeckMsg::SetTempo(tempo)))
                    if tempo == TempoPercent::DEFAULT
            ));
            assert!(matches!(
                write(state, "mixer.trim@deck=a", WriteValue::Scalar(2.0)),
                Some(Message::Mix(MixCmd::Trim(DeckId(0), trim)))
                    if (trim - 1.0).abs() < f32::EPSILON
            ));
            assert!(matches!(
                press(state, "broadcast.toggle"),
                Some(Message::BroadcastToggle)
            ));
        }

        #[kithara::test(native, flash(false))]
        fn the_wave_window_answers_its_zoom_writes() {
            let mut rig = Rig::offline();
            let state = &mut rig.ui;
            let zoom =
                |state: &mut Kithara| state.ui.cache.deck_mut(0).and_then(|deck| deck.view.zoom);

            assert!(press(state, "deck.view.zoom_in@deck=a").is_none());
            let narrowed = zoom(state).expect("zooming in sets the window");
            assert!(narrowed < f64::from(DEFAULT_ZOOM), "zoom in must narrow");
            assert!(press(state, "deck.view.zoom_out@deck=a").is_none());
            assert!(zoom(state).is_some_and(|widened| widened > narrowed));

            assert!(write(state, "deck.view.zoom@deck=a", WriteValue::Scalar(0.5)).is_none());
            assert_eq!(zoom(state), Some(0.5));
        }

        #[kithara::test(native, flash(false))]
        fn the_mixer_answers_the_writes_it_declares() {
            let mut rig = Rig::offline();
            let state = &mut rig.ui;

            assert!(matches!(
                write(state, "mix.crossfader", WriteValue::Scalar(1.5)),
                Some(Message::Mix(MixCmd::Crossfader(position)))
                    if (position - 1.0).abs() < f32::EPSILON
            ));
            assert!(matches!(
                press(state, "deck.eq.mode@bands=4,deck=a"),
                Some(Message::SetEqMode(EqMode::FourBand))
            ));
            assert!(matches!(
                press(state, "deck.eq.mode@bands=3,deck=b"),
                Some(Message::SetEqMode(EqMode::ThreeBand))
            ));

            for (mode, expected) in [
                (EqMode::ThreeBand, [Some(0), None, Some(1), None, Some(2)]),
                (EqMode::FourBand, [Some(0), Some(1), None, Some(2), Some(3)]),
            ] {
                rig.message(Message::SetEqMode(mode));
                rig.pump();
                crate::mock::render_rig(&mut rig, 128);
                rig.frame();
                assert_eq!(rig.ui.snapshot.eq_mode, mode);

                for (name, band) in ["low", "low_mid", "mid", "high_mid", "high"]
                    .into_iter()
                    .zip(expected)
                {
                    let message = write(
                        &mut rig.ui,
                        &format!("deck.eq.{name}@deck=a"),
                        WriteValue::Scalar(1.0),
                    );
                    let actual = match message {
                        Some(Message::Deck(DeckId(0), DeckMsg::EqBandChanged(index, _))) => {
                            Some(index)
                        }
                        None => None,
                        other => panic!("{mode:?} {name}: unexpected {other:?}"),
                    };
                    assert_eq!(actual, band, "{mode:?} {name}");
                }
            }
        }

        #[kithara::test(native, flash(false))]
        fn each_deck_picks_the_quality_its_row_names() {
            let mut rig = Rig::offline();
            rig.shows(DeckId(0), |deck| {
                deck.abr_variants = vec![AbrVariant {
                    index: 7,
                    label: "320k".to_string(),
                    detail: "320 kbps".to_string(),
                }];
            });
            let state = &mut rig.ui;

            assert!(matches!(
                press(state, "deck.stream.select_variant@deck=a,variant=0"),
                Some(Message::Deck(DeckId(0), DeckMsg::SetQuality(Some(7))))
            ));
            assert!(matches!(
                press(state, "deck.stream.select_variant@deck=a,variant=auto"),
                Some(Message::Deck(DeckId(0), DeckMsg::SetQuality(None)))
            ));
        }

        #[kithara::test(native, flash(false))]
        fn the_app_menu_answers_the_writes_it_declares() {
            let mut rig = Rig::offline();
            let state = &mut rig.ui;

            assert!(matches!(
                press(state, "ui.window.toggle_full_screen"),
                Some(Message::Window(WindowCommand::ToggleFullScreen))
            ));
            assert!(matches!(
                press(state, "ui.layout.apply@layout=1"),
                Some(Message::PauseHiddenDecks)
            ));
            assert_eq!(state.ui.cache.layout(), DeckLayout::Single);

            assert!(state.ui.cache.modules.is_on("ov"));
            assert!(press(state, "ui.module.toggle@module=ov").is_none());
            assert!(!state.ui.cache.modules.is_on("ov"));
        }

        #[kithara::test(native, flash(false))]
        fn library_followup_width_writes_stay_with_their_source() {
            let mut rig = Rig::offline();
            rig.send(
                "library/pages/startup/rows/width/artist",
                ControlAction::SetScalar(240.0),
            );
            assert_eq!(
                rig.scalar("source.column.width@column=artist,source=startup"),
                240.0
            );
            write(
                &mut rig.ui,
                "source.column.width@column=artist,source=explorer",
                WriteValue::Scalar(260.0),
            );
            assert_eq!(
                rig.scalar("source.column.width@column=artist,source=explorer"),
                260.0
            );
            assert_eq!(
                rig.scalar("source.column.width@column=artist,source=startup"),
                240.0
            );
        }

        fn until_current(rig: &mut Rig, deck: usize, name: &str) {
            rig.until(
                "the dropped track becomes current",
                Rig::DEADLINE,
                |rig| {
                    crate::mock::render_rig(rig, 512);
                    rig.engine.tick();
                    rig.engine.publish();
                },
                |rig| {
                    rig.queues[deck]
                        .current()
                        .is_some_and(|track| track.name == name)
                },
            );
        }

        fn names(rig: &Rig, deck: usize) -> Vec<String> {
            rig.queues[deck]
                .tracks()
                .into_iter()
                .map(|track| track.name)
                .collect()
        }

        async fn with_rig(check: impl FnOnce(&mut Rig) + Send + 'static) {
            let rig = OffThread::spawn("app-host", || Ok::<_, Infallible>(Rig::offline()))
                .await
                .expect("rig fixture is infallible");
            rig.call(check).await;
            rig.close().await;
        }

        #[kithara::test(native, tokio, flash(false))]
        async fn a_row_dropped_on_deck_b_loads_onto_deck_b(tone_mp3: String, long_wav: String) {
            with_rig(move |rig| {
                rig.drop_on("b", &long_wav);
                let [name] = names(rig, 1)
                    .try_into()
                    .expect("deck B holds the dropped track");
                until_current(rig, 1, &name);

                let queue = rig.queues[1].clone();
                let outgoing = queue.current().expect("the first drop is current").id;
                let settings = CrossfadeSettings::new(0.125, CrossfadeCurve::Linear, 1.0, 0.5)
                    .expect("a nonzero crossfade fits both fixtures");
                queue
                    .set_crossfade_settings(settings)
                    .expect("the deck accepts its crossfade profile");
                rig.until(
                    "the first dropped track is playing before the next drop",
                    Rig::DEADLINE,
                    |_| {},
                    |_| {
                        queue.is_playing()
                            && queue
                                .position_seconds()
                                .is_some_and(|position| position > 0.0)
                    },
                );

                let mut events = queue.subscribe::<QueueEvent>();
                rig.drop_on("b", &tone_mp3);
                let incoming = queue
                    .tracks()
                    .into_iter()
                    .find(|track| track.id != outgoing)
                    .expect("the playing deck takes the second dropped track")
                    .id;
                let mut crossfades = 0;
                let mut changed = false;
                rig.until(
                    "a drop on the playing deck crossfades and hands over its transport",
                    Rig::DEADLINE,
                    |_| {},
                    |_| {
                        loop {
                            let event = match events.try_recv() {
                                Ok(event) => event,
                                Err(TryRecvError::Empty) => break,
                                Err(error) => {
                                    panic!("the drop event stream lost evidence: {error}")
                                }
                            };
                            match event.event {
                                QueueEvent::CrossfadeStarted { settings: applied } => {
                                    assert_eq!(applied, settings);
                                    crossfades += 1;
                                }
                                QueueEvent::CurrentTrackChanged { id: Some(id) }
                                    if id == incoming =>
                                {
                                    assert_eq!(
                                        crossfades, 1,
                                        "the real fade starts before handover"
                                    );
                                    changed = true;
                                }
                                _ => {}
                            }
                        }
                        changed && queue.is_playing()
                    },
                );
                assert_eq!(
                    queue.current().expect("the incoming track is current").id,
                    incoming
                );
                rig.until(
                    "the incoming track plays beyond the short fade",
                    Rig::DEADLINE,
                    |_| {},
                    |_| {
                        queue
                            .position_seconds()
                            .is_some_and(|position| position > 0.25)
                    },
                );

                queue.pause();
                rig.until(
                    "the dropped track still answers Pause",
                    Rig::DEADLINE,
                    |_| {},
                    |_| !queue.is_playing(),
                );
                queue
                    .seek(0.5)
                    .expect("the dropped track still accepts seek");
                rig.until(
                    "the paused dropped track reaches the requested cue",
                    Rig::DEADLINE,
                    |_| {},
                    |_| {
                        queue
                            .position_seconds()
                            .is_some_and(|position| (position - 0.5).abs() < 0.05)
                    },
                );
                queue.play();
                rig.until(
                    "the dropped track still answers Play and advances from its cue",
                    Rig::DEADLINE,
                    |_| {},
                    |_| {
                        queue.is_playing()
                            && queue
                                .position_seconds()
                                .is_some_and(|position| position > 0.625)
                    },
                );
                queue.pause();
                rig.until(
                    "the deck is actually paused before another drop",
                    Rig::DEADLINE,
                    |_| {},
                    |_| !queue.is_playing(),
                );

                assert_eq!(
                    queue.current().expect("the paused item remains current").id,
                    incoming
                );
                assert_eq!(queue.status(), PlayerStatus::ReadyToPlay);
                let duration = queue
                    .duration_seconds()
                    .expect("the incoming duration is known");
                assert!(
                    queue
                        .position_seconds()
                        .is_some_and(|position| position < duration - 0.25),
                    "the nonplaying item is paused before its end"
                );
                rig.drop_on("b", &long_wav);
                until_current(rig, 1, &name);
                loop {
                    let event = match events.try_recv() {
                        Ok(event) => event,
                        Err(TryRecvError::Empty) => break,
                        Err(error) => panic!("the paused drop event stream lost evidence: {error}"),
                    };
                    assert!(
                        !matches!(event.event, QueueEvent::CrossfadeStarted { .. }),
                        "a drop on a paused deck selects without a crossfade"
                    );
                }
                assert_eq!(
                    queue.current().expect("the paused selection completes").id,
                    outgoing
                );
                assert_eq!(
                    queue.tracks().len(),
                    2,
                    "dropping a queued source does not duplicate it"
                );
                assert!(names(rig, 0).is_empty(), "deck A was not the target");
            })
            .await;
        }

        #[kithara::test(native, tokio, flash(false))]
        async fn the_same_source_dropped_twice_stays_one_entry_and_current(
            tone_mp3: String,
            short_wav: String,
        ) {
            with_rig(move |rig| {
                rig.drop_on("a", &tone_mp3);
                rig.drop_on("a", &short_wav);
                let [tone, wav] = names(rig, 0).try_into().expect("deck A holds both tracks");
                until_current(rig, 0, &wav);

                rig.drop_on("a", &tone_mp3);

                assert_eq!(names(rig, 0), [tone.clone(), wav]);
                until_current(rig, 0, &tone);
            })
            .await;
        }
    }
}
