//! Post-process antialiasing configuration and spatial weighting.
//!
//! The module keeps the historical `upscaler.json` file name and candidate
//! paths, but the settings are AA-only. The graphics backend consumes an
//! equal-size [`UpscalerParams`] and never resizes the submitted image.

pub mod auto_scale;
pub mod diagnostics;
pub mod math;

use openvr as vr;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const FLAG_PRESERVE_ALPHA: u32 = 1 << 0;
pub const FLAG_SHOW_REGION: u32 = 1 << 1;
pub const FLAG_ENHANCEMENT_ENABLED: u32 = 1 << 2;

const RELOAD_INTERVAL_MS: u64 = 500;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AntialiasMethod {
    Off,
    #[default]
    Cmaa2,
    Smaa,
    Fxaa,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum WeightMode {
    Center,
    #[default]
    Inverted,
    Whole,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum FxaaQuality {
    Performance,
    Balanced,
    #[default]
    Quality,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct UpscalerConfig {
    pub enabled: bool,
    pub antialias: AntialiasMethod,
    pub weight_mode: WeightMode,
    /// Field centre in pre-distortion eye-texture UV space.
    pub center: [f32; 2],
    /// Per-eye x correction: left eye uses `-offset`, right eye `+offset`.
    pub center_offset: f32,
    /// Inner boundary of the weight field.
    pub radius: f32,
    /// Width of the transition outside `radius`.
    pub feather: f32,
    /// Blend of the AA result with the original image.
    pub strength: f32,
    pub preserve_alpha: bool,
    pub fxaa_quality: FxaaQuality,
    pub smaa_threshold: f32,
    /// Toggle AA on/off every `compare_interval_ms` for A/B comparison.
    pub compare: bool,
    pub compare_interval_ms: u64,
    /// Draw the `radius` contour for `center` and `inverted`.
    pub show_region: bool,
    /// Retained render-scale control; not part of the AA pass.
    pub auto_scale: bool,
    pub min_scale: f32,
    pub max_scale: f32,
}

impl Default for UpscalerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            antialias: AntialiasMethod::Cmaa2,
            weight_mode: WeightMode::Inverted,
            center: [0.5, 0.5],
            center_offset: 0.0,
            radius: 0.35,
            feather: 0.10,
            strength: 1.0,
            preserve_alpha: true,
            fxaa_quality: FxaaQuality::Quality,
            smaa_threshold: 0.1,
            compare: false,
            compare_interval_ms: 2000,
            show_region: false,
            auto_scale: false,
            min_scale: 0.5,
            max_scale: 1.5,
        }
    }
}

impl UpscalerConfig {
    /// Clamp every numeric field into its documented range. Non-finite values
    /// fall back to the default for that field so a malformed config cannot
    /// produce NaNs in the weight field.
    pub fn sanitized(self) -> Self {
        let default = Self::default();
        Self {
            center: [
                sanitize_finite(self.center[0], default.center[0]).clamp(0.0, 1.0),
                sanitize_finite(self.center[1], default.center[1]).clamp(0.0, 1.0),
            ],
            center_offset: sanitize_finite(self.center_offset, default.center_offset)
                .clamp(-0.20, 0.20),
            radius: sanitize_finite(self.radius, default.radius).clamp(0.0, 1.0),
            feather: sanitize_finite(self.feather, default.feather).clamp(0.0, 1.0),
            strength: sanitize_finite(self.strength, default.strength).clamp(0.0, 1.0),
            smaa_threshold: sanitize_finite(self.smaa_threshold, default.smaa_threshold)
                .clamp(0.05, 0.2),
            compare_interval_ms: self.compare_interval_ms.clamp(500, 10_000),
            min_scale: sanitize_finite(self.min_scale, default.min_scale).clamp(0.1, 4.0),
            max_scale: sanitize_finite(self.max_scale, default.max_scale).clamp(0.1, 4.0),
            ..self
        }
        .normalize_scale_bounds()
    }

    fn normalize_scale_bounds(mut self) -> Self {
        if self.min_scale > self.max_scale {
            std::mem::swap(&mut self.min_scale, &mut self.max_scale);
        }
        self
    }

