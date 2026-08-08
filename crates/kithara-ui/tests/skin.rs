use kithara_test_utils::kithara;
use kithara_ui::{
    builtin,
    envelope::{DocKind, probe},
    error::UiDocError,
    ids::{DocId, SourceUri},
    size::{Dim, SizeSpec},
    skin::{ColorRole, FontFamily, FontWeight, TextRoleSkin, parse_skin},
};

const TOKENS: [(&str, &str, ColorRole); 22] = [
    ("bg", "#12121f", ColorRole::Bg),
    ("deep", "#0b0b16", ColorRole::BgDeep),
    ("inset", "#15152a", ColorRole::BgInset),
    ("panel", "#20203a", ColorRole::BgPanel),
    ("panel2", "#1b1b32", ColorRole::BgFooter),
    ("select", "#26264a", ColorRole::BgSelect),
    ("line", "#3b3b67", ColorRole::Line),
    ("lineDim", "#2a2a4c", ColorRole::LineInner),
    ("lineHi", "#4a4a7a", ColorRole::LineHi),
    ("linePop", "#2f2f57", ColorRole::LinePop),
    ("text", "#e6e6e6", ColorRole::Text),
    ("dim", "#a7aac2", ColorRole::TextDim),
    ("muted", "#6f7189", ColorRole::Muted),
    ("gold", "#bb9442", ColorRole::Accent),
    ("goldHi", "#d6ad59", ColorRole::AccentStrong),
    ("ok", "#66cc66", ColorRole::Success),
    ("warn", "#e6b333", ColorRole::Warning),
    ("alert", "#e64d4d", ColorRole::Danger),
    ("waveLow", "#eb298c", ColorRole::WaveLow),
    ("waveMid", "#f2d129", ColorRole::WaveMid),
    ("waveHigh", "#2ec7eb", ColorRole::WaveHigh),
    ("shadow", "#000000", ColorRole::Shadow),
];

type Role = (FontFamily, FontWeight, f32, f32, ColorRole);

fn origin() -> SourceUri {
    SourceUri("kithara-dark.kskin.ron".to_owned())
}

const fn role(skin: TextRoleSkin) -> Role {
    (skin.font, skin.weight, skin.size, skin.spacing, skin.color)
}

const fn mono(size: f32, spacing: f32, color: ColorRole) -> Role {
    (FontFamily::Mono, FontWeight::Normal, size, spacing, color)
}

#[kithara::test]
fn palette_carries_every_design_token() {
    let document = parse_skin(builtin::DARK_SKIN, &origin()).unwrap();
    let palette = &document.palette;
    let values = [
        palette.bg.as_str(),
        palette.bg_deep.as_str(),
        palette.bg_inset.as_str(),
        palette.bg_panel.as_str(),
        palette.bg_footer.as_str(),
        palette.bg_select.as_str(),
        palette.line.as_str(),
        palette.line_inner.as_str(),
        palette.line_hi.as_str(),
        palette.line_pop.as_str(),
        palette.text.as_str(),
        palette.text_dim.as_str(),
        palette.muted.as_str(),
        palette.accent.as_str(),
        palette.accent_strong.as_str(),
        palette.success.as_str(),
        palette.warning.as_str(),
        palette.danger.as_str(),
        palette.wave_low.as_str(),
        palette.wave_mid.as_str(),
        palette.wave_high.as_str(),
        palette.shadow.as_str(),
    ];

    assert_eq!(values.len(), TOKENS.len());
    for ((token, hex, alias), value) in TOKENS.into_iter().zip(values) {
        assert_eq!(value, hex, "design token {token} / {alias:?}");
    }
}

#[kithara::test]
fn menu_skin_pins_the_design_canon() {
    let menu = parse_skin(builtin::DARK_SKIN, &origin()).unwrap().menu;

    assert_eq!(menu.icon_size, 11.0);
    assert_eq!(menu.burger_icon_size, 13.0);
    assert_eq!(menu.small_icon_size, 10.0);
    assert_eq!(menu.cell_icon_size, 9.0);
}

#[kithara::test]
fn the_mono_text_roles_pin_the_design_canon() {
    let text = parse_skin(builtin::DARK_SKIN, &origin()).unwrap().text;

    assert_eq!(role(text.mono), mono(10.0, 0.0, ColorRole::TextDim));
    assert_eq!(role(text.micro_label), mono(8.0, 0.12, ColorRole::Muted));
    assert_eq!(role(text.caption), mono(7.0, 0.08, ColorRole::Muted));
}

