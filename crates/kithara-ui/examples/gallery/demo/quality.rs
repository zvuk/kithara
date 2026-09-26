use kithara_ui::render::ReadValue;

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

pub(crate) struct QualityState {
    value: String,
    auto: bool,
    open: bool,
    current: usize,
}

impl Default for QualityState {
    fn default() -> Self {
        let mut state = Self {
            open: false,
            auto: true,
            current: 1,
            value: String::new(),
        };
        state.rebuild();
        state
    }
}

impl QualityState {
    pub(crate) fn activate(&mut self, path: &str) -> bool {
        let Some((_, id)) = path.split_once("/stream/") else {
            return false;
        };
        let (node, _) = id.split_once('/').unwrap_or((id, ""));
        match node {
            "pop" => self.open = false,
            "cell" => self.open = !self.open,
            "auto" => self.select(None),
            _ => return self.select_variant(node),
        }
        true
    }

    fn active(&self, variant: &str) -> Option<bool> {
        if variant == "auto" {
            return Some(self.auto);
        }
        Some(!self.auto && index(variant)? == self.current)
    }

    pub(crate) fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        let (id, scope) = endpoint.split_once('@').unwrap_or((endpoint, ""));
        let value = match id {
            "deck.stream.quality_menu" => ReadValue::Bool(self.open),
            "deck.stream.quality" => ReadValue::Text(&self.value),
            "deck.stream.quality_hidden" => ReadValue::Bool(false),
            "deck.stream.variant_active" => ReadValue::Bool(self.active(variant(scope)?)?),
            "deck.stream.variant_hidden" => {
                ReadValue::Bool(index(variant(scope)?)? >= consts::VARIANTS.len())
            }
            "deck.stream.variant_label" => ReadValue::Text(Self::text(variant(scope)?)?.label),
            "deck.stream.variant_sub" => ReadValue::Text(Self::text(variant(scope)?)?.sub),
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
        self.open = false;
        self.rebuild();
    }

    fn select_variant(&mut self, node: &str) -> bool {
        let Some(index) = node
            .strip_prefix("variant-")
            .and_then(index)
            .filter(|slot| *slot < consts::VARIANTS.len())
        else {
            return false;
        };
        self.select(Some(index));
        true
    }

    fn text(variant: &str) -> Option<&'static QualityVariant> {
        consts::VARIANTS.get(index(variant)?)
    }
}

fn variant(scope: &str) -> Option<&str> {
    scope
        .split(',')
        .find_map(|pair| pair.strip_prefix("variant="))
}

fn index(variant: &str) -> Option<usize> {
    variant.parse().ok().filter(|slot| *slot < consts::SLOTS)
}
