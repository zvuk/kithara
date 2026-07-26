use std::{env, path::PathBuf};

use iced::{Task, window, window::Screenshot};
use kithara_ui::render::bmp;

use super::{Gallery, Message, sections::Tab};

struct Consts;

impl Consts {
    const ATOMS_CAPTURE_TICK: u32 = 30;
    const ATOMS_SELECT_TICK: u32 = 1;
    const BUTTONS_CAPTURE_TICK: u32 = 40;
    const BUTTONS_SELECT_TICK: u32 = 35;
    const CELLS_CAPTURE_TICK: u32 = 85;
    const CELLS_SELECT_TICK: u32 = 80;
    const CHROME_CAPTURE_TICK: u32 = 135;
    const CHROME_SELECT_TICK: u32 = 130;
    const FADERS_CAPTURE_TICK: u32 = 50;
    const FADERS_SELECT_TICK: u32 = 45;
    const LIBRARY2_CAPTURE_TICK: u32 = 190;
    const LIBRARY2_SELECT_TICK: u32 = 185;
    const MICRO_CAPTURE_TICK: u32 = 115;
    const MICRO_SELECT_TICK: u32 = 110;
    const MIXER_CAPTURE_TICK: u32 = 125;
    const MIXER_SELECT_TICK: u32 = 120;
    const MODULES_CAPTURE_TICK: u32 = 60;
    const MODULES_SELECT_TICK: u32 = 55;
    const SIZES_CAPTURE_TICK: u32 = 95;
    const SIZES_SELECT_TICK: u32 = 90;
    const STRESS_CAPTURE_TICK: u32 = 590;
    const STRESS_SELECT_TICK: u32 = 225;
    const TITLEBARS_CAPTURE_TICK: u32 = 145;
    const TITLEBARS_SELECT_TICK: u32 = 140;
    const TOKENS_CAPTURE_TICK: u32 = 105;
    const TOKENS_SELECT_TICK: u32 = 100;
    const TRACKLIST_CAPTURE_TICK: u32 = 160;
    const TRACKLIST_SELECT_TICK: u32 = 155;
    const TREE_CAPTURE_TICK: u32 = 175;
    const TREE_SELECT_TICK: u32 = 170;
    const TYPOGRAPHY_CAPTURE_TICK: u32 = 75;
    const TYPOGRAPHY_SELECT_TICK: u32 = 70;
    const VIS_CAPTURE_TICK: u32 = 220;
    const VIS_SELECT_TICK: u32 = 195;
}

pub(super) struct ShotPlan {
    dir: PathBuf,
    tick: u32,
}

impl ShotPlan {
    pub(super) fn read() -> Option<Self> {
        let dir = env::var_os("KITHARA_SHOT_DIR")?;
        Some(Self {
            dir: PathBuf::from(dir),
            tick: 0,
        })
    }
}

pub(super) fn drive(state: &mut Gallery) -> Task<Message> {
    let tick = {
        let Some(plan) = state.shot.as_mut() else {
            return Task::none();
        };
        plan.tick += 1;
        plan.tick
    };

    match tick {
        Consts::ATOMS_SELECT_TICK => {
            state.select_tab(Tab::Atoms);
            Task::none()
        }
        Consts::ATOMS_CAPTURE_TICK => capture(state.window_id, "tab-atoms"),
        Consts::BUTTONS_SELECT_TICK => {
            state.select_tab(Tab::Buttons);
            Task::none()
        }
        Consts::BUTTONS_CAPTURE_TICK => capture(state.window_id, "tab-buttons"),
        Consts::FADERS_SELECT_TICK => {
            state.select_tab(Tab::Faders);
            Task::none()
        }
        Consts::FADERS_CAPTURE_TICK => capture(state.window_id, "tab-faders"),
        Consts::MODULES_SELECT_TICK => {
            state.select_tab(Tab::Modules);
            Task::none()
        }
        Consts::MODULES_CAPTURE_TICK => capture(state.window_id, "tab-modules"),
        Consts::TYPOGRAPHY_SELECT_TICK => {
            state.select_tab(Tab::Typography);
            Task::none()
        }
        Consts::TYPOGRAPHY_CAPTURE_TICK => capture(state.window_id, "tab-typography"),
        Consts::CELLS_SELECT_TICK => {
            state.select_tab(Tab::Cells);
            Task::none()
        }
        Consts::CELLS_CAPTURE_TICK => capture(state.window_id, "tab-cells"),
        Consts::SIZES_SELECT_TICK => {
            state.select_tab(Tab::Sizes);
            Task::none()
        }
        Consts::SIZES_CAPTURE_TICK => capture(state.window_id, "tab-sizes"),
        Consts::TOKENS_SELECT_TICK => {
            state.select_tab(Tab::Tokens);
            Task::none()
        }
        Consts::TOKENS_CAPTURE_TICK => capture(state.window_id, "tab-tokens"),
        Consts::MICRO_SELECT_TICK => {
            state.select_tab(Tab::Micro);
            Task::none()
        }
        Consts::MICRO_CAPTURE_TICK => capture(state.window_id, "tab-micro"),
        Consts::MIXER_SELECT_TICK => {
            state.select_tab(Tab::Mixer);
            Task::none()
        }
        Consts::MIXER_CAPTURE_TICK => capture(state.window_id, "tab-mixer"),
        Consts::CHROME_SELECT_TICK => {
            state.select_tab(Tab::Chrome);
            Task::none()
        }
        Consts::CHROME_CAPTURE_TICK => capture(state.window_id, "tab-chrome"),
        Consts::TITLEBARS_SELECT_TICK => {
            state.select_tab(Tab::Titlebars);
            Task::none()
        }
        Consts::TITLEBARS_CAPTURE_TICK => capture(state.window_id, "tab-titlebars"),
        Consts::TRACKLIST_SELECT_TICK => {
            state.select_tab(Tab::Tracklist);
            Task::none()
        }
        Consts::TRACKLIST_CAPTURE_TICK => capture(state.window_id, "tab-tracklist"),
        Consts::TREE_SELECT_TICK => {
            state.select_tab(Tab::Tree);
            Task::none()
        }
        Consts::TREE_CAPTURE_TICK => capture(state.window_id, "tab-tree"),
        Consts::LIBRARY2_SELECT_TICK => {
            state.select_tab(Tab::Library2);
            Task::none()
        }
        Consts::LIBRARY2_CAPTURE_TICK => capture(state.window_id, "tab-library2"),
        Consts::VIS_SELECT_TICK => {
            state.select_tab(Tab::Vis);
            Task::none()
        }
        Consts::VIS_CAPTURE_TICK => capture(state.window_id, "tab-vis"),
        Consts::STRESS_SELECT_TICK => {
            state.select_tab(Tab::Stress);
            Task::none()
        }
        Consts::STRESS_CAPTURE_TICK => capture(state.window_id, "tab-stress"),
        _ => Task::none(),
    }
}

fn capture(id: window::Id, name: &'static str) -> Task<Message> {
    window::screenshot(id).map(move |screenshot| Message::Shot(name, screenshot))
}

pub(super) fn save(state: &Gallery, name: &str, screenshot: &Screenshot) -> Task<Message> {
    let Some(plan) = state.shot.as_ref() else {
        return Task::none();
    };
    let path = plan.dir.join(format!("{name}.bmp"));
    if let Err(error) = bmp::write(
        &path,
        screenshot.size.width,
        screenshot.size.height,
        &screenshot.rgba,
    ) {
        eprintln!("failed to save {}: {error}", path.display());
    }
    if name == "tab-stress" {
        iced::exit()
    } else {
        Task::none()
    }
}
