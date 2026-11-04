//! Experimental spatial upscaler configuration and spatial weighting.
//!
//! This module is deliberately independent of the graphics backend so the
//! continuous weight field, fallback selection, and hot-reload behaviour can be
//! tested on the CPU.

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
const COMPARE_INTERVAL_SECS: f32 = 2.0;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum UpscalerPreset {
    Fast,
    #[default]
    Balanced,
    Fsr,
    Ml,
}

impl UpscalerPreset {
    pub fn shader_kind(self) -> u32 {
        match self {
            Self::Fast => 0,
            Self::Balanced => 1,
            Self::Fsr => 2,
            Self::Ml => 3,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct UpscalerConfig {
    pub enabled: bool,
    pub preset: UpscalerPreset,
    /// Field centre in pre-distortion eye-texture UV space.
    pub center: [f32; 2],
    /// Full-weight radius.
    pub radius: f32,
    /// Width of the smooth falloff after `radius`.
    pub feather: f32,
    /// Scales enhancement (ML residual and sharpening), never the base resample.
    pub strength: f32,
    /// Output swapchain size multiplier over the runtime-recommended size.
    pub output_scale: f32,
    pub preserve_alpha: bool,
    /// Toggle enhancement on/off every two seconds for A/B comparison.
    pub compare: bool,
    /// Draw the full-weight contour at `radius`.
    pub show_region: bool,
    pub model_path: Option<PathBuf>,
}

impl Default for UpscalerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            preset: UpscalerPreset::Balanced,
            center: [0.5, 0.5],
            radius: 0.35,
            feather: 0.10,
            strength: 1.0,
            output_scale: 1.0,
            preserve_alpha: true,
            compare: false,
            show_region: false,
            model_path: None,
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
            radius: sanitize_finite(self.radius, default.radius).clamp(0.05, 2.0),
            feather: sanitize_finite(self.feather, default.feather).clamp(0.0, 1.0),
            strength: sanitize_finite(self.strength, default.strength).clamp(0.0, 3.0),
            output_scale: sanitize_finite(self.output_scale, default.output_scale).clamp(0.5, 2.0),
            ..self
        }
    }

    /// Build parameters for one frame. `compare_enabled` is the A/B toggle;
    /// output extent and base resampling never depend on it.
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
        let dst_width = ((base_width as f32 * config.output_scale).round() as u32).max(1);
        let dst_height = ((base_height as f32 * config.output_scale).round() as u32).max(1);

