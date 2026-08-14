use kithara_ui::render::{PortalMapView, PortalTarget, ReadValue, ScalarRange, TrackRow};

use crate::mock_data::CATALOG;

struct PivotConsts;

impl PivotConsts {
    const RANGE_LOW: f32 = 60.0;
    const RANGE_HIGH: f32 = 200.0;
    const RANGE_GAP: f32 = 8.0;
    const STEP: [(u16, u16); 9] = [
        (3, 4),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 8),
        (8, 9),
        (9, 10),
        (10, 11),
        (11, 12),
    ];
    const LEAP: [(u16, u16); 9] = [
        (5, 7),
        (7, 9),
        (8, 11),
        (9, 11),
        (10, 13),
        (11, 13),
        (11, 14),
        (11, 15),
        (11, 16),
    ];
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum Family {
    #[default]
    Step,
    Leap,
}

struct Portal {
    loop_master: u16,
    loop_target: u16,
    bpm: f32,
    pulse: f32,
    duration: f32,
    ratio: String,
    bpm_label: String,
    pulse_label: String,
    loop_label: String,
    stretch_label: String,
}

#[derive(Default)]
struct SelectedView {
    ratio: String,
    master: String,
    target: String,
    pulse: String,
    duration: String,
    loop_a: String,
    loop_b: String,
    hint: String,
    none: String,
}

pub(super) struct PivotState {
    portals: Vec<Portal>,
    targets: Vec<PortalTarget>,
    selected: Option<usize>,
    selected_view: SelectedView,
    family: Family,
    min: f32,
    max: f32,
    master: f32,
    multiplier: usize,
    min_label: String,
    max_label: String,
    master_label: String,
}

impl Default for PivotState {
    fn default() -> Self {
        let mut state = Self {
            portals: Vec::new(),
            targets: Vec::new(),
            selected: None,
            selected_view: SelectedView::default(),
            family: Family::Step,
            min: 92.0,
            max: 176.0,
            master: 124.0,
            multiplier: 0,
            min_label: String::new(),
            max_label: String::new(),
            master_label: String::new(),
        };
        state.rebuild(Selection::SmallestRatio);
        state
    }
}

