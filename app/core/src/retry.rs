// SPDX-License-Identifier: GPL-3.0-only
//! Retry helper for transient Windows file locks.
//!
//! Antivirus real-time scanning and the Windows Search indexer briefly open
//! audio files right after they are touched, so a rename/write can fail with
//! `ERROR_SHARING_VIOLATION` (os error 32) or `ERROR_LOCK_VIOLATION` (33) even
//! though nothing holds the file for long. Those are worth a short retry;
//! anything else (missing file, permission denied, cross-device) is returned
//! immediately.

use std::io;
use std::time::Duration;

/// Raw OS errors that a brief backoff usually clears.
fn is_transient_lock(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(32) | Some(33))
}

/// Run `op`, retrying only on a transient sharing/lock violation with an
/// increasing backoff (~50, 100, 200, 400 ms → total under a second). Returns
/// the last error if every attempt is still blocked.
pub fn on_lock<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    const BACKOFF_MS: [u64; 4] = [50, 100, 200, 400];
    let mut attempt = 0;
    loop {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) if is_transient_lock(&e) && attempt < BACKOFF_MS.len() => {
                std::thread::sleep(Duration::from_millis(BACKOFF_MS[attempt]));
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn succeeds_after_transient_failures() {
        let calls = Cell::new(0);
        let out = on_lock(|| {
            calls.set(calls.get() + 1);
            if calls.get() < 3 {
                Err(io::Error::from_raw_os_error(32))
            } else {
                Ok(calls.get())
            }
        })
        .unwrap();
        assert_eq!(out, 3, "retried until the lock cleared");
    }

    #[test]
    fn gives_up_after_max_attempts_and_returns_last_error() {
        let calls = Cell::new(0);
        let err = on_lock::<()>(|| {
            calls.set(calls.get() + 1);
            Err(io::Error::from_raw_os_error(32))
        })
        .unwrap_err();
        assert_eq!(err.raw_os_error(), Some(32));
        assert_eq!(calls.get(), 5, "1 initial try + 4 backoff retries");
    }

    #[test]
    fn does_not_retry_other_errors() {
        let calls = Cell::new(0);
        let err = on_lock::<()>(|| {
            calls.set(calls.get() + 1);
            Err(io::Error::new(io::ErrorKind::NotFound, "gone"))
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(calls.get(), 1, "non-lock errors fail fast");
    }
}