#[kithara::test]
fn pop_skin_pins_the_design_canon() {
    let pop = parse_skin(builtin::DARK_SKIN, &origin()).unwrap().pop;

    assert_eq!(pop.background, ColorRole::BgFooter);
    assert_eq!(pop.frame.radius, 0.0);
    assert_eq!(pop.frame.border_width, 1.0);
    assert_eq!(pop.frame.border, ColorRole::LineHi);
    assert_eq!(pop.cap_height, 2.0);
    assert_eq!(pop.cap_color, ColorRole::Accent);
    assert_eq!(pop.shadow.color, ColorRole::Shadow);
    assert_eq!(pop.shadow.alpha, 0.6);
    assert_eq!(pop.shadow.offset_x, 0.0);
    assert_eq!(pop.shadow.offset_y, 16.0);
    assert_eq!(pop.shadow.blur, 40.0);
}

#[kithara::test]
fn builtin_skin_parses_every_required_section() {
    let document = parse_skin(builtin::DARK_SKIN, &origin()).unwrap();

    assert_eq!(document.id, DocId("kithara-dark".to_owned()));
    assert_eq!(builtin::skin_doc(), &document);
}

#[kithara::test]
fn builtin_skin_pins_the_design_canon() {
    let document = parse_skin(builtin::DARK_SKIN, &origin()).unwrap();

    assert_eq!(document.palette.bg, "#12121f");
    assert_eq!(document.layout.grid_gap, 1.0);
    assert_eq!(document.layout.grid_pad, 0.0);
    assert_eq!(document.chrome.frame.border, ColorRole::Line);
    assert_eq!(document.palette.line_inner, "#2a2a4c");
    assert_eq!(document.palette.bg_footer, "#1b1b32");
    assert_eq!(document.chrome.header_height, 26.0);
    assert_eq!(document.chrome.chip_pad, 9.0);
    assert_eq!(document.chrome.chip_text_size, 9.0);
    assert_eq!(document.chrome.title_text_size, 11.0);
    assert_eq!(document.chrome.chevron_size, 26.0);
    assert_eq!(document.chrome.footer_height, 22.0);
    assert_eq!(document.chrome.footer_text_size, 9.0);
    assert_eq!(document.chrome.inner_line, ColorRole::LineInner);
    assert_eq!(document.chrome.footer_background, ColorRole::BgFooter);
    assert_eq!(document.chrome.corner_size, 10.0);
    assert_eq!(document.chrome.corner_width, 2.0);
    assert_eq!(document.chrome.corner_offset, 0.0);
    assert_eq!(document.chrome.corner_color, ColorRole::Accent);
    assert_eq!(document.text_input.idle_border_width, 0.0);
    assert_eq!(document.window.resize_edge, 4.0);
    assert_eq!(document.knob.body_fill, ColorRole::BgSelect);
    assert_eq!(document.knob.body_border, ColorRole::Line);
    assert_eq!(document.knob.track_color, ColorRole::Line);
    assert_eq!(document.knob.value_color, ColorRole::Accent);
    assert_eq!(document.knob.indicator_color, ColorRole::Text);
    assert_eq!(document.knob.track_alpha, 1.0);
    assert_eq!(document.knob.drag_range, 140.0);
    assert_eq!(document.knob.wheel_step, 0.02);
    assert_eq!(document.crossfader.rail_height, 6.0);
    assert_eq!(document.crossfader.padding_x, 14.0);
    assert_eq!(document.crossfader.thumb_width, 10.0);
    assert_eq!(document.crossfader.thumb_height, 16.0);
    assert_eq!(document.crossfader.left_label, "A");
    assert_eq!(document.crossfader.center_label, "XFADE");
    assert_eq!(document.crossfader.right_label, "B");
    let horizontal_ticks = document.crossfader.ticks;
    assert_eq!(horizontal_ticks.count, 11);
    assert_eq!(horizontal_ticks.thickness, 1.0);
    assert_eq!(horizontal_ticks.length, 6.0);
    assert_eq!(horizontal_ticks.center_length, 10.0);
    assert_eq!(horizontal_ticks.gap, 5.0);
    assert_eq!(horizontal_ticks.inset, 2.0);
    assert_eq!(horizontal_ticks.color, ColorRole::Line);
    assert_eq!(horizontal_ticks.center_color, ColorRole::TextDim);
    assert_eq!(
        document.meter.size,
        SizeSpec::new(Dim::Fixed(40.0), Dim::Fixed(7.0))
    );
    assert_eq!(document.meter.frame.border_width, 1.0);
    assert_eq!(document.meter.frame.border, ColorRole::LineInner);
    assert_eq!(document.meter.background, ColorRole::BgInset);
    assert_eq!(document.meter.fill, ColorRole::Accent);
    assert_eq!(document.vu_stereo.segment_count, 16);
    assert_eq!(document.vu_vertical.fader_width, 18.0);
    let vertical_ticks = document.vu_vertical.ticks;
    assert_eq!(vertical_ticks.count, 11);
    assert_eq!(vertical_ticks.thickness, 1.0);
    assert_eq!(vertical_ticks.length, 8.0);
    assert_eq!(vertical_ticks.center_length, 14.0);
    assert_eq!(vertical_ticks.gap, 6.0);
    assert_eq!(vertical_ticks.inset, 2.0);
    assert_eq!(vertical_ticks.color, ColorRole::Line);
    assert_eq!(vertical_ticks.center_color, ColorRole::TextDim);
    assert_eq!(document.vu_vertical.warning_threshold, 0.66);
    assert_eq!(
        document.toggle.size,
        SizeSpec::new(Dim::Fixed(26.0), Dim::Fixed(14.0))
    );
    assert_eq!(document.toggle.active_frame.radius, 0.0);
    assert_eq!(
        document.checkbox.size,
        SizeSpec::new(Dim::Fixed(10.0), Dim::Fixed(10.0))
    );
    assert_eq!(document.checkbox.inactive_frame.border_width, 1.0);
    assert_eq!(document.readout.label.weight, FontWeight::Normal);
    assert_eq!(document.chip.inactive_frame.border, ColorRole::Line);
    assert_eq!(document.chip.deck_text.size, 9.0);
    assert_eq!(document.chip.routing_text.size, 7.0);
    assert_eq!(document.button.primary_text.weight, FontWeight::Normal);
    assert_eq!(document.button.transport_icon_size, 10.0);
    assert_eq!(document.nav.item_height, 30.0);
    assert_eq!(document.nav.marker_width, 2.0);
    assert_eq!(document.nav.icon_size, 14.0);
    assert_eq!(document.nav.text_pad_x, 14.0);
    assert_eq!(document.tab_large.height, 28.0);
    assert_eq!(document.tab_large.pad_y, 0.0);
    assert_eq!(document.tab_large.underline_width, 2.0);
    assert_eq!(document.text.brand.font, FontFamily::Display);
    assert_eq!(document.text.brand.weight, FontWeight::Bold);
    assert_eq!(document.text.brand.spacing, 0.3);
    assert_eq!(document.text.brand_small.font, FontFamily::Display);
    assert_eq!(document.text.brand_small.weight, FontWeight::Bold);
    assert_eq!(document.text.brand_small.size, 10.0);
    assert_eq!(document.text.brand_small.spacing, 0.3);
    assert_eq!(document.text.brand_small.color, ColorRole::Text);
    assert_eq!(document.text.deck_letter.size, 12.0);
    assert_eq!(document.text.track_title.weight, FontWeight::Medium);
    assert_eq!(document.text.track_title.size, 12.0);
    assert_eq!(document.text.body.size, 11.0);
    assert_eq!(document.text.telemetry.font, FontFamily::Mono);
    assert_eq!(document.text.telemetry.color, ColorRole::Accent);
    assert_eq!(document.text.micro_label.size, 8.0);
    assert_eq!(document.text.micro_label.spacing, 0.12);
    assert_eq!(document.text.micro_label.color, ColorRole::Muted);
    assert_eq!(document.segmented.frame.border_width, 1.0);
    assert_eq!(document.segmented.active_background, ColorRole::Accent);
    assert_eq!(document.select.background, ColorRole::BgInset);
    assert_eq!(document.status_dot.dot_size, 6.0);
    assert_eq!(document.swatch.box_height, 44.0);
    assert_eq!(document.swatch.frame.border, ColorRole::Line);
    assert_eq!(document.swatch.label.font, FontFamily::Mono);
    assert_eq!(document.swatch.hex.color, ColorRole::TextDim);
    assert_eq!(document.cell.frame.radius, 0.0);
    assert_eq!(document.fader.handle_frame.radius, 0.0);
    assert_eq!(document.fader.handle_width, 9);
    assert_eq!(document.fader.handle_color, ColorRole::Accent);
    assert_eq!(document.fader.icon_width, 18.0);
    assert_eq!(document.fader.label_width, 28.0);
    assert_eq!(document.vu_vertical.thumb_height, 9.0);
    assert_eq!(document.vu_vertical.thumb_color, ColorRole::Accent);
    assert_eq!(document.vu_vertical.thumb_notch_offset, 4.0);
    assert_eq!(document.vu_vertical.thumb_notch_height, 1.0);
    assert_eq!(document.vis.header_height, 26.0);
    assert_eq!(document.vis.size.h, Dim::Fixed(300.0));
    assert_eq!(document.vis.footer_height, 22.0);
    assert_eq!(document.wave.cue_badge_background, ColorRole::WaveHigh);
    assert_eq!(document.wave.downbeat_alpha, 0.6);
    assert_eq!(document.wave.grid_alpha, 0.4);
    assert_eq!(document.wave.loop_fill_alpha, 0.22);
    assert_eq!(document.wave.overlay.background, ColorRole::BgDeep);
    assert_eq!(document.wave.overlay.background_alpha, 0.84);
    assert_eq!(document.wave.overlay.key_color, ColorRole::Accent);
    assert_eq!(document.wave.played_alpha, 0.35);
    assert_eq!(document.wave.playhead_width, 1.5);
    assert_eq!(document.global_bar.brand_width, 112.0);
    assert_eq!(document.telemetry.percent_scale, 100.0);
    assert_eq!(document.telemetry.percent_width, 3);
    assert_eq!(document.telemetry.scalar_precision, 2);
    assert_eq!(document.tree.size.w, Dim::Fixed(232.0));
    assert_eq!(document.tree.row_height, 24.0);
    assert_eq!(document.tree.context_height, 26.0);
    let track_list = &document.track_list;
    assert_eq!(track_list.header_height, 22.0);
    assert_eq!(track_list.row_height, 30.0);
    assert_eq!(track_list.footer_height, 22.0);
    assert_eq!(track_list.index_width, 28.0);
    assert_eq!(track_list.deck_width, 64.0);
    assert_eq!(track_list.artist_width, 200.0);
    assert_eq!(track_list.bpm_width, 70.0);
    assert_eq!(track_list.key_width, 56.0);
    assert_eq!(track_list.time_width, 70.0);
    assert_eq!(track_list.energy_width, 110.0);
    assert_eq!(track_list.transition_width, 130.0);
    assert_eq!(track_list.deck_text.size, 9.0);
    assert_eq!(track_list.bpm_badge_background, ColorRole::BgPanel2);
    assert_eq!(track_list.energy_bar_width, 66.0);
    assert_eq!(track_list.energy_bar_height, 4.0);
    assert_eq!(track_list.footer_text.size, 9.0);
    assert_eq!(document.layout_preview.height, 92.0);
}

