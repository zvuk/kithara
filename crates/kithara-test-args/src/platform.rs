use std::{fmt, str::FromStr};

/// A target platform a test, or one of its cases, requires.
///
/// The test attribute compiles a declared test only for its platform and
/// prefixes its name, so a runner can select exactly the tests that need it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestPlatform {
    /// macOS and iOS: Apple frameworks, decoders and system services.
    Apple,
}

impl TestPlatform {
    /// Every platform a test can declare.
    pub const ALL: [Self; 1] = [Self::Apple];

    /// The name a test attribute and a runner use for this platform.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Apple => "apple",
        }
    }
}

impl fmt::Display for TestPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for TestPlatform {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|platform| platform.name() == value)
            .ok_or_else(|| {
                let known: Vec<_> = Self::ALL.iter().map(|platform| platform.name()).collect();
                format!(
                    "unknown platform `{value}`; known platforms: {}",
                    known.join(", ")
                )
            })
    }
}
