//! Lightweight, privacy-preserving upscaler diagnostics.
//!
//! Counters only: no image contents, no paths, no user identifiers. A summary
//! is logged every 300 frames.

use crate::upscaler::UpscalerPreset;
use crate::upscaler::model::ModelKind;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

#[derive(Default)]
pub struct UpscalerDiagnostics {
    frames: AtomicU64,
    upscaled: AtomicU64,
    fsr: AtomicU64,
    sgsr: AtomicU64,
    fast: AtomicU64,
    mlp: AtomicU64,
    residual_cnn: AtomicU64,
    srpo: AtomicU64,
    fallbacks: AtomicU64,
    last_reason: Mutex<&'static str>,
}

static DIAGNOSTICS: LazyLock<UpscalerDiagnostics> = LazyLock::new(UpscalerDiagnostics::default);

pub fn record_frame(preset: UpscalerPreset, enabled: bool) {
    DIAGNOSTICS.frames.fetch_add(1, Ordering::Relaxed);
    if enabled {
        DIAGNOSTICS.upscaled.fetch_add(1, Ordering::Relaxed);
        match preset {
            UpscalerPreset::Fast => DIAGNOSTICS.fast.fetch_add(1, Ordering::Relaxed),
            UpscalerPreset::Balanced => DIAGNOSTICS.sgsr.fetch_add(1, Ordering::Relaxed),
            UpscalerPreset::Fsr => DIAGNOSTICS.fsr.fetch_add(1, Ordering::Relaxed),
            UpscalerPreset::Ml => DIAGNOSTICS.mlp.fetch_add(1, Ordering::Relaxed),
        };
    }
    let frames = DIAGNOSTICS.frames.load(Ordering::Relaxed);
    if frames.is_multiple_of(300) {
        log::info!("{}", summary());
    }
}

pub fn record_ml_model(kind: ModelKind) {
    match kind {
        ModelKind::Mlp => DIAGNOSTICS.mlp.fetch_add(1, Ordering::Relaxed),
        ModelKind::ResidualCnn => DIAGNOSTICS.residual_cnn.fetch_add(1, Ordering::Relaxed),
        ModelKind::Offset => DIAGNOSTICS.srpo.fetch_add(1, Ordering::Relaxed),
    };
}

pub fn record_fallback(reason: &'static str) {
    DIAGNOSTICS.fallbacks.fetch_add(1, Ordering::Relaxed);
    *DIAGNOSTICS
        .last_reason
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = reason;
}

pub fn summary() -> String {
    format!(
        "upscaler: frames={} upscaled={} fast={} sgsr={} fsr={} mlp={} cnn={} srpo={} fallbacks={} last_reason={} gpu_ms=unavailable",
        DIAGNOSTICS.frames.load(Ordering::Relaxed),
        DIAGNOSTICS.upscaled.load(Ordering::Relaxed),
        DIAGNOSTICS.fast.load(Ordering::Relaxed),
        DIAGNOSTICS.sgsr.load(Ordering::Relaxed),
        DIAGNOSTICS.fsr.load(Ordering::Relaxed),
        DIAGNOSTICS.mlp.load(Ordering::Relaxed),
        DIAGNOSTICS.residual_cnn.load(Ordering::Relaxed),
        DIAGNOSTICS.srpo.load(Ordering::Relaxed),
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
        record_frame(UpscalerPreset::Fsr, true);
        record_ml_model(ModelKind::ResidualCnn);
        record_fallback("msaa");
        let summary = summary();
        assert!(summary.contains("frames="));
        assert!(summary.contains("fallbacks="));
        assert!(summary.contains("gpu_ms=unavailable"));
        assert!(!summary.contains('/'));
    }
}
