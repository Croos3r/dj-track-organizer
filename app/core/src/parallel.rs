// SPDX-License-Identifier: GPL-3.0-only
//! Small shared helper for running rayon work on a bounded pool.

/// Run `f` on a rayon pool: the global pool for `parallelism == 0` (all cores),
/// or a fresh pool capped to `parallelism` threads otherwise. Callers use
/// `parallelism == 1` to mean "plain sequential" and skip this entirely.
pub(crate) fn run_in_pool<T: Send>(parallelism: usize, f: impl FnOnce() -> T + Send) -> T {
    if parallelism == 0 {
        f()
    } else {
        rayon::ThreadPoolBuilder::new()
            .num_threads(parallelism)
            .build()
            .expect("build rayon pool")
            .install(f)
    }
}
