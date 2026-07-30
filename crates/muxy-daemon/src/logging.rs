//! Structured logging setup + a testable helper for connection-task error reporting.

use anyhow::Result;

/// The warning line for a finished connection task, or `None` if it ended cleanly.
pub fn conn_error_line(kind: &str, result: Result<()>) -> Option<String> {
    result
        .err()
        .map(|e| format!("{kind} connection task ended with error: {e}"))
}

/// Install the global tracing subscriber (RUST_LOG-aware, default `info`). Call once at startup.
pub fn init() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn ok_result_produces_no_line() {
        assert_eq!(conn_error_line("client", Ok(())), None);
    }

    #[test]
    fn err_result_produces_a_line_naming_kind_and_error() {
        let line = conn_error_line("control", Err(anyhow!("boom"))).expect("Err must produce a line");
        assert!(line.contains("control"), "line should name the connection kind: {line}");
        assert!(line.contains("boom"), "line should include the error: {line}");
    }
}