        Some(UpscalerParams {
            dst_width,
            dst_height,
            center: config.center,
            radius: config.radius,
            feather: config.feather,
            strength: config.strength,
            src_bounds: bounds,
            preset: config.preset,
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

    pub fn params(
        &self,
        base: (u32, u32),
        bounds: vr::VRTextureBounds_t,
    ) -> Option<UpscalerParams> {
        self.params_for_size(base.0, base.1, bounds)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct UpscalerParams {
    pub dst_width: u32,
    pub dst_height: u32,
    pub center: [f32; 2],
    pub radius: f32,
    pub feather: f32,
    pub strength: f32,
    pub src_bounds: vr::VRTextureBounds_t,
    pub preset: UpscalerPreset,
    pub preserve_alpha: bool,
    pub show_region: bool,
    pub enabled: bool,
}

impl PartialEq for UpscalerParams {
    fn eq(&self, other: &Self) -> bool {
        self.dst_width == other.dst_width
            && self.dst_height == other.dst_height
            && self.center == other.center
            && self.radius == other.radius
            && self.feather == other.feather
            && self.strength == other.strength
            && self.src_bounds.uMin == other.src_bounds.uMin
            && self.src_bounds.vMin == other.src_bounds.vMin
            && self.src_bounds.uMax == other.src_bounds.uMax
            && self.src_bounds.vMax == other.src_bounds.vMax
            && self.preset == other.preset
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

    pub fn weight_at(&self, uv: [f32; 2]) -> f32 {
        if !self.enabled {
            return 0.0;
        }
        let aspect = self.dst_width as f32 / self.dst_height as f32;
        math::weight_at(uv, self.center, self.radius, self.feather, aspect)
    }

    pub fn shader_kind(&self) -> u32 {
        self.preset.shader_kind()
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

    /// ML is the only preset with a runtime capability requirement. The output
    /// extent and base resample are identical for every preset.
    pub fn with_fallback(&self, ml_supported: bool) -> Self {
        let mut params = *self;
        if params.preset == UpscalerPreset::Ml && !ml_supported {
            params.preset = UpscalerPreset::Fsr;
        }
        params
    }

    /// Bounding box of pixels with `weight > epsilon`, in output pixels.
    pub fn ml_bounds(&self, epsilon: f32) -> (u32, u32, u32, u32) {
        math::ml_bounds(
            self.dst_width,
            self.dst_height,
            self.center,
            self.radius,
            self.feather,
            epsilon,
        )
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

/// The upscaler runs only on single-sample frames and only on backends that
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

/// A/B state. When `compare` is off this is always true (enhancement on).
pub fn compare_enabled() -> bool {
    let compare = CONFIG
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .compare;
    if !compare {
        return true;
    }
    let elapsed = COMPARE_EPOCH.elapsed().as_secs_f32();
    elapsed % (COMPARE_INTERVAL_SECS * 2.0) < COMPARE_INTERVAL_SECS
}

#[cfg(test)]
pub fn force_reload_for_test() {
    LAST_POLL_MS.store(0, Ordering::Relaxed);
    maybe_reload();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_bounds() -> vr::VRTextureBounds_t {
        vr::VRTextureBounds_t {
            uMin: 0.0,
            vMin: 0.0,
            uMax: 1.0,
            vMax: 1.0,
        }
    }

    #[test]
    fn default_is_disabled() {
        let c = UpscalerConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.preset, UpscalerPreset::Balanced);
        assert_eq!(c.center, [0.5, 0.5]);
        assert_eq!(c.radius, 0.35);
        assert_eq!(c.feather, 0.10);
        assert_eq!(c.strength, 1.0);
        assert_eq!(c.output_scale, 1.0);
        assert!(c.preserve_alpha);
        assert!(!c.compare);
        assert!(!c.show_region);
        assert_eq!(c.model_path, None);
    }

    #[test]
    fn sanitize_clamps() {
        let c = UpscalerConfig {
            center: [-1.0, 2.0],
            radius: 0.0,
            feather: 9.0,
            strength: 99.0,
            output_scale: 9.0,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(c.center, [0.0, 1.0]);
        assert_eq!(c.radius, 0.05);
        assert_eq!(c.feather, 1.0);
        assert_eq!(c.strength, 3.0);
        assert_eq!(c.output_scale, 2.0);
    }

    #[test]
    fn params_none_when_disabled() {
        let c = UpscalerConfig::default();
        assert!(c.params_for_size(1000, 800, full_bounds()).is_none());
    }

    #[test]
    fn params_output_size_uses_scale() {
        let b = full_bounds();
        let c = UpscalerConfig {
            enabled: true,
            output_scale: 0.7,
            ..Default::default()
        };
        let p = c.params_for_size(1000, 800, b).unwrap();
        assert_eq!((p.dst_width, p.dst_height), (700, 560));

        let p = c.params((2000, 1000), b).unwrap();
        assert_eq!((p.dst_width, p.dst_height), (1400, 700));
    }

    #[test]
    fn source_uv_respects_bounds_and_flip() {
        let b = vr::VRTextureBounds_t {
            uMin: 0.1,
            vMin: 0.9,
            uMax: 0.9,
            vMax: 0.1,
        };
        let c = UpscalerConfig {
            enabled: true,
            ..Default::default()
        };
        let p = c.params_for_size(100, 100, b).unwrap();
        assert_eq!(p.source_uv([0.0, 0.0]), [0.1, 0.9]);
        assert_eq!(p.source_uv([1.0, 1.0]), [0.9, 0.1]);
        assert_eq!(p.source_uv([0.5, 0.5]), [0.5, 0.5]);
    }

    #[test]
    fn weight_field_center_radius_feather() {
        let b = full_bounds();
        let c = UpscalerConfig {
            enabled: true,
            center: [0.5, 0.5],
            radius: 0.2,
            feather: 0.1,
            ..Default::default()
        };
        let p = c.params_for_size(100, 100, b).unwrap();
        assert_eq!(p.weight_at([0.5, 0.5]), 1.0);
        assert_eq!(p.weight_at([0.7, 0.5]), 1.0);
        assert!((0.0..1.0).contains(&p.weight_at([0.75, 0.5])));
        assert_eq!(p.weight_at([0.9, 0.5]), 0.0);
    }

    #[test]
    fn weight_field_hard_step_when_feather_is_zero() {
        let b = full_bounds();
        let c = UpscalerConfig {
            enabled: true,
            center: [0.5, 0.5],
            radius: 0.2,
            feather: 0.0,
            ..Default::default()
        };
        let p = c.params_for_size(100, 100, b).unwrap();
        assert_eq!(p.weight_at([0.7, 0.5]), 1.0);
        assert_eq!(p.weight_at([0.7002, 0.5]), 0.0);
    }

    #[test]
    fn large_radius_makes_weight_one_everywhere() {
        let b = full_bounds();
        let c = UpscalerConfig {
            enabled: true,
            radius: 2.0,
            feather: 0.0,
            ..Default::default()
        };
        let p = c.params_for_size(100, 100, b).unwrap();
        assert_eq!(p.weight_at([0.0, 0.0]), 1.0);
        assert_eq!(p.weight_at([1.0, 1.0]), 1.0);
        assert_eq!(p.weight_at([0.5, 0.5]), 1.0);
    }

    #[test]
    fn center_offset_moves_weight() {
        let b = full_bounds();
        let c = UpscalerConfig {
            enabled: true,
            center: [0.25, 0.75],
            radius: 0.1,
            feather: 0.0,
            ..Default::default()
        };
        let p = c.params_for_size(100, 100, b).unwrap();
        assert_eq!(p.weight_at([0.25, 0.75]), 1.0);
        assert_eq!(p.weight_at([0.5, 0.5]), 0.0);
    }

    #[test]
    fn weight_uses_output_aspect() {
        let b = full_bounds();
        let c = UpscalerConfig {
            enabled: true,
            center: [0.5, 0.5],
            radius: 0.2,
            feather: 0.0,
            ..Default::default()
        };
        let p = c.params_for_size(200, 100, b).unwrap();
        assert_eq!(p.weight_at([0.59, 0.5]), 1.0);
        assert_eq!(p.weight_at([0.61, 0.5]), 0.0);
    }

    #[test]
    fn ml_bounds_follow_weight_region() {
        let b = full_bounds();
        let c = UpscalerConfig {
            enabled: true,
            radius: 0.2,
            feather: 0.1,
            ..Default::default()
        };
        let p = c.params_for_size(1000, 1000, b).unwrap();
        let (x, y, w, h) = p.ml_bounds(0.001);
        assert!(x > 0 && y > 0 && w < 1000 && h < 1000);
        assert!(x + w <= 1000 && y + h <= 1000);
        assert_eq!((x, y, w, h), (200, 200, 600, 600));
    }

    #[test]
    fn ml_bounds_full_frame_when_radius_is_large() {
        let b = full_bounds();
        let c = UpscalerConfig {
            enabled: true,
            radius: 2.0,
            feather: 0.0,
            ..Default::default()
        };
        let p = c.params_for_size(640, 480, b).unwrap();
        assert_eq!(p.ml_bounds(0.001), (0, 0, 640, 480));
    }

    #[test]
    fn preset_maps_to_shader_kind() {
        let b = full_bounds();
        for (preset, kind) in [
            (UpscalerPreset::Fast, 0),
            (UpscalerPreset::Balanced, 1),
            (UpscalerPreset::Fsr, 2),
            (UpscalerPreset::Ml, 3),
        ] {
            let c = UpscalerConfig {
                enabled: true,
                preset,
                ..Default::default()
            };
            assert_eq!(c.params_for_size(100, 100, b).unwrap().shader_kind(), kind);
        }
    }

    #[test]
    fn flags_encode_alpha_region_and_enhancement() {
        let b = full_bounds();
        let c = UpscalerConfig {
            enabled: true,
            preserve_alpha: true,
            show_region: true,
            strength: 0.5,
            ..Default::default()
        };
        let p = c.params_for_size(100, 100, b).unwrap();
        assert_eq!(p.flags() & FLAG_PRESERVE_ALPHA, FLAG_PRESERVE_ALPHA);
        assert_eq!(p.flags() & FLAG_SHOW_REGION, FLAG_SHOW_REGION);
        assert_eq!(
            p.flags() & FLAG_ENHANCEMENT_ENABLED,
            FLAG_ENHANCEMENT_ENABLED
        );

        let p = UpscalerConfig {
            enabled: true,
            strength: 0.0,
            ..Default::default()
        }
        .params_for_size(100, 100, b)
        .unwrap();
        assert_eq!(p.flags() & FLAG_ENHANCEMENT_ENABLED, 0);
    }

    #[test]
    fn ml_falls_back_to_fsr_only_when_unsupported() {
        let b = full_bounds();
        let c = UpscalerConfig {
            enabled: true,
            preset: UpscalerPreset::Ml,
            ..Default::default()
        };
        let p = c.params_for_size(100, 100, b).unwrap();
        assert_eq!(p.with_fallback(true).preset, UpscalerPreset::Ml);
        assert_eq!(p.with_fallback(false).preset, UpscalerPreset::Fsr);

        let fast = UpscalerConfig {
            enabled: true,
            preset: UpscalerPreset::Fast,
            ..Default::default()
        }
        .params_for_size(100, 100, b)
        .unwrap();
        assert_eq!(fast.with_fallback(false).preset, UpscalerPreset::Fast);
    }

    #[test]
    fn compare_does_not_change_output_extent() {
        let b = full_bounds();
        let c = UpscalerConfig {
            enabled: true,
            compare: true,
            output_scale: 0.8,
            ..Default::default()
        };
        let on = c.params_for_size_at(1000, 800, b, true).unwrap();
        let off = c.params_for_size_at(1000, 800, b, false).unwrap();
        assert_eq!((on.dst_width, on.dst_height), (800, 640));
        assert_eq!((off.dst_width, off.dst_height), (800, 640));
        assert_ne!(on.enabled, off.enabled);
    }

    #[test]
    fn params_uses_recommended_size_and_output_scale() {
        let b = full_bounds();
        let c = UpscalerConfig {
            enabled: true,
            output_scale: 0.8,
            ..Default::default()
        };
        let p = c.params_for_size(2000, 1600, b).unwrap();
        assert_eq!((p.dst_width, p.dst_height), (1600, 1280));
    }

    #[test]
    fn preserve_alpha_is_in_flags() {
        let b = full_bounds();
        let p = UpscalerConfig {
            enabled: true,
            preserve_alpha: true,
            ..Default::default()
        }
        .params_for_size(100, 100, b)
        .unwrap();
        assert_eq!(p.flags() & FLAG_PRESERVE_ALPHA, FLAG_PRESERVE_ALPHA);
    }

    #[test]
    fn upscaler_requires_single_sample() {
        assert!(upscaler_allowed(true, 1));
        assert!(!upscaler_allowed(true, 2));
        assert!(!upscaler_allowed(true, 4));
        assert!(!upscaler_allowed(false, 1));
    }

    #[test]
    fn json_roundtrip_and_bad_json_is_error() {
        let c = UpscalerConfig {
            enabled: true,
            preset: UpscalerPreset::Fsr,
            center: [0.4, 0.6],
            radius: 0.5,
            feather: 0.2,
            strength: 1.5,
            output_scale: 0.8,
            preserve_alpha: false,
            compare: true,
            show_region: true,
            model_path: Some("/tmp/model.xru".into()),
        };
        let json = serde_json::to_string(&c).unwrap();
        let parsed: UpscalerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, c);

        assert!(serde_json::from_str::<UpscalerConfig>("not json").is_err());
        assert!(serde_json::from_str::<UpscalerConfig>("{\"preset\":\"nope\"}").is_err());
    }

    #[test]
    fn missing_fields_use_defaults() {
        let c: UpscalerConfig = serde_json::from_str("{\"enabled\":true}").unwrap();
        assert!(c.enabled);
        assert_eq!(c.preset, UpscalerPreset::Balanced);
        assert_eq!(c.radius, 0.35);
    }

    #[test]
    fn candidate_paths_honour_explicit_override() {
        let _guard = test_lock();
        let path = std::env::temp_dir().join("xrizer_upscaler_candidate_test.json");
        unsafe { std::env::set_var("XRIZER_UPSCALER_CONFIG", &path) };
        let paths = candidate_paths();
        assert_eq!(paths.first(), Some(&path));
        unsafe { std::env::remove_var("XRIZER_UPSCALER_CONFIG") };
    }

    #[test]
    fn reload_keeps_previous_config_on_bad_json() {
        let _guard = test_lock();
        unsafe { std::env::set_var("XRIZER_TEST_FILE_POLL", "1") };
        let path = std::env::temp_dir().join("xrizer_upscaler_reload_test.json");
        unsafe { std::env::set_var("XRIZER_UPSCALER_CONFIG", &path) };

        std::fs::write(&path, "{\"enabled\":true,\"radius\":0.5}").unwrap();
        reset_for_test();
        maybe_reload();
        let good = snapshot();
        assert!(good.enabled);
        assert_eq!(good.radius, 0.5);

        std::fs::write(&path, "{ definitely not json").unwrap();
        force_reload_for_test();
        let kept = snapshot();
        assert!(kept.enabled);
        assert_eq!(kept.radius, 0.5);

        let _ = std::fs::remove_file(&path);
        unsafe { std::env::remove_var("XRIZER_UPSCALER_CONFIG") };
        unsafe { std::env::remove_var("XRIZER_TEST_FILE_POLL") };
        reset_for_test();
    }

    #[test]
    fn config_file_is_sanitized_on_load() {
        let _guard = test_lock();
        let path = std::env::temp_dir().join("xrizer_upscaler_sanitize_test.json");
        std::fs::write(&path, "{\"radius\":99,\"feather\":-1,\"strength\":-2}").unwrap();
        let c = load_from_path(&path).unwrap();
        assert_eq!(c.radius, 2.0);
        assert_eq!(c.feather, 0.0);
        assert_eq!(c.strength, 0.0);
        let _ = std::fs::remove_file(&path);
    }
}