#[kithara::test]
fn skin_envelope_is_probed_as_skin() {
    let envelope = probe(builtin::DARK_SKIN, &origin()).unwrap();

    assert_eq!(envelope.kind, DocKind::Skin);
}

#[kithara::test]
fn unknown_skin_field_is_rejected() {
    let text = builtin::DARK_SKIN.replacen(
        "id: \"kithara-dark\",",
        "id: \"kithara-dark\", unknown: 1,",
        1,
    );
    let error = parse_skin(&text, &origin()).unwrap_err();

    assert!(matches!(error, UiDocError::Syntax { .. }));
}

#[kithara::test]
fn required_control_skin_field_is_rejected_when_missing() {
    let text = builtin::DARK_SKIN.replacen("        body_fill: BgSelect,\n", "", 1);
    let error = parse_skin(&text, &origin()).unwrap_err();

    assert!(matches!(error, UiDocError::Syntax { .. }));
}

#[kithara::test]
fn required_crossfader_skin_field_is_rejected_when_missing() {
    let text = builtin::DARK_SKIN.replacen("        rail_height: 6.0,\n", "", 1);
    let error = parse_skin(&text, &origin()).unwrap_err();

    assert!(matches!(error, UiDocError::Syntax { .. }));
}

#[kithara::test]
fn required_vis_skin_field_is_rejected_when_missing() {
    let text = builtin::DARK_SKIN.replacen("        header_height: 26.0,\n", "", 1);
    let error = parse_skin(&text, &origin()).unwrap_err();

    assert!(matches!(error, UiDocError::Syntax { .. }));
}

#[kithara::test]
fn malformed_skin_hex_has_typed_error() {
    let text = builtin::DARK_SKIN.replacen("#12121f", "#12xx1f", 1);
    let error = parse_skin(&text, &origin()).unwrap_err();

    assert!(matches!(
        error,
        UiDocError::BadColor { origin, value }
            if origin == self::origin() && value == "#12xx1f"
    ));
}