    pub fn auto_scale_config(&self) -> crate::upscaler::auto_scale::AutoScaleConfig {
        crate::upscaler::auto_scale::AutoScaleConfig {
            min_scale: self.min_scale,
            max_scale: self.max_scale,
            ..Default::default()
        }
    }

    /// Build parameters for one frame. The output extent is always the input
    /// extent; `compare_enabled` only toggles the AA effect.
    pub fn params_for_size_at(
        &self,
        base_width: u32,
        base_height: u32,
        bounds: vr::VRTextureBounds_t,
        compare_enabled: bool,
    ) -> Option<UpscalerParams> {
        if !self.enabled {
            return None;
        }

        let config = self.clone().sanitized();

        Some(UpscalerParams {
            dst_width: base_width,
            dst_height: base_height,
            center: config.center,
            center_offset: config.center_offset,
            radius: config.radius,
            feather: config.feather,
            strength: config.strength,
            src_bounds: bounds,
            antialias: config.antialias,
            weight_mode: config.weight_mode,
            preserve_alpha: config.preserve_alpha,
            show_region: config.show_region,
            enabled: compare_enabled,
        })
    }

    pub fn params_for_size(
        &self,
        base_width: u32,
        base_height: u32,
        bounds: vr::VRTextureBounds_t,
    ) -> Option<UpscalerParams> {
        self.params_for_size_at(base_width, base_height, bounds, compare_enabled())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct UpscalerParams {
    pub dst_width: u32,
    pub dst_height: u32,
    pub center: [f32; 2],
    pub center_offset: f32,
    pub radius: f32,
    pub feather: f32,
    pub strength: f32,
    pub src_bounds: vr::VRTextureBounds_t,
    pub antialias: AntialiasMethod,
    pub weight_mode: WeightMode,
    pub preserve_alpha: bool,
    pub show_region: bool,
    pub enabled: bool,
}

impl PartialEq for UpscalerParams {
    fn eq(&self, other: &Self) -> bool {
        self.dst_width == other.dst_width
            && self.dst_height == other.dst_height
            && self.center == other.center
            && self.center_offset == other.center_offset
            && self.radius == other.radius
            && self.feather == other.feather
            && self.strength == other.strength
            && self.src_bounds.uMin == other.src_bounds.uMin
            && self.src_bounds.vMin == other.src_bounds.vMin
            && self.src_bounds.uMax == other.src_bounds.uMax
            && self.src_bounds.vMax == other.src_bounds.vMax
            && self.antialias == other.antialias
            && self.weight_mode == other.weight_mode
            && self.preserve_alpha == other.preserve_alpha
            && self.show_region == other.show_region
            && self.enabled == other.enabled
    }
}

impl UpscalerParams {
    /// Map destination UV to source texture UV, honouring the submitted
    /// bounds (including vertically flipped submissions).
    pub fn source_uv(&self, uv: [f32; 2]) -> [f32; 2] {
        // Lerp form keeps the endpoints exactly at the submitted bounds even
        // for non-power-of-two values like 0.1.
        [
            self.src_bounds.uMin * (1.0 - uv[0]) + self.src_bounds.uMax * uv[0],
            self.src_bounds.vMin * (1.0 - uv[1]) + self.src_bounds.vMax * uv[1],
        ]
    }

    pub fn center_for_eye(&self, eye: usize) -> [f32; 2] {
        let offset = if eye == 0 {
            -self.center_offset
        } else {
            self.center_offset
        };
        [(self.center[0] + offset).clamp(0.0, 1.0), self.center[1]]
    }

    pub fn weight_at(&self, uv: [f32; 2]) -> f32 {
        self.weight_at_center(uv, self.center)
    }

    pub fn weight_at_for_eye(&self, uv: [f32; 2], eye: usize) -> f32 {
        self.weight_at_center(uv, self.center_for_eye(eye))
    }

    fn weight_at_center(&self, uv: [f32; 2], center: [f32; 2]) -> f32 {
        if !self.enabled {
            return 0.0;
        }
        let aspect = self.dst_width as f32 / self.dst_height as f32;
        math::weight_at_mode(
            uv,
            center,
            self.radius,
            self.feather,
            aspect,
            self.weight_mode,
        )
    }

    pub fn flags(&self) -> u32 {
        let mut flags = 0;
        if self.preserve_alpha {
            flags |= FLAG_PRESERVE_ALPHA;
        }
        if self.show_region {
            flags |= FLAG_SHOW_REGION;
        }
        if self.enabled && self.strength > 0.0 {
            flags |= FLAG_ENHANCEMENT_ENABLED;
        }
        flags
    }

    /// SGSR/FSR used flipped bounds as a fallback condition. The AA pass has
    /// no such restriction, but callers still use this to preserve bounds
    /// handling in tests and diagnostics.
    pub fn has_flipped_bounds(&self) -> bool {
        self.src_bounds.uMin > self.src_bounds.uMax || self.src_bounds.vMin > self.src_bounds.vMax
    }
}

fn sanitize_finite(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

static CONFIG: LazyLock<RwLock<UpscalerConfig>> =
    LazyLock::new(|| RwLock::new(UpscalerConfig::default()));
static LAST_POLL_MS: AtomicU64 = AtomicU64::new(0);
static COMPARE_EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

#[cfg(test)]
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
pub fn reset_for_test() {
    *CONFIG
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = UpscalerConfig::default();
    LAST_POLL_MS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
pub fn set_for_test(config: UpscalerConfig) {
    *CONFIG
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = config.sanitized();
}

/// Atomic-write friendly config locations, in priority order.
pub fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(path) = std::env::var("XRIZER_UPSCALER_CONFIG") {
        paths.push(PathBuf::from(path));
    }
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(dir).join("xrizer/upscaler.json"));
    }
    if let Ok(home) = std::env::var("HOME") {
        paths.push(PathBuf::from(home).join(".config/xrizer/upscaler.json"));
    }
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        paths.push(PathBuf::from(dir).join("xrizer_upscaler.json"));
    }
    paths.push(PathBuf::from("/tmp/xrizer_upscaler.json"));
    paths.dedup();
    paths
}

