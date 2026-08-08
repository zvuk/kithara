use std::ops::Range;

use num_traits::cast::AsPrimitive;

#[cfg(feature = "masonry-host")]
use super::masonry::TreeMetrics;
use crate::{
    atoms::{
        bar::context::Context,
        wave::zoom_math::{clamp_zoom, window_bounds, zoom_for_wheel},
    },
    compile::CompiledUi,
    draw::Rect,
    engine::{Descriptor, ScrollConfig},
    expand::{Binding, ControlSpec},
    ids::InternId,
    interact::{CursorShape, Hover, ScrollAxis, TextInputLayout, recognizers::WheelStep},
    module::{FaderStyle, WaveStyle},
    render::{
        ReadValue, Reads, Skin,
        document::read::{read_scope, resolve, wave_zoom},
        model::derived,
        picker_selected_index, text_input_layout,
    },
    text::TextContext,
    widgets::track_list::{
        ColumnLayout, column_layouts, column_resizable, minimum_table_width,
        track_list_content_height,
    },
};
#[derive(Clone)]
pub(crate) enum HostedControlPlan {
    Activation {
        path: String,
    },
    Segmented {
        path: String,
        item_count: usize,
    },
    Picker {
        path: String,
        item_count: usize,
        item_height: f32,
        selected: Option<usize>,
        /// Where the strip put its closed face, as an offset from the strip's
        /// own corner. Both hosts hit-test and anchor the menu against the box
        /// the painter drew, rather than measuring the same parts again.
        face: Rect,
    },
    Tree {
        path: String,
        query: String,
        scroll: ScrollConfig,
        search_layout: TextInputLayout,
        search_path: String,
        #[cfg(feature = "masonry-host")]
        metrics: TreeMetrics,
    },
    TrackList(Box<TrackListPlan>),
    Fader {
        path: String,
        style: FaderStyle,
        labelled: bool,
        drag_step: Option<f64>,
        wheel: Option<WheelStep>,
        metrics: crate::skin::FaderSkin,
    },
    Crossfader {
        path: String,
    },
    Knob {
        path: String,
        current: f32,
        drag_range: f32,
        wheel_step: f32,
    },
    StereoMeter {
        path: String,
    },
    VerticalVu {
        path: String,
    },
    Wave {
        path: String,
    },
    HeroWave {
        path: String,
        scale: f32,
        progress: f32,
        visible: Range<f32>,
        wheel_positive: f32,
        wheel_non_positive: f32,
    },
}

#[derive(Clone)]
pub(crate) struct TrackListPlan {
    pub(crate) columns: Vec<ColumnLayout>,
    pub(crate) divider_paths: Vec<String>,
    pub(crate) horizontal_path: String,
    pub(crate) path: String,
    pub(crate) row_count: usize,
    pub(crate) row_target: String,
    content_height: f32,
    min_column_width: f32,
    #[cfg(feature = "masonry-host")]
    pub(super) skin: Skin,
}

