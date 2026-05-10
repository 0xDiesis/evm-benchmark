pub mod burst;
pub mod ceiling;
pub mod sustained;

pub use burst::run_burst;
#[allow(unused_imports)]
pub use ceiling::run_ceiling;
pub use sustained::run_sustained;

const BENCH_CONFIRM_WAIT_SECS: &str = "BENCH_CONFIRM_WAIT_SECS";

pub(crate) fn confirmation_wait_duration(default_secs: u64) -> std::time::Duration {
    std::time::Duration::from_secs(confirmation_wait_secs_from(
        default_secs,
        std::env::var(BENCH_CONFIRM_WAIT_SECS).ok().as_deref(),
    ))
}

fn confirmation_wait_secs_from(default_secs: u64, raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(default_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_wait_secs_uses_default_when_unset() {
        assert_eq!(confirmation_wait_secs_from(30, None), 30);
    }

    #[test]
    fn confirmation_wait_secs_uses_valid_override() {
        assert_eq!(confirmation_wait_secs_from(30, Some("90")), 90);
    }

    #[test]
    fn confirmation_wait_secs_ignores_zero_or_invalid_override() {
        assert_eq!(confirmation_wait_secs_from(30, Some("0")), 30);
        assert_eq!(confirmation_wait_secs_from(30, Some("garbage")), 30);
    }
}