impl PivotState {
    pub(super) fn activate(&mut self, path: &str) -> bool {
        if !path.contains("pivot") {
            return false;
        }
        if let Some(index) = path
            .split('/')
            .find_map(|part| part.strip_prefix("row-"))
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|index| *index < self.portals.len())
        {
            self.select(index);
            return true;
        }
        match path.rsplit('/').next() {
            Some("step") => {
                self.family = Family::Step;
                self.rebuild(Selection::Index(0));
            }
            Some("leap") => {
                self.family = Family::Leap;
                self.rebuild(Selection::Index(0));
            }
            Some("mul-1") => self.set_multiplier(0),
            Some("mul-2") => self.set_multiplier(1),
            Some("mul-4") => self.set_multiplier(2),
            _ => return false,
        }
        true
    }

    pub(super) fn set_scalar(&mut self, path: &str, value: f64) -> bool {
        if !path.contains("pivot") || !value.is_finite() {
            return false;
        }
        let bpm = PivotConsts::RANGE_LOW
            + value.clamp(0.0, 1.0) as f32 * (PivotConsts::RANGE_HIGH - PivotConsts::RANGE_LOW);
        let bpm = (bpm / 2.0).round() * 2.0;
        match path.rsplit('/').next() {
            Some("min") => self.min = bpm.min(self.max - PivotConsts::RANGE_GAP),
            Some("max") => self.max = bpm.max(self.min + PivotConsts::RANGE_GAP),
            _ => return false,
        }
        self.rebuild(Selection::Current);
        true
    }

    pub(super) fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let (id, scope) = endpoint.split_once('@').unwrap_or((endpoint, ""));
        let portal_index = scope_index(scope, "portal");
        let portal = portal_index.and_then(|index| self.portals.get(index));
        let track_index = scope_index(scope, "track");
        let track = track_index.and_then(|index| self.track(index));
        let value = match id {
            "pivot.map" => ReadValue::PortalMap(PortalMapView {
                master: self.master,
                min: self.min - 4.0,
                max: self.max + 4.0,
                targets: &self.targets,
            }),
            "pivot.master.label" => ReadValue::Text(&self.master_label),
            "pivot.range" => ReadValue::Range(ScalarRange {
                min: (self.min - PivotConsts::RANGE_LOW)
                    / (PivotConsts::RANGE_HIGH - PivotConsts::RANGE_LOW),
                max: (self.max - PivotConsts::RANGE_LOW)
                    / (PivotConsts::RANGE_HIGH - PivotConsts::RANGE_LOW),
            }),
            "pivot.range.min_label" => ReadValue::Text(&self.min_label),
            "pivot.range.max_label" => ReadValue::Text(&self.max_label),
            "pivot.family.step" => ReadValue::Bool(self.family == Family::Step),
            "pivot.family.leap" => ReadValue::Bool(self.family == Family::Leap),
            "pivot.multiplier.1" => ReadValue::Bool(self.multiplier == 0),
            "pivot.multiplier.2" => ReadValue::Bool(self.multiplier == 1),
            "pivot.multiplier.4" => ReadValue::Bool(self.multiplier == 2),
            "pivot.selected.ratio" => ReadValue::Text(&self.selected_view.ratio),
            "pivot.selected.master" => ReadValue::Text(&self.selected_view.master),
            "pivot.selected.target" => ReadValue::Text(&self.selected_view.target),
            "pivot.selected.pulse" => ReadValue::Text(&self.selected_view.pulse),
            "pivot.selected.duration" => ReadValue::Text(&self.selected_view.duration),
            "pivot.selected.loop_a" => ReadValue::Text(&self.selected_view.loop_a),
            "pivot.selected.loop_b" => ReadValue::Text(&self.selected_view.loop_b),
            "pivot.footer.hint" => ReadValue::Text(&self.selected_view.hint),
            "pivot.no_tracks.hidden" => ReadValue::Bool(self.track(0).is_some()),
            "pivot.no_tracks.label" => ReadValue::Text(&self.selected_view.none),
            "pivot.portal.hidden" => ReadValue::Bool(portal.is_none()),
            "pivot.portal.active" => {
                ReadValue::Bool(portal_index.is_some_and(|index| self.selected == Some(index)))
            }
            "pivot.portal.ratio" => ReadValue::Text(&portal?.ratio),
            "pivot.portal.bpm" => ReadValue::Text(&portal?.bpm_label),
            "pivot.portal.pulse" => ReadValue::Text(&portal?.pulse_label),
            "pivot.portal.loop" => ReadValue::Text(&portal?.loop_label),
            "pivot.portal.stretch" => ReadValue::Text(&portal?.stretch_label),
            "pivot.track.hidden" => ReadValue::Bool(track.is_none()),
            "pivot.track.title" => ReadValue::Text(track?.title),
            "pivot.track.artist" => ReadValue::Text(track?.artist.unwrap_or("—")),
            "pivot.track.bpm" => ReadValue::Text(track?.bpm.unwrap_or("—")),
            "pivot.track.key" => ReadValue::Text(track?.key.unwrap_or("—")),
            _ => return None,
        };
        Some(value)
    }

    fn rebuild(&mut self, selection: Selection) {
        let pairs = match self.family {
            Family::Step => &PivotConsts::STEP,
            Family::Leap => &PivotConsts::LEAP,
        };
        self.portals = pairs
            .iter()
            .flat_map(|&(p, q)| [(q, p), (p, q)])
            .filter_map(|(n, d)| self.portal(n, d))
            .collect();
        self.portals.sort_by(|left, right| {
            distance(left.bpm, self.master).total_cmp(&distance(right.bpm, self.master))
        });
        self.selected = match selection {
            Selection::SmallestRatio => self
                .portals
                .iter()
                .enumerate()
                .min_by_key(|(_, portal)| portal.loop_master + portal.loop_target)
                .map(|(index, _)| index),
            Selection::Current => self
                .selected
                .map(|index| index.min(self.portals.len().saturating_sub(1)))
                .filter(|_| !self.portals.is_empty()),
            Selection::Index(index) => (!self.portals.is_empty())
                .then_some(index.min(self.portals.len().saturating_sub(1))),
        };
        self.targets = self
            .portals
            .iter()
            .enumerate()
            .map(|(index, portal)| PortalTarget {
                bpm: portal.bpm,
                is_selected: self.selected == Some(index),
            })
            .collect();
        self.min_label = format!("{:.0}", self.min);
        self.max_label = format!("{:.0}", self.max);
        self.master_label = format!("A · {:.2}", self.master);
        self.rebuild_selected();
    }

    fn portal(&self, numerator: u16, denominator: u16) -> Option<Portal> {
        let bpm =
            (self.master * f32::from(numerator) / f32::from(denominator) * 100.0).round() / 100.0;
        if bpm < self.min || bpm > self.max || distance(bpm, self.master) < 0.05 {
            return None;
        }
        let mut pulse = self.master / f32::from(denominator);
        while pulse < 40.0 {
            pulse *= 2.0;
        }
        while pulse > 170.0 {
            pulse /= 2.0;
        }
        let duration = f32::from(denominator) * 4.0 * 60.0 / self.master;
        let stretch = (bpm / self.master - 1.0) * 100.0;
        Some(Portal {
            loop_master: denominator,
            loop_target: numerator,
            bpm,
            pulse,
            duration,
            ratio: format!("{denominator}:{numerator}"),
            bpm_label: format!("{bpm:.2}"),
            pulse_label: format!("{pulse:.2}"),
            loop_label: format!("{denominator} / {numerator}"),
            stretch_label: format!(
                "{}{:.2}%",
                if stretch >= 0.0 { "+" } else { "−" },
                stretch.abs()
            ),
        })
    }

    fn select(&mut self, index: usize) {
        self.selected = Some(index);
        for (target_index, target) in self.targets.iter_mut().enumerate() {
            target.is_selected = target_index == index;
        }
        self.rebuild_selected();
    }

    fn set_multiplier(&mut self, multiplier: usize) {
        self.multiplier = multiplier;
        self.rebuild_selected();
    }

    fn rebuild_selected(&mut self) {
        let copy = CATALOG.pivot;
        let Some(portal) = self.selected.and_then(|index| self.portals.get(index)) else {
            self.selected_view = SelectedView {
                ratio: "—".to_owned(),
                master: format!("{:.2}", self.master),
                target: "—".to_owned(),
                pulse: copy.pulse_empty.clone(),
                duration: "—".to_owned(),
                loop_a: "—".to_owned(),
                loop_b: "—".to_owned(),
                hint: copy.hint_empty.clone(),
                none: copy.tracks_empty.clone(),
            };
            return;
        };
        let multiplier = [1, 2, 4][self.multiplier];
        let loop_a = (portal.loop_master * multiplier).to_string();
        let loop_b = (portal.loop_target * multiplier).to_string();
        let pulse = format!("{:.2}", portal.pulse);
        self.selected_view = SelectedView {
            ratio: portal.ratio.clone(),
            master: format!("{:.2}", self.master),
            target: portal.bpm_label.clone(),
            pulse: fill(&copy.pulse, &[("pulse", &pulse)]),
            duration: fill(
                &copy.duration,
                &[("duration", &format!("{:.1}", portal.duration))],
            ),
            hint: fill(
                &copy.hint,
                &[("pulse", &pulse), ("loop_a", &loop_a), ("loop_b", &loop_b)],
            ),
            none: fill(&copy.tracks, &[("bpm", &format!("{:.2}", portal.bpm))]),
            loop_a,
            loop_b,
        };
    }

    fn track(&self, index: usize) -> Option<&'static TrackRow<'static>> {
        let bpm = self
            .selected
            .and_then(|selected| self.portals.get(selected))?
            .bpm;
        CATALOG
            .rows
            .iter()
            .filter(|track| {
                track
                    .bpm
                    .and_then(|value| value.parse::<f32>().ok())
                    .is_some_and(|track_bpm| distance(track_bpm / bpm, 1.0) < 0.02)
            })
            .nth(index)
    }
}