impl HostedControlPlan {
    pub(in crate::render) fn resolved(
        path: &str,
        spec: &ControlSpec,
        value: Option<ReadValue<'_>>,
        scope: &str,
        reads: &dyn Reads,
        ui: &CompiledUi,
        skin: &Skin,
    ) -> Option<Self> {
        match (spec, value) {
            (ControlSpec::Button { .. }, _)
            | (
                ControlSpec::Checkbox
                | ControlSpec::Chip { .. }
                | ControlSpec::NavItem { .. }
                | ControlSpec::TabLarge { .. }
                | ControlSpec::Toggle,
                Some(ReadValue::Bool(_)),
            ) => Some(Self::Activation {
                path: path.to_owned(),
            }),
            (ControlSpec::Segmented { items }, Some(ReadValue::Scalar(_))) if !items.is_empty() => {
                Some(Self::Segmented {
                    path: path.to_owned(),
                    item_count: items.len(),
                })
            }
            (ControlSpec::ContextBar { scope_items, scope }, Some(ReadValue::Text(_)))
                if !scope_items.is_empty() =>
            {
                Some(context_bar_plan(
                    path,
                    scope_items,
                    scope.as_ref(),
                    reads,
                    ui,
                    skin,
                ))
            }
            (ControlSpec::Tree { query }, Some(ReadValue::Tree(rows))) => {
                let query = query
                    .as_ref()
                    .and_then(|binding| resolve(reads, binding, ui))
                    .and_then(|value| match value {
                        ReadValue::Text(query) => Some(query.to_owned()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let search_layout = text_input_layout(&query, skin);
                Some(Self::Tree {
                    path: path.to_owned(),
                    query,
                    scroll: ScrollConfig::items(
                        ScrollAxis::Vertical,
                        AsPrimitive::<f32>::as_(rows.len()) * skin.tree.row_height,
                        rows.len(),
                        skin.tree.row_height,
                        skin.tree.row_height,
                        skin.tree.scrollbar_margin + skin.tree.scrollbar_width,
                    ),
                    search_layout,
                    search_path: format!("{path}/search"),
                    #[cfg(feature = "masonry-host")]
                    metrics: TreeMetrics {
                        search_height: skin.tree.search_height,
                        search_icon_width: skin.tree.search_icon_width,
                        panel_padding_top: skin.tree.panel_padding_top,
                        panel_padding_bottom: skin.tree.panel_padding_bottom,
                    },
                })
            }
            (
                ControlSpec::TrackList {
                    columns,
                    columns_state,
                },
                Some(ReadValue::TrackList(rows)),
            ) => {
                let state = columns_state
                    .as_ref()
                    .map(|binding| (ui.resolve(binding.id), read_scope(Some(binding), ui)));
                let columns = column_layouts(columns, reads, state, skin);
                Some(Self::TrackList(Box::new(TrackListPlan::new(
                    path,
                    columns,
                    rows.len(),
                    skin,
                ))))
            }
            (ControlSpec::Fader { style, label }, Some(ReadValue::Scalar(value))) => {
                let (drag_step, wheel) = match style {
                    FaderStyle::Default => (Some(skin.fader.step), None),
                    FaderStyle::Volume => (
                        None,
                        Some(WheelStep {
                            value: value.clamp(0.0, 1.0).as_(),
                            step: skin.fader.step.as_(),
                        }),
                    ),
                };
                Some(Self::Fader {
                    path: path.to_owned(),
                    style: *style,
                    labelled: label.is_some(),
                    drag_step,
                    wheel,
                    metrics: skin.fader,
                })
            }
            (ControlSpec::Crossfader { .. }, Some(ReadValue::Scalar(_))) => {
                Some(Self::Crossfader {
                    path: path.to_owned(),
                })
            }
            (ControlSpec::Knob { .. }, Some(ReadValue::Scalar(value))) => Some(Self::Knob {
                path: path.to_owned(),
                current: value.clamp(0.0, 1.0).as_(),
                drag_range: skin.knob.drag_range,
                wheel_step: skin.knob.wheel_step,
            }),
            (ControlSpec::VuStereo, Some(ReadValue::Stereo(_))) => Some(Self::StereoMeter {
                path: path.to_owned(),
            }),
            (ControlSpec::VuVertical { .. }, Some(ReadValue::Stereo(_))) => {
                Some(Self::VerticalVu {
                    path: path.to_owned(),
                })
            }
            (ControlSpec::Wave { style, zoom, .. }, Some(ReadValue::Waveform(waveform)))
                if !waveform.buckets.is_empty() =>
            {
                Some(wave_plan(path, *style, zoom.as_ref(), scope, reads, ui))
            }
            _ => None,
        }
    }

    pub(crate) fn descriptors(&self) -> Vec<Descriptor> {
        let mut descriptors = Vec::with_capacity(self.descriptor_count());
        self.append_descriptors(&mut descriptors);
        descriptors
    }

    pub(in crate::render) fn path(&self) -> &str {
        match self {
            Self::Activation { path }
            | Self::Segmented { path, .. }
            | Self::Picker { path, .. }
            | Self::Tree { path, .. }
            | Self::Fader { path, .. }
            | Self::Crossfader { path }
            | Self::Knob { path, .. }
            | Self::StereoMeter { path }
            | Self::VerticalVu { path }
            | Self::Wave { path }
            | Self::HeroWave { path, .. } => path,
            Self::TrackList(plan) => &plan.path,
        }
    }

    fn descriptor_count(&self) -> usize {
        if let Self::TrackList(plan) = self {
            return plan.descriptor_count();
        }
        if matches!(self, Self::Tree { .. }) {
            2
        } else {
            1
        }
    }

    fn append_descriptors(&self, descriptors: &mut Vec<Descriptor>) {
        match self {
            Self::Activation { path } => descriptors.push(Descriptor::activation(path.clone())),
            Self::Segmented { path, item_count } => {
                descriptors.push(Descriptor::segmented(path.clone(), *item_count));
            }
            Self::Picker {
                path,
                item_count,
                selected,
                ..
            } => descriptors.push(Descriptor::picker(path.clone(), *item_count, *selected)),
            Self::Tree {
                path,
                query,
                scroll,
                search_layout,
                search_path,
                ..
            } => {
                descriptors.push(Descriptor::text_input(
                    search_path.clone(),
                    query.clone(),
                    search_layout.clone(),
                ));
                descriptors.push(Descriptor::scroll(path.clone(), *scroll));
            }
            Self::TrackList(plan) => plan.append_descriptors(descriptors),
            Self::Fader {
                path,
                style,
                drag_step,
                wheel,
                ..
            } => descriptors.push(Descriptor::fader(
                path.clone(),
                Hover::new(match style {
                    FaderStyle::Default => CursorShape::Grab,
                    FaderStyle::Volume => CursorShape::ResizeH,
                }),
                *drag_step,
                *wheel,
            )),
            Self::Crossfader { path } => {
                descriptors.push(Descriptor::crossfader(path.clone()));
            }
            Self::Knob {
                path,
                current,
                drag_range,
                wheel_step,
            } => descriptors.push(Descriptor::knob(
                path.clone(),
                *current,
                *drag_range,
                *wheel_step,
            )),
            Self::StereoMeter { path } => {
                descriptors.push(Descriptor::stereo_meter(path.clone()));
            }
            Self::VerticalVu { path } => {
                descriptors.push(Descriptor::vertical_vu(path.clone()));
            }
            Self::Wave { path } => descriptors.push(Descriptor::wave(path.clone())),
            Self::HeroWave {
                path,
                scale,
                progress,
                visible,
                wheel_positive,
                wheel_non_positive,
            } => descriptors.push(Descriptor::hero_wave(
                path.clone(),
                *scale,
                *progress,
                visible.clone(),
                *wheel_positive,
                *wheel_non_positive,
            )),
        }
    }
}

fn context_bar_plan(
    path: &str,
    scope_items: &[InternId],
    scope: Option<&Binding>,
    reads: &dyn Reads,
    ui: &CompiledUi,
    skin: &Skin,
) -> HostedControlPlan {
    let scope_value = scope.and_then(|binding| resolve(reads, binding, ui));
    let selected = picker_selected_index(scope_value.as_ref(), scope_items.len());
    let mut text = TextContext::from(skin.text_resources());
    HostedControlPlan::Picker {
        path: path.to_owned(),
        item_count: scope_items.len(),
        item_height: skin.tree.scope_item_height,
        selected,
        face: Context::new(skin)
            .face_of(&mut text, scope_items.iter().map(|item| ui.resolve(*item))),
    }
}

fn wave_plan(
    path: &str,
    style: WaveStyle,
    zoom: Option<&Binding>,
    scope: &str,
    reads: &dyn Reads,
    ui: &CompiledUi,
) -> HostedControlPlan {
    if style != WaveStyle::Hero {
        return HostedControlPlan::Wave {
            path: path.to_owned(),
        };
    }
    let progress = match reads.get(&derived("deck.playback.position_normalized", scope)) {
        Some(ReadValue::Scalar(value)) => value.as_(),
        _ => 0.0,
    };
    let scale = clamp_zoom(wave_zoom(zoom, reads, ui));
    HostedControlPlan::HeroWave {
        path: path.to_owned(),
        scale,
        progress,
        visible: window_bounds(progress, scale),
        wheel_positive: zoom_for_wheel(scale, 1.0),
        wheel_non_positive: zoom_for_wheel(scale, 0.0),
    }
}

impl TrackListPlan {
    pub(super) fn new(
        path: &str,
        columns: Vec<ColumnLayout>,
        row_count: usize,
        skin: &Skin,
    ) -> Self {
        let divider_paths = columns
            .iter()
            .enumerate()
            .filter(|(index, _)| column_resizable(&columns, *index))
            .map(|(_, column)| format!("{path}/width/{}", column.column.endpoint_name()))
            .collect();
        Self {
            content_height: track_list_content_height(row_count, skin),
            columns,
            divider_paths,
            horizontal_path: format!("{path}/scroll-x"),
            path: path.to_owned(),
            row_count,
            row_target: format!("{path}/rows"),
            min_column_width: skin.track_list.min_column_width,
            #[cfg(feature = "masonry-host")]
            skin: skin.clone(),
        }
    }

    fn descriptor_count(&self) -> usize {
        self.divider_paths.len() + 3
    }

    fn append_descriptors(&self, descriptors: &mut Vec<Descriptor>) {
        descriptors.push(Descriptor::scroll(
            self.horizontal_path.clone(),
            ScrollConfig::plain(ScrollAxis::Horizontal, minimum_table_width(&self.columns)),
        ));
        descriptors.push(Descriptor::scroll(
            self.path.clone(),
            ScrollConfig::plain(ScrollAxis::Vertical, self.content_height),
        ));
        descriptors.push(Descriptor::item(
            self.row_target.clone(),
            self.path.clone(),
            self.row_count,
        ));
        let resizable = self
            .columns
            .iter()
            .enumerate()
            .filter(|(index, _)| column_resizable(&self.columns, *index));
        for (divider_path, (_, column)) in self.divider_paths.iter().zip(resizable) {
            descriptors.push(Descriptor::column_divider(
                divider_path.clone(),
                column.width,
                self.min_column_width,
            ));
        }
    }
}
