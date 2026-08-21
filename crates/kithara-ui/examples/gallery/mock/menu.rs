use kithara_ui::render::ReadValue;

struct Consts;

impl Consts {
    const CLUB_MODULES: [bool; 11] = [
        true, true, true, false, true, true, false, false, true, true, true,
    ];
    const DISPLAYS: [&str; 3] = ["MACBOOK PRO 16\"", "DELL U2720Q", "IPAD · SIDECAR"];
    const LAYOUTS: [&str; 4] = [
        "CLUB · 2 DECKS",
        "STUDIO · 4 DECKS + VST",
        "VISUALS + TIMELINE",
        "NARROW WINDOW · TABS",
    ];
    const MAX_WINDOWS: usize = 3;
    const MODULES: [&str; 11] = [
        "ov", "mix", "fx1", "fx2", "vcf", "rec", "vis", "tl", "cpu", "net", "buf",
    ];
    const NEW_WINDOW_LAYOUT: usize = 3;
    const NEW_WINDOW_MODULES: [bool; 11] = [
        true, true, false, false, false, false, false, false, false, false, false,
    ];
    const TRACKS: [MenuTrack; 4] = [
        MenuTrack {
            title: "AURORA DRIFT",
            meta: "124 · 8A",
            energy: 0.72,
        },
        MenuTrack {
            title: "NIGHT SHIFT",
            meta: "126 · 5A",
            energy: 0.54,
        },
        MenuTrack {
            title: "PARALLAX",
            meta: "128 · 11B",
            energy: 0.83,
        },
        MenuTrack {
            title: "SLOW BURN",
            meta: "120 · 2A",
            energy: 0.36,
        },
    ];
    const VISUAL_MODULES: [bool; 11] = [
        false, false, false, false, false, false, true, true, false, false, false,
    ];
}

struct MenuTrack {
    meta: &'static str,
    title: &'static str,
    energy: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MenuGroup {
    Win,
    Mod,
    Lay,
    None,
}

struct MenuWindow {
    caption: String,
    title: String,
    modules: [bool; 11],
    display: usize,
    layout: usize,
}

impl MenuWindow {
    const fn new(display: usize, layout: usize, modules: [bool; 11]) -> Self {
        Self {
            display,
            layout,
            modules,
            title: String::new(),
            caption: String::new(),
        }
    }

    fn modules_on(&self) -> usize {
        self.modules.iter().filter(|on| **on).count()
    }
}

pub(super) struct MenuState {
    group: MenuGroup,
    count_label: String,
    modules_count: String,
    modules_title: String,
    windows: Vec<MenuWindow>,
    autogain: bool,
    casting: bool,
    mono: bool,
    open: bool,
    recording: bool,
    wave_follow: bool,
    active: usize,
}

impl Default for MenuState {
    fn default() -> Self {
        let mut state = Self {
            open: true,
            group: MenuGroup::Win,
            windows: vec![
                MenuWindow::new(0, 0, Consts::CLUB_MODULES),
                MenuWindow::new(1, 2, Consts::VISUAL_MODULES),
            ],
            active: 0,
            wave_follow: true,
            autogain: true,
            mono: false,
            recording: false,
            casting: true,
            count_label: String::new(),
            modules_title: String::new(),
            modules_count: String::new(),
        };
        state.rebuild();
        state
    }
}

impl MenuState {
    pub(super) fn activate(&mut self, path: &str) -> bool {
        let Some(id) = path.strip_prefix("app-menu/") else {
            return false;
        };
        let (instance, node) = id.split_once('/').unwrap_or((id, ""));
        if let Some(number) = instance.strip_prefix("window-") {
            return self.window_row(number, node);
        }
        if let Some(key) = instance.strip_prefix("module-") {
            return self.toggle_module(key);
        }
        if let Some(number) = instance.strip_prefix("layout-") {
            return self.apply_layout(number);
        }
        match instance {
            // The widget publishes the dismissal on the popover's own path, so
            // this handler sets false; the burger stays the only toggle.
            "pop" | "header-close" => self.open = false,
            "burger" => self.open = !self.open,
            "new-window" => self.open_window(),
            "modules-head" => self.toggle_group(MenuGroup::Mod),
            "layouts-head" => self.toggle_group(MenuGroup::Lay),
            "wave-follow" => self.wave_follow = !self.wave_follow,
            "autogain" => self.autogain = !self.autogain,
            "mono" => self.mono = !self.mono,
            "record" => self.recording = !self.recording,
            "cast" => self.casting = !self.casting,
            "full-screen" | "add-folder" | "settings" => {}
            _ => return false,
        }
        true
    }

