//! CPU reference math for the spatial upscaler.

pub const MLP_FEATURE_COUNT: usize = 28;

/// GLSL-compatible smoothstep. `edge0 > edge1` produces a descending ramp.
/// A zero-width edge is treated as a hard step: below is 1, above is 0, and
/// exactly on the edge is 0.5.
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge0 == edge1 {
        return if x < edge0 {
            1.0
        } else if x > edge0 {
            0.0
        } else {
            0.5
        };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Continuous radial weight in aspect-corrected UV space.
pub fn weight_at(
    uv: [f32; 2],
    center: [f32; 2],
    radius: f32,
    feather: f32,
    aspect: f32,
) -> f32 {
    let dx = (uv[0] - center[0]) * aspect;
    let dy = uv[1] - center[1];
    let distance = (dx * dx + dy * dy).sqrt();
    if feather <= 0.0 {
        return if distance <= radius { 1.0 } else { 0.0 };
    }
    1.0 - smoothstep(radius, radius + feather, distance)
}

/// Flatten the 5x5 luma patch plus sub-pixel phase and scale into the 28
/// features consumed by the per-pixel MLP baseline.
pub fn extract_luma_features(
    patch: &[f32; 25],
    phase: [f32; 2],
    scale: f32,
) -> [f32; MLP_FEATURE_COUNT] {
    let mut features = [0.0; MLP_FEATURE_COUNT];
    features[..25].copy_from_slice(patch);
    features[25] = phase[0];
    features[26] = phase[1];
    features[27] = scale;
    features
}

/// Symmetric residual clamp. `NaN` residuals become zero so a broken model
/// cannot poison the base image.
pub fn residual_clamp(residual: f32, limit: f32) -> f32 {
    if residual.is_nan() {
        return 0.0;
    }
    let limit = if limit.is_nan() { 0.0 } else { limit.abs() };
    residual.clamp(-limit, limit)
}

/// Clamp a residual to a fraction of the local luma range. Flat areas allow
/// no residual at all, which keeps gradients and UI backgrounds stable.
pub fn local_residual_clamp(
    residual: f32,
    local_min: f32,
    local_max: f32,
    fraction: f32,
) -> f32 {
    let range = (local_max - local_min).abs();
    residual_clamp(residual, range * fraction.abs())
}

/// Bounding box of pixels with `weight > epsilon`, in output pixels.
///
/// The returned rectangle is clamped to the frame. A field that covers the
/// whole frame returns the whole extent, which lets callers dispatch the ML
/// pass over everything without a special case.
pub fn ml_bounds(
    dst_width: u32,
    dst_height: u32,
    center: [f32; 2],
    radius: f32,
    feather: f32,
    epsilon: f32,
) -> (u32, u32, u32, u32) {
    let effective_radius = radius + feather;
    if effective_radius <= epsilon || dst_width == 0 || dst_height == 0 {
        return (0, 0, 0, 0);
    }

    let aspect = dst_width as f32 / dst_height as f32;
    let half_u = effective_radius / aspect;
    let half_v = effective_radius;

    // Absorb float representation error at exact pixel boundaries (for
    // example 0.5 - 0.3 landing just below 0.2).
    const PIXEL_NUDGE: f32 = 1e-3;
    let x0 = ((center[0] - half_u) * dst_width as f32 + PIXEL_NUDGE).floor();
    let x1 = ((center[0] + half_u) * dst_width as f32 - PIXEL_NUDGE).ceil();
    let y0 = ((center[1] - half_v) * dst_height as f32 + PIXEL_NUDGE).floor();
    let y1 = ((center[1] + half_v) * dst_height as f32 - PIXEL_NUDGE).ceil();

    let x0 = x0.clamp(0.0, dst_width as f32) as u32;
    let x1 = x1.clamp(0.0, dst_width as f32) as u32;
    let y0 = y0.clamp(0.0, dst_height as f32) as u32;
    let y1 = y1.clamp(0.0, dst_height as f32) as u32;

    (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luma_patch_has_28_features() {
        let patch = [
            0.0, 0.1, 0.2, 0.3, 0.4, //
            0.5, 0.6, 0.7, 0.8, 0.9, //
            1.0, 0.9, 0.8, 0.7, 0.6, //
            0.5, 0.4, 0.3, 0.2, 0.1, //
            0.0, 0.1, 0.2, 0.3, 0.4,
        ];
        let features = extract_luma_features(&patch, [0.25, -0.5], 1.5);
        assert_eq!(features.len(), MLP_FEATURE_COUNT);
        assert_eq!(features[..25], patch);
        assert_eq!(features[25], 0.25);
        assert_eq!(features[26], -0.5);
        assert_eq!(features[27], 1.5);
    }

    #[test]
    fn residual_clamp_is_symmetric_and_bounded() {
        assert_eq!(residual_clamp(0.5, 0.2), 0.2);
        assert_eq!(residual_clamp(-0.5, 0.2), -0.2);
        assert_eq!(residual_clamp(0.1, 0.2), 0.1);
        assert_eq!(residual_clamp(0.5, f32::INFINITY), 0.5);
        assert_eq!(residual_clamp(f32::NAN, 0.2), 0.0);
    }

    #[test]
    fn local_residual_clamp_respects_local_contrast() {
        assert_eq!(local_residual_clamp(0.1, 0.5, 0.5, 0.2), 0.0);
        assert_eq!(local_residual_clamp(0.1, 0.0, 1.0, 0.2), 0.1);
        assert_eq!(local_residual_clamp(0.5, 0.0, 1.0, 0.2), 0.2);
        assert_eq!(local_residual_clamp(-0.5, 0.0, 1.0, 0.2), -0.2);
    }

    #[test]
    fn smoothstep_matches_glsl_formula() {
        for x in [-1.0f32, 0.0, 0.25, 0.5, 0.75, 1.0, 2.0] {
            let t = x.clamp(0.0, 1.0);
            let expected = t * t * (3.0 - 2.0 * t);
            assert!((smoothstep(0.0, 1.0, x) - expected).abs() < 1e-6);
        }
        assert_eq!(smoothstep(0.4, 0.2, 0.3), 1.0 - smoothstep(0.2, 0.4, 0.3));
        assert_eq!(smoothstep(0.5, 0.5, 0.4), 1.0);
        assert_eq!(smoothstep(0.5, 0.5, 0.5), 0.5);
        assert_eq!(smoothstep(0.5, 0.5, 0.6), 0.0);
    }

    #[test]
    fn weight_field_is_smooth_and_reaches_zero() {
        let center = [0.5, 0.5];
        let radius = 0.2;
        let feather = 0.1;
        let aspect = 1.0;
        let mut previous = 1.0;
        for step in 0..=100 {
            let d = step as f32 / 100.0;
            let w = weight_at([0.5 + d, 0.5], center, radius, feather, aspect);
            assert!((0.0..=1.0).contains(&w));
            assert!(w <= previous + 1e-6, "weight must not increase with distance");
            previous = w;
        }
        assert_eq!(
            weight_at([0.9, 0.5], center, radius, feather, aspect),
            0.0
        );
    }

    #[test]
    fn ml_bounds_round_and_clamp() {
        let (x, y, w, h) = ml_bounds(1000, 1000, [0.5, 0.5], 0.2, 0.1, 0.001);
        assert_eq!((x, y, w, h), (200, 200, 600, 600));

        let (x, y, w, h) = ml_bounds(100, 100, [0.05, 0.95], 0.3, 0.1, 0.001);
        assert_eq!(x, 0);
        assert_eq!(y + h, 100);
        assert!(w <= 100 && h <= 100);

        let (x, y, w, h) = ml_bounds(320, 200, [0.5, 0.5], 2.0, 0.0, 0.001);
        assert_eq!((x, y, w, h), (0, 0, 320, 200));
    }

    #[test]
    fn ml_bounds_respects_aspect_correction() {
        let (x, y, w, h) = ml_bounds(200, 100, [0.5, 0.5], 0.2, 0.0, 0.001);
        assert_eq!((x, y, w, h), (80, 30, 40, 40));
    }
}
