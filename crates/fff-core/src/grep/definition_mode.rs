/// Controls classification and selection of matching declaration lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum DefinitionMode {
    #[default]
    Off = 0,
    Classify = 1,
    DefinitionOnly = 2,
    UsageOnly = 3,
}

impl DefinitionMode {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Off),
            1 => Some(Self::Classify),
            2 => Some(Self::DefinitionOnly),
            3 => Some(Self::UsageOnly),
            _ => None,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "classify" => Some(Self::Classify),
            "definition_only" => Some(Self::DefinitionOnly),
            "usage_only" => Some(Self::UsageOnly),
            _ => None,
        }
    }

    pub fn is_enabled(self) -> bool {
        self != Self::Off
    }

    pub fn is_filtered(self) -> bool {
        matches!(self, Self::DefinitionOnly | Self::UsageOnly)
    }

    pub(crate) fn retains(self, is_definition: bool) -> bool {
        match self {
            Self::DefinitionOnly => is_definition,
            Self::UsageOnly => !is_definition,
            _ => true,
        }
    }

    #[inline]
    pub(super) fn classify_match(
        self,
        file: &crate::types::FileItem,
        line: &[u8],
        ranges: &[(u32, u32)],
        fuzzy: bool,
    ) -> Option<bool> {
        let is_definition = self.is_enabled()
            && file.classify_definition_match(line, ranges, fuzzy)
            && !line.contains(&0);
        self.retains(is_definition).then_some(is_definition)
    }
}

impl From<bool> for DefinitionMode {
    fn from(enabled: bool) -> Self {
        if enabled { Self::Classify } else { Self::Off }
    }
}
