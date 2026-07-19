mod mock;
mod shot;

use iced::{
    Color, Element, Size, Subscription, Task, Theme, time as iced_time, window, window::Settings,
};
use kithara_platform::time::Duration;
use kithara_ui::{
    compile::{CompiledUi, compile},
    render::{RenderPalette, UiEvent, fonts, tree},
    source::{MemResolver, UiConfig},
};

use self::mock::{MockReads, Tab};

struct Consts;

impl Consts {
    const CAPTURE_TICK_MS: u64 = 100;
    const HEIGHT: f32 = 720.0;
    const WIDTH: f32 = 1100.0;
}

const ASSETS: &[(&str, &str)] = &[
    (
        "gallery-atoms.klayout.ron",
        include_str!("assets/gallery-atoms.klayout.ron"),
    ),
    (
        "gallery-buttons.klayout.ron",
        include_str!("assets/gallery-buttons.klayout.ron"),
    ),
    (
        "gallery-faders.klayout.ron",
        include_str!("assets/gallery-faders.klayout.ron"),
    ),
    (
        "modules/tab-bar-shell.kmodule.ron",
        include_str!("assets/modules/tab-bar-shell.kmodule.ron"),
    ),
    (
        "modules/tab-bar.kmodule.ron",
        include_str!("assets/modules/tab-bar.kmodule.ron"),
    ),
    (
        "modules/tabs/atoms.kmodule.ron",
        include_str!("assets/modules/tabs/atoms.kmodule.ron"),
    ),
    (
        "modules/tabs/buttons.kmodule.ron",
        include_str!("assets/modules/tabs/buttons.kmodule.ron"),
    ),
    (
        "modules/tabs/faders.kmodule.ron",
        include_str!("assets/modules/tabs/faders.kmodule.ron"),
    ),
    (
        "modules/primitives/knobs.kmodule.ron",
        include_str!("assets/modules/primitives/knobs.kmodule.ron"),
    ),
    (
        "modules/primitives/meters.kmodule.ron",
        include_str!("assets/modules/primitives/meters.kmodule.ron"),
    ),
    (
        "modules/primitives/toggles.kmodule.ron",
        include_str!("assets/modules/primitives/toggles.kmodule.ron"),
    ),
    (
        "modules/primitives/readouts.kmodule.ron",
        include_str!("assets/modules/primitives/readouts.kmodule.ron"),
    ),
    (
        "modules/primitives/chips.kmodule.ron",
        include_str!("assets/modules/primitives/chips.kmodule.ron"),
    ),
];

#[derive(Clone, Debug)]
enum Message {
    Close(window::Id),
    Shot(&'static str, window::Screenshot),
    Tick,
    Ui(UiEvent),
}

struct Gallery {
    layouts: [CompiledUi; 3],
    palette: RenderPalette,
    reads: MockReads,
    shot: Option<shot::ShotPlan>,
    window_id: window::Id,
}

impl Gallery {
    fn new() -> (Self, Task<Message>) {
        let resolver = resolver();
        let catalog = tree::catalog();
        let endpoints = mock::registry();
        let layouts = Tab::ALL.map(|tab| {
            compile(
                tab.entry(),
                &resolver,
                &catalog,
                &endpoints,
                &UiConfig::default(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "embedded gallery document {} must compile: {error}",
                    tab.entry()
                )
            })
        });
        let settings = Settings {
            size: Size::new(Consts::WIDTH, Consts::HEIGHT),
            min_size: Some(Size::new(Consts::WIDTH, Consts::HEIGHT)),
            exit_on_close_request: false,
            ..Settings::default()
        };
        let (window_id, open) = window::open(settings);
        (
            Self {
                layouts,
                palette: palette(),
                reads: MockReads::default(),
                shot: shot::ShotPlan::read(),
                window_id,
            },
            open.discard(),
        )
    }

    fn compiled(&self) -> &CompiledUi {
        &self.layouts[self.reads.active_tab().index()]
    }

    fn select_tab(&mut self, tab: Tab) {
        self.reads.select_tab(tab);
    }
}

fn main() -> iced::Result {
    let daemon = iced::daemon(Gallery::new, update, view)
        .title(|_state: &Gallery, _window| "Kithara UI Gallery".to_owned())
        .theme(|state: &Gallery, _window| theme(state.palette))
        .subscription(subscription)
        .default_font(fonts::SANS);
    fonts::FONT_BYTES
        .iter()
        .fold(daemon, |daemon, bytes| daemon.font(*bytes))
        .run()
}

fn update(state: &mut Gallery, message: Message) -> Task<Message> {
    match message {
        Message::Close(id) if id == state.window_id => iced::exit(),
        Message::Close(id) => window::close(id),
        Message::Shot(name, screenshot) => shot::save(state, name, &screenshot),
        Message::Tick => shot::drive(state),
        Message::Ui(UiEvent::Control { path, action }) => {
            if let Ok(tab) = Tab::try_from(path.as_str()) {
                state.select_tab(tab);
            } else {
                state.reads.apply(&path, &action);
            }
            Task::none()
        }
        Message::Ui(_) => Task::none(),
    }
}

fn view(state: &Gallery, _window: window::Id) -> Element<'_, Message> {
    tree::render(
        &state.compiled().root,
        state.compiled(),
        &state.reads,
        state.palette,
    )
    .map(Message::Ui)
}

fn subscription(state: &Gallery) -> Subscription<Message> {
    let close = window::close_requests().map(Message::Close);
    if state.shot.is_some() {
        Subscription::batch([
            close,
            iced_time::every(Duration::from_millis(Consts::CAPTURE_TICK_MS)).map(|_| Message::Tick),
        ])
    } else {
        close
    }
}

fn resolver() -> MemResolver {
    let mut resolver = MemResolver::default();
    for (path, text) in ASSETS {
        resolver.insert(path, text);
    }
    resolver
}

fn palette() -> RenderPalette {
    RenderPalette::builder()
        .bg(Color::from_rgb8(18, 18, 31))
        .bg_deep(Color::from_rgb8(11, 11, 22))
        .bg_inset(Color::from_rgb8(21, 21, 42))
        .bg_panel(Color::from_rgb8(32, 32, 58))
        .bg_panel_2(Color::from_rgb8(38, 38, 74))
        .bg_select(Color::from_rgb8(38, 38, 74))
        .line(Color::from_rgb8(59, 59, 103))
        .line_soft(Color::from_rgb8(42, 42, 76))
        .text(Color::from_rgb8(230, 230, 230))
        .text_dim(Color::from_rgb8(167, 170, 194))
        .muted(Color::from_rgb8(111, 113, 137))
        .accent(Color::from_rgb8(187, 148, 66))
        .accent_strong(Color::from_rgb8(214, 173, 89))
        .accent_soft(Color::from_rgba8(187, 148, 66, 0.18))
        .danger(Color::from_rgb8(230, 77, 77))
        .success(Color::from_rgb8(102, 204, 102))
        .warning(Color::from_rgb8(230, 179, 51))
        .wave_low(Color::from_rgb8(235, 41, 140))
        .wave_mid(Color::from_rgb8(242, 209, 41))
        .wave_high(Color::from_rgb8(46, 199, 235))
        .build()
}

fn theme(palette: RenderPalette) -> Theme {
    Theme::custom(
        "Kithara".to_owned(),
        iced::theme::Palette {
            background: palette.bg,
            text: palette.text,
            primary: palette.accent,
            success: palette.success,
            danger: palette.danger,
            warning: palette.warning,
        },
    )
}
