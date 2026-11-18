use kithara_ui::render::{ReadValue, Scope, WriteValue};

struct QualityVariant {
    label: &'static str,
    sub: &'static str,
}

mod consts {
    use super::QualityVariant;

    pub(super) const SLOTS: usize = 6;
    pub(super) const VARIANTS: [QualityVariant; 3] = [
        QualityVariant {
            label: "FLAC",
            sub: "1.4 MBPS",
        },
        QualityVariant {
            label: "320",
            sub: "AAC 320K",
        },
        QualityVariant {
            label: "128",
            sub: "AAC 128K",
        },
    ];
}

pub struct QualityState {
    value: String,
    auto: bool,
    current: usize,
}

impl Default for QualityState {
    fn default() -> Self {
        let mut state = Self {
            auto: true,
            current: 1,
            value: String::new(),
        };
        state.rebuild();
        state
    }
}

impl QualityState {
    /// Answers the pick of one variant, or of the automatic choice.
    pub fn write(&mut self, id: &str, scope: Scope<'_>, value: &WriteValue) {
        if !matches!(
            (id, value),
            ("deck.stream.select_variant", WriteValue::Trigger)
        ) {
            return;
        }
        match scope.get("variant") {
            Some("auto") => self.select(None),
            Some(variant) => {
                if let Some(slot) = index(variant).filter(|slot| *slot < consts::VARIANTS.len()) {
                    self.select(Some(slot));
                }
            }
            None => {}
        }
    }

    fn active(&self, variant: &str) -> Option<bool> {
        if variant == "auto" {
            return Some(self.auto);
        }
        Some(!self.auto && index(variant)? == self.current)
    }

    #[must_use]
    pub fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let (id, scope) = Scope::split(endpoint);
        let value = match id {
            "deck.stream.quality" => ReadValue::Text(&self.value),
            "deck.stream.quality_hidden" | "deck.stream.auto_hidden" => ReadValue::Bool(false),
            "deck.stream.quality_ready" | "deck.stream.error_hidden" => ReadValue::Bool(true),
            "deck.stream.variant_active" => ReadValue::Bool(self.active(scope.get("variant")?)?),
            "deck.stream.variant_hidden" => {
                ReadValue::Bool(index(scope.get("variant")?)? >= consts::VARIANTS.len())
            }
            "deck.stream.variant_label" => {
                ReadValue::Text(Self::text(scope.get("variant")?)?.label)
            }
            "deck.stream.variant_sub" => ReadValue::Text(Self::text(scope.get("variant")?)?.sub),
            _ => return None,
        };
        Some(value)
    }

    fn rebuild(&mut self) {
        let label = consts::VARIANTS[self.current].label;
        self.value = if self.auto {
            format!("AUTO·{label}")
        } else {
            label.to_owned()
        };
    }

    fn select(&mut self, variant: Option<usize>) {
        match variant {
            Some(index) => {
                self.auto = false;
                self.current = index;
            }
            None => self.auto = true,
        }
        self.rebuild();
    }

    fn text(variant: &str) -> Option<&'static QualityVariant> {
        consts::VARIANTS.get(index(variant)?)
    }
}

fn index(variant: &str) -> Option<usize> {
    variant.parse().ok().filter(|slot| *slot < consts::SLOTS)
}