    fn apply_layout(&mut self, number: &str) -> bool {
        let Some(index) = row_index(number).filter(|index| *index < Consts::LAYOUTS.len()) else {
            return false;
        };
        let active = self.active;
        self.windows[active].layout = index;
        self.rebuild();
        true
    }

    pub(super) const fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    const fn can_open(&self) -> bool {
        self.windows.len() < Consts::MAX_WINDOWS
    }

    const fn closable(&self, index: usize) -> bool {
        self.windows.len() > 1 && index != 0
    }

    fn close(&mut self, index: usize) {
        if !self.closable(index) {
            return;
        }
        self.windows.remove(index);
        if self.active >= index {
            self.active -= 1;
        }
        self.rebuild();
    }

    fn cycle_display(&mut self, index: usize) {
        let Some(window) = self.windows.get_mut(index) else {
            return;
        };
        window.display = (window.display + 1) % Consts::DISPLAYS.len();
        self.rebuild();
    }

    fn focus(&mut self, index: usize) {
        if index < self.windows.len() {
            self.active = index;
            self.rebuild();
        }
    }

    pub(super) fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let (id, scope) = endpoint.split_once('@').unwrap_or((endpoint, ""));
        let value = match id {
            "ui.menu.open" => ReadValue::Bool(self.open),
            "ui.menu.group_open" => ReadValue::Bool(self.group_open(scope)),
            "ui.menu.group_hidden" => ReadValue::Bool(!self.group_open(scope)),
            "ui.window.count" => ReadValue::Text(&self.count_label),
            "ui.window.can_open" => ReadValue::Bool(self.can_open()),
            "ui.window.active" => ReadValue::Bool(scoped_index(scope, "window")? == self.active),
            "ui.window.hidden" => {
                ReadValue::Bool(scoped_index(scope, "window")? >= self.windows.len())
            }
            "ui.window.close_hidden" => {
                ReadValue::Bool(!self.closable(scoped_index(scope, "window")?))
            }
            "ui.window.title" => {
                ReadValue::Text(&self.windows.get(scoped_index(scope, "window")?)?.title)
            }
            "ui.window.caption" => {
                ReadValue::Text(&self.windows.get(scoped_index(scope, "window")?)?.caption)
            }
            "ui.module.on" => {
                ReadValue::Bool(self.windows[self.active].modules[module_index(scope)?])
            }
            "ui.modules.title" => ReadValue::Text(&self.modules_title),
            "ui.modules.count" => ReadValue::Text(&self.modules_count),
            "ui.layout.selected" => {
                ReadValue::Bool(scoped_index(scope, "layout")? == self.windows[self.active].layout)
            }
            "ui.layouts.active" => {
                ReadValue::Text(Consts::LAYOUTS[self.windows[self.active].layout])
            }
            "ui.prefs.wave_follow" => ReadValue::Bool(self.wave_follow),
            "ui.prefs.autogain" => ReadValue::Bool(self.autogain),
            "ui.prefs.mono" => ReadValue::Bool(self.mono),
            "ui.set.recording" => ReadValue::Bool(self.recording),
            "ui.set.casting" => ReadValue::Bool(self.casting),
            "ui.set.record_hint" => {
                ReadValue::Text(if self.recording { "RECORDING" } else { "⌘R" })
            }
            "ui.set.cast_hint" => ReadValue::Text(if self.casting { "AUDIO LIVE" } else { "OFF" }),
            _ => return None,
        };
        Some(value)
    }

    fn group_open(&self, scope: &str) -> bool {
        match self.group {
            MenuGroup::Mod => scope == "group=mod",
            MenuGroup::Lay => scope == "group=lay",
            MenuGroup::Win | MenuGroup::None => false,
        }
    }

    fn open_window(&mut self) {
        if !self.can_open() {
            return;
        }
        let display = self.windows.len();
        self.windows.push(MenuWindow::new(
            display,
            Consts::NEW_WINDOW_LAYOUT,
            Consts::NEW_WINDOW_MODULES,
        ));
        self.rebuild();
    }

