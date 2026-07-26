use crate::{
    module::IconName,
    render::{Icon, TreeIcon},
};

pub(super) fn render_icon(icon: IconName) -> Icon {
    match icon {
        IconName::ChevronDown => Icon::ChevronDown,
        IconName::ChevronUp => Icon::ChevronUp,
        IconName::Disc => Icon::Disc,
        IconName::Faders => Icon::Faders,
        IconName::FastForward => Icon::FastForward,
        IconName::Gear => Icon::Gear,
        IconName::Headphones => Icon::Headphones,
        IconName::Maximize => Icon::Maximize,
        IconName::Menu => Icon::Menu,
        IconName::Play => Icon::Play,
        IconName::PlayReverse => Icon::PlayReverse,
        IconName::Playlist => Icon::Playlist,
        IconName::Rewind => Icon::Rewind,
        IconName::SpeakerHigh => Icon::SpeakerHigh,
        IconName::Waveform => Icon::Waveform,
        IconName::X => Icon::X,
        IconName::ZoomIn => Icon::ZoomIn,
        IconName::ZoomOut => Icon::ZoomOut,
    }
}

pub(super) fn render_tree_icon(icon: TreeIcon) -> Icon {
    match icon {
        TreeIcon::Collection => Icon::Collection,
        TreeIcon::Playlist => Icon::Playlist,
        TreeIcon::Folder => Icon::Folder,
        TreeIcon::Plus => Icon::Plus,
        TreeIcon::Zvuk => Icon::Zvuk,
        TreeIcon::Search => Icon::Search,
        TreeIcon::Charts => Icon::Charts,
        TreeIcon::Monitor => Icon::Monitor,
        TreeIcon::Home => Icon::Home,
        TreeIcon::Usb => Icon::Usb,
        TreeIcon::Instrument => Icon::Instrument,
        TreeIcon::Waveform => Icon::Waveform,
        TreeIcon::Clock => Icon::Clock,
    }
}
