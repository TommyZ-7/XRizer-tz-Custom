//! Lightweight, privacy-preserving AA diagnostics.
//!
//! Counters only: no image contents, no paths, no user identifiers. A summary
//! is logged every 300 frames.

use crate::upscaler::AntialiasMethod;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

#[derive(Default)]
pub struct AaDiagnostics {
    frames: AtomicU64,
    aa_frames: AtomicU64,
    fxaa: AtomicU64,
    cmaa2: AtomicU64,
    smaa: AtomicU64,
    fallbacks: AtomicU64,
    last_reason: Mutex<&'static str>,
}

static DIAGNOSTICS: LazyLock<AaDiagnostics> = LazyLock::new(AaDiagnostics::default);

pub fn record_frame(method: AntialiasMethod, enabled: bool) {
    DIAGNOSTICS.frames.fetch_add(1, Ordering::Relaxed);
    if enabled {
        DIAGNOSTICS.aa_frames.fetch_add(1, Ordering::Relaxed);
        match method {
            AntialiasMethod::Off => {}
            AntialiasMethod::Fxaa => {
                DIAGNOSTICS.fxaa.fetch_add(1, Ordering::Relaxed);
            }
            AntialiasMethod::Cmaa2 => {
                DIAGNOSTICS.cmaa2.fetch_add(1, Ordering::Relaxed);
            }
            AntialiasMethod::Smaa => {
                DIAGNOSTICS.smaa.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    let frames = DIAGNOSTICS.frames.load(Ordering::Relaxed);
    if frames.is_multiple_of(300) {
        log::info!("{}", summary());
    }
}

#[allow(dead_code)]
pub fn record_fallback(reason: &'static str) {
    DIAGNOSTICS.fallbacks.fetch_add(1, Ordering::Relaxed);
    *DIAGNOSTICS
        .last_reason
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = reason;
}

pub fn summary() -> String {
    format!(
        "aa: frames={} aa_frames={} fxaa={} cmaa2={} smaa={} fallbacks={} last_reason={} gpu_ms=unavailable",
        DIAGNOSTICS.frames.load(Ordering::Relaxed),
        DIAGNOSTICS.aa_frames.load(Ordering::Relaxed),
        DIAGNOSTICS.fxaa.load(Ordering::Relaxed),
        DIAGNOSTICS.cmaa2.load(Ordering::Relaxed),
        DIAGNOSTICS.smaa.load(Ordering::Relaxed),
        DIAGNOSTICS.fallbacks.load(Ordering::Relaxed),
        DIAGNOSTICS
            .last_reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_contains_counters_and_no_paths() {
        record_frame(AntialiasMethod::Fxaa, true);
        record_fallback("msaa");
        let summary = summary();
        assert!(summary.contains("frames="));
        assert!(summary.contains("fallbacks="));
        assert!(summary.contains("gpu_ms=unavailable"));
        assert!(!summary.contains('/'));
    }
}
