use std::time::Duration;
use thiserror::Error;

/// Custom error type for benchmark harness operations
#[allow(dead_code)]
#[derive(Error, Debug)]
pub enum BenchError {
    #[error("RPC error: {0}")]
    RpcError(String),

    #[error("Signing error: {0}")]
    SigningError(String),

    #[error("Nonce too low, reconciling...")]
    NonceTooLow,

    #[error("Timeout waiting for confirmation")]
    ConfirmationTimeout,

    #[error("Connection error: {0}")]
    ConnectionError(String),

    #[error("Transaction submission error: {0}")]
    SubmissionError(String),

    #[error("IO error: {0}")]
    IoError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Internal error: {0}")]
    InternalError(String),
}

impl From<anyhow::Error> for BenchError {
    fn from(err: anyhow::Error) -> Self {
        BenchError::InternalError(err.to_string())
    }
}

/// Retry a function with exponential backoff
///
/// # Arguments
/// * `mut f` - Async function that returns a Result
/// * `max_attempts` - Maximum number of retry attempts
///
/// # Returns
/// The result of the function if successful, or the last error if all attempts fail
#[allow(dead_code)]
pub async fn retry_with_backoff<F, T>(mut f: F, max_attempts: u32) -> Result<T, BenchError>
where
    F: FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, BenchError>>>>,
{
    for attempt in 0..max_attempts {
        match f().await {
            Ok(result) => {
                if attempt > 0 {
                    tracing::info!(attempt, "Retry successful");
                }
                return Ok(result);
            }
            Err(e) => {
                if attempt < max_attempts - 1 {
                    // Calculate exponential backoff: 2^attempt milliseconds
                    let backoff_ms = 2u64.pow(attempt);
                    let sleep_duration = Duration::from_millis(backoff_ms);

                    tracing::warn!(
                        attempt,
                        backoff_ms,
                        error = %e,
                        "Transient error, retrying..."
                    );

                    tokio::time::sleep(sleep_duration).await;
                } else {
                    tracing::error!(
                        total_attempts = max_attempts,
                        final_error = %e,
                        "All retry attempts exhausted"
                    );
                    return Err(e);
                }
            }
        }
    }

    Err(BenchError::InternalError(
        "retry loop exited unexpectedly".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn test_retry_success_first_attempt() {
        let result = retry_with_backoff(|| Box::pin(async { Ok::<i32, BenchError>(42) }), 3).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_retry_success_after_failures() {
        let attempt_count = Arc::new(AtomicU32::new(0));
        let count_clone = Arc::clone(&attempt_count);

        let result = retry_with_backoff(
            move || {
                let count = Arc::clone(&count_clone);
                Box::pin(async move {
                    let current = count.fetch_add(1, Ordering::SeqCst);
                    if current < 2 {
                        Err(BenchError::ConnectionError("simulated error".to_string()))
                    } else {
                        Ok(42)
                    }
                })
            },
            5,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(attempt_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_exhausted() {
        let result = retry_with_backoff(
            || Box::pin(async { Err::<i32, _>(BenchError::RpcError("always fails".to_string())) }),
            3,
        )
        .await;

        assert!(result.is_err());
    }

    #[test]
    fn test_bench_error_display() {
        let err = BenchError::RpcError("test error".to_string());
        assert_eq!(err.to_string(), "RPC error: test error");
    }

    #[test]
    fn test_bench_error_display_all_variants() {
        assert_eq!(
            BenchError::NonceTooLow.to_string(),
            "Nonce too low, reconciling..."
        );
        assert_eq!(
            BenchError::ConfirmationTimeout.to_string(),
            "Timeout waiting for confirmation"
        );
        assert_eq!(
            BenchError::ConnectionError("conn down".into()).to_string(),
            "Connection error: conn down"
        );
        assert_eq!(
            BenchError::SubmissionError("bad tx".into()).to_string(),
            "Transaction submission error: bad tx"
        );
        assert_eq!(
            BenchError::IoError("disk full".into()).to_string(),
            "IO error: disk full"
        );
        assert_eq!(
            BenchError::ConfigError("missing key".into()).to_string(),
            "Configuration error: missing key"
        );
        assert_eq!(
            BenchError::InternalError("oops".into()).to_string(),
            "Internal error: oops"
        );
        assert_eq!(
            BenchError::SigningError("bad key".into()).to_string(),
            "Signing error: bad key"
        );
    }

    #[test]
    fn test_from_anyhow_error() {
        let anyhow_err = anyhow::anyhow!("something went wrong");
        let bench_err: BenchError = anyhow_err.into();
        match bench_err {
            BenchError::InternalError(msg) => {
                assert!(msg.contains("something went wrong"));
            }
            other => panic!("Expected InternalError, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_retry_with_backoff_zero_attempts() {
        let result = retry_with_backoff(|| Box::pin(async { Ok::<i32, BenchError>(99) }), 0).await;

        // With 0 attempts the loop body never runs, so we hit the fallback InternalError.
        match result {
            Err(BenchError::InternalError(msg)) => {
                assert!(msg.contains("retry loop exited unexpectedly"));
            }
            other => panic!("Expected InternalError for 0 attempts, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_retry_with_backoff_one_attempt_success() {
        let result = retry_with_backoff(|| Box::pin(async { Ok::<i32, BenchError>(7) }), 1).await;
        assert_eq!(result.unwrap(), 7);
    }

    #[tokio::test]
    async fn test_retry_with_backoff_one_attempt_failure() {
        let result = retry_with_backoff(
            || Box::pin(async { Err::<i32, _>(BenchError::RpcError("fail".into())) }),
            1,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "RPC error: fail");
    }

    /// Tests the "retry successful after first failure" path, which exercises
    /// the `attempt > 0` branch that logs info about retry success.
    #[tokio::test]
    async fn test_retry_success_on_second_attempt_logs_info() {
        let attempt_count = Arc::new(AtomicU32::new(0));
        let count_clone = Arc::clone(&attempt_count);

        let result = retry_with_backoff(
            move || {
                let count = Arc::clone(&count_clone);
                Box::pin(async move {
                    let current = count.fetch_add(1, Ordering::SeqCst);
                    if current == 0 {
                        Err(BenchError::ConnectionError("transient".to_string()))
                    } else {
                        // This is attempt > 0, so the info! tracing path is hit
                        Ok(99)
                    }
                })
            },
            3,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 99);
        // Exactly 2 attempts: first fails, second succeeds
        assert_eq!(attempt_count.load(Ordering::SeqCst), 2);
    }

    /// Tests that retry exhausts all attempts and returns the final error,
    /// hitting the error! tracing path on the last attempt.
    #[tokio::test]
    async fn test_retry_exhausted_two_attempts() {
        let attempt_count = Arc::new(AtomicU32::new(0));
        let count_clone = Arc::clone(&attempt_count);

        let result = retry_with_backoff(
            move || {
                let count = Arc::clone(&count_clone);
                Box::pin(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Err::<i32, _>(BenchError::RpcError("persistent".to_string()))
                })
            },
            2,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "RPC error: persistent");
        assert_eq!(attempt_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_bench_error_debug_format() {
        let err = BenchError::NonceTooLow;
        let debug_str = format!("{:?}", err);
        assert!(debug_str.contains("NonceTooLow"));
    }
}