#[derive(Clone, Copy)]
enum Selection {
    SmallestRatio,
    Current,
    Index(usize),
}

fn distance(left: f32, right: f32) -> f32 {
    (left - right).abs()
}

fn fill(template: &str, slots: &[(&str, &str)]) -> String {
    slots
        .iter()
        .fold(template.to_owned(), |text, (name, value)| {
            text.replace(&format!("{{{name}}}"), value)
        })
}

fn scope_index(scope: &str, name: &str) -> Option<usize> {
    scope.split(',').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.parse::<usize>().ok()).flatten()
    })
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn default_step_portal_matches_the_handoff() {
        let state = PivotState::default();
        let selected = state
            .selected
            .and_then(|index| state.portals.get(index))
            .unwrap();

        assert_eq!((selected.loop_master, selected.loop_target), (4, 3));
        assert_eq!(selected.bpm, 93.0);
        assert_eq!(selected.pulse, 62.0);
    }

    #[kithara::test]
    fn range_keeps_an_eight_bpm_gap_and_two_bpm_steps() {
        let mut state = PivotState::default();

        assert!(state.set_scalar("pivot/range/min", 1.0));
        assert_eq!(state.min, 168.0);
        assert!(state.set_scalar("pivot/range/max", 0.0));
        assert_eq!(state.max, 176.0);
    }
}
