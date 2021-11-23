use super::*;

/// A relative length.
///
/// _Note_: `50%` is represented as `0.5` here, but stored as `50.0` in the
/// corresponding [literal](crate::syntax::ast::LitKind::Percent).
#[derive(Default, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[derive(Serialize, Deserialize)]
pub struct Relative(Scalar);

impl Relative {
    /// A ratio of `0%` represented as `0.0`.
    pub const fn zero() -> Self {
        Self(Scalar(0.0))
    }

    /// A ratio of `100%` represented as `1.0`.
    pub const fn one() -> Self {
        Self(Scalar(1.0))
    }

    /// Create a new relative value.
    pub const fn new(ratio: f64) -> Self {
        Self(Scalar(ratio))
    }

    /// Get the underlying ratio.
    pub const fn get(self) -> f64 {
        (self.0).0
    }

    /// Resolve this relative to the given `length`.
    pub fn resolve(self, length: Length) -> Length {
        // We don't want NaNs.
        if length.is_infinite() {
            Length::zero()
        } else {
            self.get() * length
        }
    }

    /// Whether the ratio is zero.
    pub fn is_zero(self) -> bool {
        self.0 == 0.0
    }

    /// The absolute value of the this relative.
    pub fn abs(self) -> Self {
        Self::new(self.get().abs())
    }
}

impl Debug for Relative {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{}%", 100.0 * self.get())
    }
}

impl Neg for Relative {
    type Output = Self;

    fn neg(self) -> Self {
        Self(-self.0)
    }
}

impl Add for Relative {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

sub_impl!(Relative - Relative -> Relative);

impl Mul for Relative {
    type Output = Self;

    fn mul(self, other: Self) -> Self {
        Self(self.0 * other.0)
    }
}

impl Mul<f64> for Relative {
    type Output = Self;

    fn mul(self, other: f64) -> Self {
        Self(self.0 * other)
    }
}

impl Mul<Relative> for f64 {
    type Output = Relative;

    fn mul(self, other: Relative) -> Relative {
        other * self
    }
}

impl Div<f64> for Relative {
    type Output = Self;

    fn div(self, other: f64) -> Self {
        Self(self.0 / other)
    }
}

impl Div for Relative {
    type Output = f64;

    fn div(self, other: Self) -> f64 {
        self.get() / other.get()
    }
}

assign_impl!(Relative += Relative);
assign_impl!(Relative -= Relative);
assign_impl!(Relative *= Relative);
assign_impl!(Relative *= f64);
assign_impl!(Relative /= f64);
