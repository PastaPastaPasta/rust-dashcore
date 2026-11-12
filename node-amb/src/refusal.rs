/// A pass, record or comparison that gives no number, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal(pub String);

pub type Result<T> = std::result::Result<T, Refusal>;

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "refused: {}", self.0)
    }
}

impl std::error::Error for Refusal {}

/// Returns a [`Refusal`] from the enclosing function.
#[macro_export]
macro_rules! refuse {
    ($($arg:tt)*) => {
        return Err($crate::Refusal(format!($($arg)*)))
    };
}
