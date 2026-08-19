#[cfg(feature = "render")]
use crate::{atoms::bar::preset::PresetItem, builtin};
use crate::{mount::Control, size::SizeSpec, skin::SkinDoc};

/// The global bar's preset picker.
pub(crate) struct Preset;

#[cfg(feature = "render")]
const ITEMS: [PresetItem; 2] = [
    PresetItem {
        label: "MICRO",
        name: builtin::MICRO_PRESET,
    },
    PresetItem {
        label: "PLAYER",
        name: builtin::PLAYER_PRESET,
    },
];

impl Control for Preset {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        skin.global_bar.preset_size
    }
}

#[cfg(feature = "render")]
mod host {
    use super::{ITEMS, Preset};
    #[cfg(feature = "masonry")]
    use crate::render::controls::DataRefresh;
    use crate::{
        atoms::bar::preset::{Preset as Face, PresetData, PresetItem},
        render::{
            ReadValue, Skin, UiEvent,
            controls::{Draws, Grip, IndexEvent, Reading},
            document::Ctx,
        },
    };

    impl Draws for Preset {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(skin)
        }

        fn data(&self, read: Reading<'_>) -> Option<PresetData> {
            Some(Self::snapshot(read.ctx))
        }

        fn grip(&self, _skin: &Skin, data: &PresetData) -> Grip {
            Grip::Index {
                count: data.items.len(),
            }
        }

        fn index_event(&self) -> Option<IndexEvent<PresetData>> {
            Some(select)
        }

        #[cfg(feature = "masonry")]
        fn retained_refresh(
            &self,
            _read: Reading<'_>,
            _endpoint: Option<&str>,
        ) -> Option<DataRefresh<PresetData>> {
            Some(Box::new(refresh))
        }
    }

    impl Preset {
        pub(crate) fn snapshot(ctx: Ctx<'_, '_>) -> PresetData {
            let items = &ITEMS;
            let active = active(items, ctx);
            PresetData { active, items }
        }
    }

    fn active(items: &[PresetItem], ctx: Ctx<'_, '_>) -> Option<usize> {
        let Some(ReadValue::Text(name)) = ctx.get("ui.preset") else {
            return None;
        };
        items.iter().position(|item| item.name == name)
    }

    fn select(data: &PresetData, index: usize) -> Option<UiEvent> {
        data.items
            .get(index)
            .map(|item| UiEvent::SelectPreset(item.name.to_owned()))
    }

    #[cfg(feature = "masonry")]
    fn refresh(data: &mut PresetData, ctx: Ctx<'_, '_>) -> bool {
        let active = active(data.items, ctx);
        std::mem::replace(&mut data.active, active) != active
    }

    #[cfg(test)]
    mod tests {
        use kithara_test_utils::kithara;

        use super::*;
        use crate::{
            builtin,
            render::{Reads, document::probe},
        };

        struct PresetReads(Option<ReadValue<'static>>);

        impl Reads for PresetReads {
            fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
                if endpoint == "ui.preset" {
                    self.0
                } else {
                    None
                }
            }
        }

        #[kithara::test]
        fn data_keeps_the_canonical_inventory_and_reads_the_active_name() {
            let data = Preset::snapshot(probe(&PresetReads(Some(ReadValue::Text(
                builtin::PLAYER_PRESET,
            )))));

            assert_eq!(data.items.len(), 2);
            assert_eq!(data.items[0].label, "MICRO");
            assert_eq!(data.items[0].name, builtin::MICRO_PRESET);
            assert_eq!(data.items[1].label, "PLAYER");
            assert_eq!(data.items[1].name, builtin::PLAYER_PRESET);
            assert_eq!(data.active, Some(1));
        }

        #[kithara::test]
        fn an_absent_wrong_or_unknown_read_has_no_active_item() {
            for reads in [
                PresetReads(None),
                PresetReads(Some(ReadValue::Bool(true))),
                PresetReads(Some(ReadValue::Text("unknown.klayout.ron"))),
            ] {
                assert_eq!(Preset::snapshot(probe(&reads)).active, None);
            }
        }

        #[kithara::test]
        fn selection_reads_the_name_from_the_same_data_item() {
            let data = Preset::snapshot(probe(&PresetReads(None)));

            assert_eq!(
                select(&data, 0),
                Some(UiEvent::SelectPreset(builtin::MICRO_PRESET.to_owned()))
            );
            assert_eq!(
                select(&data, 1),
                Some(UiEvent::SelectPreset(builtin::PLAYER_PRESET.to_owned()))
            );
            assert_eq!(select(&data, 2), None);
        }
    }
}
