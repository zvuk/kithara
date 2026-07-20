use std::{env, fs, io, path::PathBuf};

use iced::{Task, window, window::Screenshot};

use super::{Gallery, Message, mock::Tab};

struct Consts;

impl Consts {
    const ATOMS_CAPTURE_TICK: u32 = 30;
    const ATOMS_SELECT_TICK: u32 = 1;
    const BUTTONS_CAPTURE_TICK: u32 = 40;
    const BUTTONS_SELECT_TICK: u32 = 35;
    const FADERS_CAPTURE_TICK: u32 = 50;
    const FADERS_SELECT_TICK: u32 = 45;
    const MODULES_CAPTURE_TICK: u32 = 60;
    const MODULES_SELECT_TICK: u32 = 55;
    const TYPOGRAPHY_CAPTURE_TICK: u32 = 75;
    const TYPOGRAPHY_SELECT_TICK: u32 = 70;
    const CELLS_CAPTURE_TICK: u32 = 85;
    const CELLS_SELECT_TICK: u32 = 80;
    const SIZES_CAPTURE_TICK: u32 = 95;
    const SIZES_SELECT_TICK: u32 = 90;
    const TRACKLIST_CAPTURE_TICK: u32 = 110;
    const TRACKLIST_SELECT_TICK: u32 = 105;
    const TREE_CAPTURE_TICK: u32 = 125;
    const TREE_SELECT_TICK: u32 = 120;
    const LIBRARY2_CAPTURE_TICK: u32 = 135;
    const LIBRARY2_SELECT_TICK: u32 = 130;
    const STRESS_CAPTURE_TICK: u32 = 540;
    const STRESS_SELECT_TICK: u32 = 140;
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
    if let Err(error) = write_bmp(
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

fn write_bmp(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> io::Result<()> {
    let row = (w as usize) * 4;
    let pixels = row * (h as usize);
    if rgba.len() < pixels {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short pixel buffer",
        ));
    }
    let file_size = u32::try_from(54 + pixels).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(file_size as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&u32::try_from(pixels).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for y in (0..h as usize).rev() {
        for px in rgba[y * row..y * row + row].chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
    }
    fs::write(path, out)
}
