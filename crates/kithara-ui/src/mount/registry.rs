/// Names every built-in control once, and builds its configuration.
///
/// The single place that maps a document variant to the file that owns it, so
/// teaching the toolkit a control is one file and one arm rather than an edit
/// in every table that has ever met one.
///
/// A macro rather than one generic function because each caller brings its own
/// bound: a size wants only the neutral half of the contract, and each host
/// wants its own. The list of controls is still written exactly once, here.
macro_rules! controls {
    ($spec:expr, $mount:expr) => {{
        let with = $mount;
        match $spec {
            $crate::expand::ControlSpec::DeckSummary { style } => {
                with.apply(&$crate::mount::Summary::builder().style(*style).build())
            }
            $crate::expand::ControlSpec::Brand => with.apply(&$crate::mount::Brand),
            $crate::expand::ControlSpec::Spacer => with.apply(&$crate::mount::Spacer),
            $crate::expand::ControlSpec::Divider => with.apply(&$crate::mount::Divider),
            $crate::expand::ControlSpec::PresetSelector => with.apply(&$crate::mount::Preset),
            $crate::expand::ControlSpec::SettingsButton => with.apply(&$crate::mount::Settings),
            $crate::expand::ControlSpec::WindowDrag => with.apply(&$crate::mount::Drag),
            $crate::expand::ControlSpec::TitleBar { label } => {
                with.apply(&$crate::mount::TitleBar::builder().label(*label).build())
            }
            $crate::expand::ControlSpec::WindowControls { style } => {
                with.apply(&$crate::mount::Controls::builder().style(*style).build())
            }
            $crate::expand::ControlSpec::Text {
                style,
                label,
                color,
                active_color,
                active,
                align,
            } => with.apply(
                &$crate::mount::Text::builder()
                    .maybe_active(active.as_ref())
                    .maybe_active_color(*active_color)
                    .align(*align)
                    .maybe_color(*color)
                    .maybe_label(*label)
                    .style(*style)
                    .build(),
            ),
            $crate::expand::ControlSpec::Glyph {
                icon,
                active_icon,
                style,
                color,
                active_color,
                active,
            } => with.apply(
                &$crate::mount::Glyph::builder()
                    .maybe_active(active.as_ref())
                    .maybe_active_color(*active_color)
                    .maybe_active_icon(*active_icon)
                    .maybe_color(*color)
                    .icon(*icon)
                    .style(*style)
                    .build(),
            ),
            $crate::expand::ControlSpec::NavItem { label, icon } => with.apply(
                &$crate::mount::NavItem::builder()
                    .icon(*icon)
                    .label(*label)
                    .build(),
            ),
            $crate::expand::ControlSpec::TabLarge { label } => {
                with.apply(&$crate::mount::Tab::builder().label(*label).build())
            }
            $crate::expand::ControlSpec::Button {
                label,
                icon,
                active_label,
                style,
                frame,
            } => with.apply(
                &$crate::mount::Button::builder()
                    .maybe_active_label(*active_label)
                    .maybe_frame(*frame)
                    .maybe_icon(*icon)
                    .label(*label)
                    .style(*style)
                    .build(),
            ),
            $crate::expand::ControlSpec::Bpm { placeholder } => with.apply(
                &$crate::mount::Bpm::builder()
                    .maybe_placeholder(*placeholder)
                    .build(),
            ),
            $crate::expand::ControlSpec::Time => with.apply(&$crate::mount::Time),
            $crate::expand::ControlSpec::Scalar { format, framed } => with.apply(
                &$crate::mount::Telemetry::builder()
                    .format(*format)
                    .framed(*framed)
                    .build(),
            ),
            $crate::expand::ControlSpec::Crossfader { ticks } => {
                with.apply(&$crate::mount::Crossfader::builder().ticks(*ticks).build())
            }
            $crate::expand::ControlSpec::Fader { style, label } => with.apply(
                &$crate::mount::Fader::builder()
                    .maybe_label(*label)
                    .style(*style)
                    .build(),
            ),
            $crate::expand::ControlSpec::Wave { style, badge, zoom } => with.apply(
                &$crate::mount::Wave::builder()
                    .maybe_badge(*badge)
                    .style(*style)
                    .maybe_zoom(zoom.as_ref())
                    .build(),
            ),
            $crate::expand::ControlSpec::Vis => with.apply(&$crate::mount::Vis),
            $crate::expand::ControlSpec::TrackList {
                columns,
                columns_state,
            } => with.apply(
                &$crate::mount::TrackList::builder()
                    .columns(columns)
                    .maybe_columns_state(columns_state.as_ref())
                    .build(),
            ),
            $crate::expand::ControlSpec::Tree { query } => with.apply(
                &$crate::mount::Tree::builder()
                    .maybe_query(query.as_ref())
                    .build(),
            ),
            $crate::expand::ControlSpec::ContextBar { scope_items, scope } => with.apply(
                &$crate::mount::ContextBar::builder()
                    .maybe_scope(scope.as_ref())
                    .scope_items(scope_items)
                    .build(),
            ),
            $crate::expand::ControlSpec::Toggle => with.apply(&$crate::mount::Toggle),
            $crate::expand::ControlSpec::Checkbox => with.apply(&$crate::mount::Checkbox),
            $crate::expand::ControlSpec::Segmented { items } => {
                with.apply(&$crate::mount::Segmented::builder().items(items).build())
            }
            $crate::expand::ControlSpec::Select { label } => {
                with.apply(&$crate::mount::Select::builder().label(*label).build())
            }
            $crate::expand::ControlSpec::StatusDot { label, tone } => with.apply(
                &$crate::mount::StatusDot::builder()
                    .label(*label)
                    .tone(*tone)
                    .build(),
            ),
            $crate::expand::ControlSpec::Swatch { role, label } => with.apply(
                &$crate::mount::Swatch::builder()
                    .label(*label)
                    .role(*role)
                    .build(),
            ),
            $crate::expand::ControlSpec::Cell { label, highlighted } => with.apply(
                &$crate::mount::Cell::builder()
                    .highlighted(*highlighted)
                    .maybe_label(*label)
                    .build(),
            ),
            $crate::expand::ControlSpec::Readout {
                label,
                tone,
                framed,
            } => with.apply(
                &$crate::mount::Readout::builder()
                    .framed(*framed)
                    .maybe_label(*label)
                    .tone(*tone)
                    .build(),
            ),
            $crate::expand::ControlSpec::Chip { label, style } => with.apply(
                &$crate::mount::Chip::builder()
                    .label(*label)
                    .style(*style)
                    .build(),
            ),
            $crate::expand::ControlSpec::Knob { label } => {
                with.apply(&$crate::mount::Knob::builder().maybe_label(*label).build())
            }
            $crate::expand::ControlSpec::Meter => with.apply(&$crate::mount::Meter),
            $crate::expand::ControlSpec::VuStereo => with.apply(&$crate::mount::VuStereo),
            $crate::expand::ControlSpec::VuVertical { ticks } => {
                with.apply(&$crate::mount::VuVertical::builder().ticks(*ticks).build())
            }
        }
    }};
}

pub(crate) use controls;
