use kithara::ui::render::{Node, PortalMapView, PortalTarget, ReadValue, ScalarRange, Scope};
use num_traits::cast::AsPrimitive;

use super::value::Value;
use crate::gui::ui::cache::StageView;

/// One deck as the tempo map sees it.
#[derive(Clone, Copy)]
pub(super) struct DeckTempo {
    pub(super) bpm: Option<f32>,
    pub(super) focused: bool,
    pub(super) position: f64,
}

/// The tempo axis: the decks' analysed BPMs against the processed Host tempo.
/// Moving a window edge really changes the axis the map is drawn on, so the
/// range beside it is not a decoration.
pub(super) struct TempoNode<'a> {
    view: &'a StageView,
    targets: Vec<PortalTarget>,
    master: Option<f32>,
}

impl<'a> TempoNode<'a> {
    pub(super) fn new(view: &'a StageView, decks: &[DeckTempo]) -> Self {
        let master = view.host_bpm;
        let targets = decks
            .iter()
            .filter_map(|deck| {
                Some(PortalTarget {
                    bpm: deck.bpm?,
                    is_selected: deck.focused,
                })
            })
            .collect();
        Self {
            view,
            targets,
            master,
        }
    }
}

impl<'a, 'b: 'a> Node<'a> for &'a TempoNode<'b> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let (min, max) = self.view.bpm_window();
        let value = match segment {
            "map" => ReadValue::PortalMap(PortalMapView {
                min,
                max,
                master: self.master?,
                targets: &self.targets,
            }),
            "window" => ReadValue::Range(ScalarRange {
                min: self.view.window.0,
                max: self.view.window.1,
            }),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

/// What the visualiser reads besides the master level it takes from the player:
/// the preset the host holds, and a clock. The clock is the focused deck's
/// playhead, so the surface moves with playback rather than with wall time.
#[derive(Clone, Copy)]
pub(super) struct VisNode<'a> {
    view: &'a StageView,
    clock: f64,
}

impl<'a> VisNode<'a> {
    pub(super) fn new(view: &'a StageView, decks: &[DeckTempo]) -> Self {
        Self {
            view,
            clock: focused(decks).map_or(0.0, |deck| deck.position),
        }
    }
}

impl<'a> Node<'a> for VisNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "preset" => ReadValue::Scalar(self.view.preset.as_()),
            "time" => ReadValue::Scalar(self.clock),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

fn focused(decks: &[DeckTempo]) -> Option<&DeckTempo> {
    decks.iter().find(|deck| deck.focused)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn tempo_map_uses_only_processed_host_tempo() {
        let decks = [DeckTempo {
            bpm: Some(139.0),
            focused: true,
            position: 0.0,
        }];
        let mut view = StageView::default();
        let unavailable = TempoNode::new(&view, &decks);
        assert!((&unavailable).child("map", Scope::default()).is_none());

        view.host_bpm = Some(124.0);
        let available = TempoNode::new(&view, &decks);
        let value = (&available)
            .child("map", Scope::default())
            .and_then(|node| node.read());
        let Some(ReadValue::PortalMap(map)) = value else {
            panic!("processed Host tempo must expose the map");
        };
        assert_eq!(map.master, 124.0);
        assert_eq!(map.targets[0].bpm, 139.0);
    }
}
