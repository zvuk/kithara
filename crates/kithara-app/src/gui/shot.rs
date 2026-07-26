use std::{env, path::PathBuf};

use iced::{Task, window, window::Screenshot};
use kithara_ui::render::bmp;
use tracing::{error, info};

use super::{app::Kithara, message::Message};

/// Ticks to let the studio settle before the frame is taken.
const CAPTURE_TICK: u32 = 40;

/// Screenshot pass driven by `KITHARA_SHOT_DIR`: after [`CAPTURE_TICK`] ticks
/// the studio window is captured to `studio.bmp` and the app exits.
pub(crate) struct ShotPlan {
    dir: PathBuf,
    tick: u32,
}

impl ShotPlan {
    pub(crate) fn read() -> Option<Self> {
        let dir = env::var_os("KITHARA_SHOT_DIR")?;
        Some(Self {
            dir: PathBuf::from(dir),
            tick: 0,
        })
    }
}

pub(crate) fn drive(state: &mut Kithara) -> Task<Message> {
    let Some(plan) = state.shot.as_mut() else {
        return Task::none();
    };
    plan.tick += 1;
    if plan.tick != CAPTURE_TICK {
        return Task::none();
    }
    window::screenshot(state.window_id).map(Message::Shot)
}

pub(crate) fn save(state: &Kithara, screenshot: &Screenshot) -> Task<Message> {
    let Some(plan) = state.shot.as_ref() else {
        return Task::none();
    };
    let path = plan.dir.join("studio.bmp");
    match bmp::write(
        &path,
        screenshot.size.width,
        screenshot.size.height,
        &screenshot.rgba,
    ) {
        Ok(()) => {
            info!(path = %path.display(), width = screenshot.size.width, height = screenshot.size.height, "studio screenshot written");
        }
        Err(error) => error!(path = %path.display(), %error, "failed to write studio screenshot"),
    }
    iced::exit()
}
