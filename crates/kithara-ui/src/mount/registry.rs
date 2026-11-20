/// Names every built-in control once, and builds its configuration.
///
/// The single place that maps a document variant to the file that owns it, so
/// teaching the toolkit a control is one file and one arm rather than an edit
/// in every table that has ever met one.
///
/// Each arm names the fields the control's size reads, then the fields only a
/// host reads, then how a host mounts it: `painted` for a control that draws
/// itself and nothing more, `apply` for one with its own mount. A control whose
/// host reads more than its size gives a second record for the host to mount;
/// the host pattern names every field, so a new one cannot reach the document
/// without a host deciding what it means.
///
/// A macro rather than one generic function because each caller brings its own
/// bound: `controls!(spec, with)` asks the size half only and compiles without
/// any host, and `controls!(host: spec, with)` asks the host half. The list of
/// controls is still written exactly once, here.
macro_rules! controls {
    (@list $side:ident $args:tt) => {
        $crate::mount::controls!(@$side $args
            DeckSummary {} { style } => apply
                $crate::mount::Summary,
                $crate::mount::deck::summary::host::Summary::builder().style(*style).build();
            Brand {} {} => painted $crate::mount::Brand;
            Spacer {} {} => painted $crate::mount::Spacer;
            Divider {} {} => painted $crate::mount::Divider;
            PresetSelector {} {} => apply $crate::mount::Preset;
            SettingsButton {} {} => painted $crate::mount::Settings;
            WindowDrag {} {} => apply $crate::mount::Drag;
            TitleBar {} { label } => apply
                $crate::mount::TitleBar,
                $crate::mount::window::title_bar::host::TitleBar::builder().label(*label).build();
            WindowControls { style } {} => apply
                $crate::mount::Controls { style: *style };
            Text { style } { label, color, active_color, active, align, font, weight } => apply
                $crate::mount::Text { style: *style },
                $crate::mount::label::text::host::Text::builder()
                    .maybe_active(active.as_ref())
                    .maybe_active_color(*active_color)
                    .align(*align)
                    .maybe_color(color.as_ref())
                    .maybe_font(*font)
                    .maybe_label(*label)
                    .style(*style)
                    .maybe_weight(*weight)
                    .build();
            Glyph { style } { icon, active_icon, color, active_color, active } => apply
                $crate::mount::Glyph { style: *style },
                $crate::mount::label::glyph::host::Glyph::builder()
                    .maybe_active(active.as_ref())
                    .maybe_active_color(*active_color)
                    .maybe_active_icon(*active_icon)
                    .maybe_color(color.as_ref())
                    .icon(*icon)
                    .style(*style)
                    .build();
            NavItem {} { label, icon, style } => painted
                $crate::mount::NavItem,
                $crate::mount::press::nav_item::host::NavItem::builder()
                    .icon(*icon)
                    .label(*label)
                    .maybe_style(*style)
                    .build();
            TabLarge {} { label } => painted
                $crate::mount::Tab,
                $crate::mount::press::tab::host::Tab::builder().label(*label).build();
            Button { style } { label, icon, active_label, frame } => painted
                $crate::mount::Button { style: *style },
                $crate::mount::press::button::host::Button::builder()
                    .maybe_active_label(*active_label)
                    .maybe_frame(*frame)
                    .maybe_icon(*icon)
                    .label(*label)
                    .style(*style)
                    .build();
            Bpm {} { placeholder } => painted
                $crate::mount::Bpm,
                $crate::mount::deck::bpm::host::Bpm::builder()
                    .maybe_placeholder(*placeholder)
                    .build();
            Time {} {} => painted $crate::mount::Time;
            Scalar {} { format, framed } => painted
                $crate::mount::Telemetry,
                $crate::mount::label::telemetry::host::Telemetry::builder()
                    .format(*format)
                    .framed(*framed)
                    .build();
            Crossfader {} { ticks } => painted
                $crate::mount::Crossfader,
                $crate::mount::scalar::crossfader::host::Crossfader::builder().ticks(*ticks).build();
            Fader {} { style, label } => painted
                $crate::mount::Fader,
                $crate::mount::scalar::fader::host::Fader::builder()
                    .maybe_label(*label)
                    .style(*style)
                    .build();
            Wave { style } { badge, zoom } => apply
                $crate::mount::Wave { style: *style },
                $crate::mount::deck::wave::host::Wave::builder()
                    .maybe_badge(*badge)
                    .style(*style)
                    .maybe_zoom(zoom.as_ref())
                    .build();
            Vis {} {} => apply $crate::mount::Vis;
            Shader {} { 0: spec } => apply
                $crate::mount::Shader,
                $crate::mount::panel::shader::host::Shader::new(spec);
            Custom {} { kind } => apply
                $crate::mount::Custom,
                $crate::mount::panel::custom::host::Custom::new(*kind);
            Lottie {} { artwork, active_artwork, active, seconds } => apply
                $crate::mount::Lottie,
                $crate::mount::panel::lottie::host::Lottie::builder()
                    .artwork(*artwork)
                    .maybe_active_artwork(*active_artwork)
                    .maybe_active(active.as_ref())
                    .seconds(*seconds)
                    .build();
            Sprite {} { sheet, seconds } => apply
                $crate::mount::Sprite,
                $crate::mount::panel::sprite::host::Sprite::builder()
                    .seconds(*seconds)
                    .sheet(*sheet)
                    .build();
            PortalMap {} {} => painted $crate::mount::PortalMap;
            Range {} {} => painted $crate::mount::Range;
            Table {} { columns, columns_state, status, frame, width } => apply
                $crate::mount::Table,
                $crate::mount::panel::table::host::Table::builder()
                    .columns(columns)
                    .maybe_columns_state(columns_state.as_ref())
                    .maybe_width(width.as_ref())
                    .frame(*frame)
                    .maybe_status(status.as_ref())
                    .build();
            Search {} {} => apply $crate::mount::Search;
            Tree {} { query, search, toggle } => apply
                $crate::mount::Tree,
                $crate::mount::panel::tree::host::Tree::builder()
                    .maybe_query(query.as_ref())
                    .search(*search)
                    .toggle(*toggle)
                    .build();
            ContextBar {} { scope_items, scope } => apply
                $crate::mount::ContextBar,
                $crate::mount::panel::context_bar::host::ContextBar::builder()
                    .maybe_scope(scope.as_ref())
                    .scope_items(scope_items)
                    .build();
            Toggle {} {} => painted $crate::mount::Toggle;
            Checkbox {} {} => painted $crate::mount::Checkbox;
            Segmented {} { items } => painted
                $crate::mount::Segmented,
                $crate::mount::press::segmented::host::Segmented::builder().items(items).build();
            Select {} { label } => painted
                $crate::mount::Select,
                $crate::mount::label::select::host::Select::builder().label(*label).build();
            StatusDot {} { label, dot_size, tone, active_tone, active } => painted
                $crate::mount::StatusDot,
                $crate::mount::badge::status_dot::host::StatusDot::builder()
                    .maybe_active(active.as_ref())
                    .maybe_active_tone(*active_tone)
                    .maybe_dot_size(*dot_size)
                    .label(*label)
                    .tone(*tone)
                    .build();
            Swatch {} { role, label } => painted
                $crate::mount::Swatch,
                $crate::mount::badge::swatch::host::Swatch::builder()
                    .label(*label)
                    .role(*role)
                    .build();
            Cell {} { label, highlighted } => painted
                $crate::mount::Cell,
                $crate::mount::badge::cell::host::Cell::builder()
                    .highlighted(*highlighted)
                    .maybe_label(*label)
                    .build();
            Readout {} { label, tone, framed } => painted
                $crate::mount::Readout,
                $crate::mount::label::readout::host::Readout::builder()
                    .framed(*framed)
                    .maybe_label(*label)
                    .tone(*tone)
                    .build();
            Chip {} { label, style } => painted
                $crate::mount::Chip,
                $crate::mount::press::chip::host::Chip::builder()
                    .label(*label)
                    .style(*style)
                    .build();
            Knob {} { label } => painted
                $crate::mount::Knob,
                $crate::mount::scalar::knob::host::Knob::builder().maybe_label(*label).build();
            Meter {} {} => painted $crate::mount::Meter;
            VuStereo {} {} => painted $crate::mount::VuStereo;
            VuVertical {} { ticks } => painted
                $crate::mount::VuVertical,
                $crate::mount::scalar::vu_vertical::host::VuVertical::builder().ticks(*ticks).build();
        )
    };
    (@sized ($spec:expr, $with:expr)
        $($variant:ident { $($size:tt $(: $size_at:ident)?),* } { $($host:tt $(: $host_at:ident)?),* }
            => $kind:ident $sized:expr $(, $hosted:expr)?;)*
    ) => {{
        let with = $with;
        match $spec {
            $($crate::expand::ControlSpec::$variant { $($size $(: $size_at)?,)* .. } => {
                with.apply(&$sized)
            })*
        }
    }};
    (@host ($spec:expr, $with:expr)
        $($variant:ident { $($size:tt $(: $size_at:ident)?),* } { $($host:tt $(: $host_at:ident)?),* }
            => $kind:ident $sized:expr $(, $hosted:expr)?;)*
    ) => {{
        let with = $with;
        match $spec {
            $($crate::expand::ControlSpec::$variant {
                $($size $(: $size_at)?,)* $($host $(: $host_at)?),*
            } => with.$kind(&$crate::mount::controls!(@pick $sized $(, $hosted)?)),)*
        }
    }};
    (@pick $sized:expr) => {
        $sized
    };
    (@pick $sized:expr, $hosted:expr) => {
        $hosted
    };
    (host: $spec:expr, $mount:expr) => {
        $crate::mount::controls!(@list host ($spec, $mount))
    };
    ($spec:expr, $mount:expr) => {
        $crate::mount::controls!(@list sized ($spec, $mount))
    };
}

pub(crate) use controls;
