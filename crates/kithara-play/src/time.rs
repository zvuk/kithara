use std::time::Duration;

use derivative::Derivative;

#[derive(Clone, Copy, Debug, Derivative, PartialEq)]
#[derivative(Default)]
pub struct MediaTime {
    value: i64,
    #[derivative(Default(value = "1"))]
    timescale: i32,
}

impl MediaTime {
    pub const ZERO: Self = Self {
        value: 0,
        timescale: 1,
    };
    pub const INVALID: Self = Self {
        value: 0,
        timescale: 0,
    };
    pub const POSITIVE_INFINITY: Self = Self {
        value: i64::MAX,
        timescale: 1,
    };

    #[must_use]
    pub fn new(value: i64, timescale: i32) -> Self {
        Self { value, timescale }
    }

    #[must_use]
    #[expect(clippy::cast_possible_truncation)]
    pub fn with_seconds(seconds: f64, timescale: i32) -> Self {
        Self {
            value: (seconds * f64::from(timescale)) as i64,
            timescale,
        }
    }

    #[must_use]
    pub fn with_duration(duration: Duration) -> Self {
        Self::with_seconds(duration.as_secs_f64(), 600)
    }

    #[must_use]
    pub fn value(&self) -> i64 {
        self.value
    }

    #[must_use]
    pub fn timescale(&self) -> i32 {
        self.timescale
    }

    #[must_use]
    #[expect(clippy::cast_precision_loss)]
    pub fn seconds(&self) -> f64 {
        if self.timescale == 0 {
            return 0.0;
        }
        self.value as f64 / f64::from(self.timescale)
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.timescale > 0
    }

    #[must_use]
    pub fn is_indefinite(&self) -> bool {
        self.value == i64::MAX
    }

    #[must_use]
    pub fn to_duration(&self) -> Option<Duration> {
        if !self.is_valid() || self.is_indefinite() {
            return None;
        }
        Some(Duration::from_secs_f64(self.seconds()))
    }
}

impl Eq for MediaTime {}

impl std::hash::Hash for MediaTime {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.value.hash(state);
        self.timescale.hash(state);
    }
}

impl From<Duration> for MediaTime {
    fn from(d: Duration) -> Self {
        Self::with_duration(d)
    }
}

impl PartialOrd for MediaTime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MediaTime {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let lhs = i128::from(self.value) * i128::from(other.timescale);
        let rhs = i128::from(other.value) * i128::from(self.timescale);
        lhs.cmp(&rhs)
    }
}

impl std::ops::Add for MediaTime {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        if self.timescale == rhs.timescale {
            return Self::new(self.value + rhs.value, self.timescale);
        }
        let ts = self.timescale.max(rhs.timescale);
        Self::with_seconds(self.seconds() + rhs.seconds(), ts)
    }
}

impl std::ops::Sub for MediaTime {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        if self.timescale == rhs.timescale {
            return Self::new(self.value - rhs.value, self.timescale);
        }
        let ts = self.timescale.max(rhs.timescale);
        Self::with_seconds(self.seconds() - rhs.seconds(), ts)
    }
}