    fn rebuild(&mut self) {
        for (index, window) in self.windows.iter_mut().enumerate() {
            let number = index + 1;
            window.title = format!("WINDOW {number} · {}", Consts::LAYOUTS[window.layout]);
            window.caption = format!(
                "{} · {} MOD.",
                Consts::DISPLAYS[window.display],
                window.modules_on()
            );
        }
        let count = self.windows.len();
        self.count_label = if count == 1 {
            "1 WINDOW".to_owned()
        } else {
            format!("{count} WINDOWS")
        };
        let active = self.active + 1;
        self.modules_title = format!("Modules · WINDOW {active}");
        let on = self.windows[self.active].modules_on();
        self.modules_count = format!("{on} OF 11");
    }

    fn toggle_group(&mut self, group: MenuGroup) {
        self.group = if self.group == group {
            MenuGroup::None
        } else {
            group
        };
    }

    fn toggle_module(&mut self, key: &str) -> bool {
        let Some(index) = module_index_of(key) else {
            return false;
        };
        let active = self.active;
        self.windows[active].modules[index] = !self.windows[active].modules[index];
        self.rebuild();
        true
    }

    fn window_row(&mut self, number: &str, node: &str) -> bool {
        let Some(index) = row_index(number) else {
            return false;
        };
        match node {
            "focus" => self.focus(index),
            "display" => self.cycle_display(index),
            "close" => self.close(index),
            _ => return false,
        }
        true
    }
}

pub(super) struct ContextState {
    open: Option<usize>,
    action: String,
    selected: usize,
}

impl Default for ContextState {
    fn default() -> Self {
        Self {
            open: None,
            selected: 0,
            action: "—".to_owned(),
        }
    }
}

impl ContextState {
    pub(super) fn activate(&mut self, path: &str) -> bool {
        let Some((row, action)) = track_address(path) else {
            return false;
        };
        match action {
            "row" => self.selected = row,
            "menu" => {
                if self.open == Some(row) {
                    self.open = None;
                }
            }
            "deck-a" => self.run(row, "DECK A"),
            "deck-b" => self.run(row, "DECK B"),
            "queue" => self.run(row, "TO QUEUE"),
            _ => return false,
        }
        true
    }

    pub(super) fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        if endpoint == "gallery.menu.action" {
            return Some(ReadValue::Text(&self.action));
        }
        let (id, scope) = endpoint.split_once('@').unwrap_or((endpoint, ""));
        let row = scoped_index(scope, "row")?;
        let track = Consts::TRACKS.get(row)?;
        let value = match id {
            "gallery.menu.context" => ReadValue::Bool(self.open == Some(row)),
            "gallery.menu.selected" => ReadValue::Bool(self.selected == row),
            "gallery.menu.track" => ReadValue::Text(track.title),
            "gallery.menu.bpm" => ReadValue::Text(track.meta),
            "gallery.menu.energy" => ReadValue::Scalar(track.energy),
            _ => return None,
        };
        Some(value)
    }

    fn run(&mut self, row: usize, label: &str) {
        let track = row + 1;
        self.action = format!("{label} · {track}");
        self.open = None;
    }

    pub(super) fn secondary(&mut self, path: &str) {
        if let Some((row, "row")) = track_address(path) {
            self.open = Some(row);
        }
    }
}

fn track_address(path: &str) -> Option<(usize, &str)> {
    let rest = path.strip_prefix("ctx/")?.strip_prefix("track-")?;
    let (number, node) = rest.split_once('/')?;
    let row = row_index(number).filter(|row| *row < Consts::TRACKS.len())?;
    Some((row, node))
}

fn row_index(number: &str) -> Option<usize> {
    number.parse::<usize>().ok()?.checked_sub(1)
}

fn scoped_index(scope: &str, name: &str) -> Option<usize> {
    row_index(scope.strip_prefix(name)?.strip_prefix('=')?)
}

fn module_index(scope: &str) -> Option<usize> {
    module_index_of(scope.strip_prefix("module=")?)
}

fn module_index_of(key: &str) -> Option<usize> {
    Consts::MODULES.iter().position(|name| *name == key)
}
