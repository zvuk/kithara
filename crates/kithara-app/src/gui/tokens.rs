/// Semantic spacing scale used by the modular canvas.
pub(crate) mod gap {
    pub(crate) const GRID: f32 = 1.0;
    pub(crate) const INLINE_TIGHT: f32 = 4.0;
    pub(crate) const INLINE_WIDE: f32 = 8.0;
    pub(crate) const CONTENT: f32 = 10.0;
}

pub(crate) mod canvas {
    pub(crate) const ERROR_TEXT_SIZE: f32 = 12.0;
    pub(crate) const FILL_WEIGHT_SCALE: f32 = 100.0;
    pub(crate) const LOAD_BUTTON_TEXT_SIZE: f32 = 12.0;
    pub(crate) const LOAD_BUTTON_PADDING_X: f32 = 10.0;
    pub(crate) const LOAD_BUTTON_PADDING_Y: f32 = 6.0;
}

pub(crate) mod chrome {
    pub(crate) const BORDER_WIDTH: f32 = 1.0;
    pub(crate) const CORNER_TICK_SIZE: f32 = 9.0;
    pub(crate) const CORNER_TICK_WIDTH: f32 = 2.0;
    pub(crate) const SHORT_MODULE_HEIGHT: f32 = 48.0;
}

pub(crate) mod type_scale {
    pub(crate) const BODY: f32 = 13.0;
    pub(crate) const MICRO_LABEL: f32 = 12.0;
}

pub(crate) mod global_bar {
    pub(crate) const BRAND_GAP: f32 = 3.0;
    pub(crate) const BRAND_PADDING_X: f32 = 13.0;
    pub(crate) const BRAND_SIZE: f32 = 14.0;
    pub(crate) const BRAND_WIDTH: f32 = 112.0;
    pub(crate) const CHIP_PADDING_X: f32 = 8.0;
    pub(crate) const CHIP_PADDING_Y: f32 = 3.0;
    pub(crate) const CHIP_TEXT: f32 = 9.0;
    pub(crate) const GEAR_SIZE: f32 = 12.0;
    pub(crate) const HEIGHT: f32 = 34.0;
    pub(crate) const SELECTOR_PADDING_X: f32 = 10.0;
    pub(crate) const SELECTOR_WIDTH: f32 = 126.0;
    pub(crate) const SETTINGS_WIDTH: f32 = 34.0;
}

pub(crate) mod deck {
    pub(crate) const ART_BORDER_WIDTH: f32 = 1.0;
    pub(crate) const ART_LABEL_SIZE: f32 = 7.0;
    pub(crate) const ART_SIZE: f32 = 40.0;
    pub(crate) const BADGE_SIZE: f32 = 26.0;
    pub(crate) const BADGE_TEXT: f32 = 14.0;
    pub(crate) const BPM_CELL_WIDTH: f32 = 64.0;
    pub(crate) const HEADER_GAP: f32 = 12.0;
    pub(crate) const HEADER_HEIGHT: f32 = 60.0;
    pub(crate) const HEADER_PADDING_X: f32 = 12.0;
    pub(crate) const HEADER_PADDING_Y: f32 = 9.0;
    pub(crate) const TITLE_SIZE: f32 = 15.0;
    pub(crate) const ARTIST_SIZE: f32 = 13.0;
    pub(crate) const SUMMARY_HEIGHT: f32 = 34.0;
    pub(crate) const SUMMARY_MIN_WIDTH: f32 = 90.0;
    pub(crate) const SUMMARY_PADDING_X: f32 = 10.0;
    pub(crate) const SUMMARY_FILL_PORTION: u16 = 3;
    pub(crate) const MICRO_SOURCE_SIZE: f32 = 13.0;
    pub(crate) const MICRO_SUMMARY_GAP: f32 = 7.0;
    pub(crate) const MICRO_TITLE_SIZE: f32 = 14.0;
    pub(crate) const READOUT_HEIGHT: f32 = 60.0;
    pub(crate) const READOUT_LABEL_SIZE: f32 = 9.0;
    pub(crate) const READOUT_VALUE_SIZE: f32 = 13.0;
    pub(crate) const REMAIN_CELL_WIDTH: f32 = 72.0;
    pub(crate) const TELEMETRY_PADDING_X: f32 = 8.0;
    pub(crate) const TELEMETRY_PADDING_Y: f32 = 3.0;
}

pub(crate) mod waveform {
    pub(crate) const BAR_GAP: f32 = 1.0;
    pub(crate) const CONTENT_INSET: f32 = 2.0;
    pub(crate) const DOWNBEAT_ALPHA: f32 = 0.72;
    pub(crate) const GRID_ALPHA: f32 = 0.55;
    pub(crate) const GRID_WIDTH: f32 = 1.0;
    pub(crate) const HERO_HEIGHT: f32 = 120.0;
    pub(crate) const HIGH_BAR_WIDTH: f32 = 1.0;
    pub(crate) const LOW_BAR_WIDTH: f32 = 3.0;
    pub(crate) const MID_BAR_WIDTH: f32 = 2.0;
    pub(crate) const PLAYHEAD_WIDTH: f32 = 2.0;
}

