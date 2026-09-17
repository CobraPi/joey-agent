//! T020 — watchdog overrun warning (feature 020, US5; contracts/api.md §2).
//! The op sleeps 500ms ignoring cancellation; limit 100ms ⇒ a warning is
//! emitted AND the job still completes.

use joey_compute::watchdog::Watchdog;
use joey_compute::CancelToken;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[test]
fn overrun_warns_and_job_still_completes() {
    // Local subscriber capturing formatted log lines (the documented
    // tracing-subscriber test pattern: layer + writer capturing output).
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let buf = captured.clone();
    struct Capture(Arc<Mutex<Vec<String>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, buf_in: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().push(String::from_utf8_lossy(buf_in).into_owned());
            Ok(buf_in.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl tracing_subscriber::fmt::MakeWriter<'_> for Capture {
        type Writer = Self;
        fn make_writer(&self) -> Self::Writer {
            Capture(self.0.clone())
        }
    }
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::FmtSubscriber::builder()
            .with_writer(Capture(buf))
            .with_max_level(tracing::Level::WARN)
            .finish(),
    );

    let wd = Watchdog::new(Duration::from_millis(100));
    let token = CancelToken::new();
    let out = wd.guard(|| {
        // Op ignores the cancel token entirely (non-cooperative).
        let _ = &token;
        std::thread::sleep(Duration::from_millis(500));
        42
    });
    assert_eq!(out, 42, "job must still complete and return its value");

    let logs = captured.lock().unwrap();
    assert!(
        logs.iter().any(|l| l.contains("overran watchdog limit")),
        "expected an overrun warning, got: {:?}",
        *logs
    );
}

/// T027 — the documented 30-second default limit is real in code (FR-011,
/// contracts/api.md §2): `Watchdog::default()` resolves to `DEFAULT_LIMIT`.
#[test]
fn default_limit_is_30_seconds() {
    assert_eq!(Watchdog::DEFAULT_LIMIT, Duration::from_secs(30));
    assert_eq!(Watchdog::default().limit(), Duration::from_secs(30));
}