pub fn load_from_path(path: &Path) -> Result<UpscalerConfig, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let config: UpscalerConfig = serde_json::from_str(&text)
        .map_err(|error| format!("invalid upscaler config {}: {error}", path.display()))?;
    Ok(config.sanitized())
}

/// Poll the config file at most every 500 ms. A parse failure keeps the last
/// good configuration.
pub fn maybe_reload() {
    if cfg!(test) && std::env::var("XRIZER_TEST_FILE_POLL").is_err() {
        return;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let last = LAST_POLL_MS.load(Ordering::Relaxed);
    if now.wrapping_sub(last) < RELOAD_INTERVAL_MS {
        return;
    }
    LAST_POLL_MS.store(now, Ordering::Relaxed);

    for path in candidate_paths() {
        if !path.exists() {
            continue;
        }
        match load_from_path(&path) {
            Ok(config) => {
                *CONFIG
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = config;
            }
            Err(error) => {
                log::warn!("{error}; keeping previous upscaler config");
            }
        }
        // Only the highest-priority existing file is considered.
        return;
    }
}

/// The AA pass runs only on single-sample frames and only on backends that
/// opted in. MSAA submissions take the existing copy/resolve path unchanged.
pub fn upscaler_allowed(supports_upscaler: bool, sample_count: u32) -> bool {
    supports_upscaler && sample_count == 1
}

pub fn snapshot() -> UpscalerConfig {
    maybe_reload();
    CONFIG
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// A/B phase for a given elapsed time and interval. Even phases are AA-on.
pub fn compare_phase(elapsed_ms: u64, interval_ms: u64) -> bool {
    if interval_ms == 0 {
        return true;
    }
    (elapsed_ms / interval_ms).is_multiple_of(2)
}

/// A/B state. When `compare` is off this is always true (AA on).
pub fn compare_enabled() -> bool {
    let config = CONFIG
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !config.compare {
        return true;
    }
    let interval = config.compare_interval_ms;
    drop(config);
    compare_phase(COMPARE_EPOCH.elapsed().as_millis() as u64, interval)
}

#[cfg(test)]
pub fn force_reload_for_test() {
    LAST_POLL_MS.store(0, Ordering::Relaxed);
    maybe_reload();
}

#[cfg(test)]
mod aa_config_tests;