pub(crate) mod transport {
    pub(crate) const BUTTON_HEIGHT: f32 = 42.0;
    pub(crate) const BUTTON_MIN_WIDTH: f32 = 72.0;
    pub(crate) const BUTTON_PADDING_X: f32 = 10.0;
    pub(crate) const BUTTON_ICON_SIZE: f32 = 11.0;
    pub(crate) const BUTTON_TEXT: f32 = 13.0;
    pub(crate) const PRIMARY_TEXT: f32 = 14.0;
    pub(crate) const MICRO_BUTTON_SIZE: f32 = 34.0;
    pub(crate) const MICRO_ICON_SIZE: f32 = 14.0;
    pub(crate) const TIME_PADDING_X: f32 = 11.0;
    pub(crate) const TIME_TEXT: f32 = 11.0;
    pub(crate) const TIME_WIDTH: f32 = 144.0;
}

pub(crate) mod volume {
    pub(crate) const BORDER_WIDTH: f32 = 1.0;
    pub(crate) const CONTENT_GAP: f32 = 6.0;
    pub(crate) const CONTROL_HEIGHT: f32 = 34.0;
    pub(crate) const CONTROL_PADDING_X: f32 = 8.0;
    pub(crate) const ICON_SIZE: f32 = 11.0;
    pub(crate) const LABEL_WIDTH: f32 = 18.0;
    pub(crate) const MIN_WIDTH: f32 = 90.0;
    pub(crate) const SEGMENT_COUNT: usize = 12;
    pub(crate) const SEGMENT_GAP: f32 = 1.5;
    pub(crate) const SEGMENT_HEIGHT: f32 = 10.0;
    pub(crate) const SLIDER_HEIGHT: f32 = 16.0;
    pub(crate) const STRIP_HEIGHT: f32 = 14.0;
    pub(crate) const STRIP_PADDING: f32 = 2.0;
    pub(crate) const RAIL_WIDTH: f32 = 6.0;
    pub(crate) const HANDLE_WIDTH: u16 = 6;
    pub(crate) const HANDLE_BORDER_WIDTH: f32 = 1.0;
    pub(crate) const TICK_STEP: f32 = 12.0;
    pub(crate) const TICK_HEIGHT: f32 = 4.0;
    pub(crate) const TICKS_HEIGHT: f32 = 22.0;
    pub(crate) const STEP: f64 = 0.01;
}

pub(crate) mod telemetry {
    pub(crate) const BPM_TEXT_SIZE: f32 = 11.0;
    pub(crate) const SCALAR_PADDING_X: f32 = 7.0;
    pub(crate) const SCALAR_PADDING_Y: f32 = 4.0;
    pub(crate) const SCALAR_TEXT_SIZE: f32 = 12.0;
}

pub(crate) mod track_list {
    pub(crate) const ARTIST_WIDTH: f32 = 170.0;
    pub(crate) const CHIP_SIZE: f32 = 18.0;
    pub(crate) const CHIP_TEXT_SIZE: f32 = 14.0;
    pub(crate) const COUNT_PADDING_X: f32 = 11.0;
    pub(crate) const COUNT_TEXT_SIZE: f32 = 9.0;
    pub(crate) const HEADER_HEIGHT: f32 = 24.0;
    pub(crate) const HEADER_TEXT_SIZE: f32 = 8.0;
    pub(crate) const INPUT_PADDING_X: f32 = 12.0;
    pub(crate) const INPUT_TEXT_SIZE: f32 = 12.0;
    pub(crate) const MIN_HEIGHT: f32 = 210.0;
    pub(crate) const MIN_WIDTH: f32 = 600.0;
    pub(crate) const NUMBER_TEXT_SIZE: f32 = 10.0;
    pub(crate) const NUMBER_WIDTH: f32 = 44.0;
    pub(crate) const ROW_HEIGHT: f32 = 30.0;
    pub(crate) const ROW_TEXT_SIZE: f32 = 13.0;
    pub(crate) const SEARCH_HEIGHT: f32 = 30.0;
    pub(crate) const TIME_PADDING_X: f32 = 12.0;
    pub(crate) const TIME_TEXT_SIZE: f32 = 12.0;
    pub(crate) const TIME_WIDTH: f32 = 70.0;
    pub(crate) const TITLE_GAP: f32 = 8.0;
}

pub(crate) mod settings {
    pub(crate) const OUTER_PADDING: f32 = 12.0;
    pub(crate) const CONTENT_PADDING: f32 = 16.0;
    pub(crate) const CARD_PADDING_X: f32 = 12.0;
    pub(crate) const CARD_PADDING_Y: f32 = 10.0;
    pub(crate) const CARD_TITLE_SIZE: f32 = 15.0;
    pub(crate) const ROW_PADDING_X: f32 = 8.0;
    pub(crate) const ROW_PADDING_Y: f32 = 6.0;
    pub(crate) const ACTION_PADDING_X: f32 = 16.0;
    pub(crate) const ACTION_PADDING_Y: f32 = 7.0;
    pub(crate) const TOGGLE_PADDING_X: f32 = 8.0;
    pub(crate) const TOGGLE_PADDING_Y: f32 = 4.0;
}
