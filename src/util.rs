// Converts a `u64` to `f64` without an `as` cast. Values above `u32::MAX` saturate
// instead of losing precision silently, which is precise enough for term/doc counters.
pub fn u64_to_f64_lossy(value: u64) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}
