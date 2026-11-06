//! CPU reference math for the AA weight field.

use super::WeightMode;

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

fn radial_distance(uv: [f32; 2], center: [f32; 2], aspect: f32) -> f32 {
    let dx = (uv[0] - center[0]) * aspect;
    let dy = uv[1] - center[1];
    (dx * dx + dy * dy).sqrt()
}

/// Continuous radial weight in aspect-corrected UV space. This is the
/// original `center` mode: maximum at the centre and zero outside the field.
#[allow(dead_code)]
pub fn weight_at(uv: [f32; 2], center: [f32; 2], radius: f32, feather: f32, aspect: f32) -> f32 {
    weight_at_mode(uv, center, radius, feather, aspect, WeightMode::Center)
}

/// Weight for one of the three AA field modes.
pub fn weight_at_mode(
    uv: [f32; 2],
    center: [f32; 2],
    radius: f32,
    feather: f32,
    aspect: f32,
    mode: WeightMode,
) -> f32 {
    if mode == WeightMode::Whole {
        return 1.0;
    }

    let distance = radial_distance(uv, center, aspect);
    if feather <= 0.0 {
        let inside = distance <= radius;
        return match mode {
            WeightMode::Center => {
                if inside {
                    1.0
                } else {
                    0.0
                }
            }
            WeightMode::Inverted => {
                if inside {
                    0.0
                } else {
                    1.0
                }
            }
            WeightMode::Whole => 1.0,
        };
    }

    match mode {
        WeightMode::Center => 1.0 - smoothstep(radius, radius + feather, distance),
        WeightMode::Inverted => smoothstep(radius, radius + feather, distance),
        WeightMode::Whole => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn center_mode_matches_the_existing_field() {
        let center = [0.5, 0.5];
        assert_eq!(
            weight_at_mode([0.5, 0.5], center, 0.2, 0.1, 1.0, WeightMode::Center),
            1.0
        );
        assert_eq!(
            weight_at_mode([0.9, 0.5], center, 0.2, 0.1, 1.0, WeightMode::Center),
            0.0
        );
    }

    #[test]
    fn inverted_mode_is_zero_in_the_centre_and_one_outside() {
        let center = [0.5, 0.5];
        assert_eq!(
            weight_at_mode([0.5, 0.5], center, 0.2, 0.1, 1.0, WeightMode::Inverted),
            0.0
        );
        assert_eq!(
            weight_at_mode([0.9, 0.5], center, 0.2, 0.1, 1.0, WeightMode::Inverted),
            1.0
        );
    }

    #[test]
    fn whole_mode_is_one_everywhere() {
        let center = [0.5, 0.5];
        for uv in [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]] {
            assert_eq!(
                weight_at_mode(uv, center, 0.2, 0.1, 1.0, WeightMode::Whole),
                1.0
            );
        }
    }

    #[test]
    fn weight_field_is_smooth_and_monotonic_in_center_mode() {
        let center = [0.5, 0.5];
        let radius = 0.2;
        let feather = 0.1;
        let aspect = 1.0;
        let mut previous = 1.0;
        for step in 0..=100 {
            let d = step as f32 / 100.0;
            let w = weight_at([0.5 + d, 0.5], center, radius, feather, aspect);
            assert!((0.0..=1.0).contains(&w));
            assert!(
                w <= previous + 1e-6,
                "weight must not increase with distance"
            );
            previous = w;
        }
        assert_eq!(weight_at([0.9, 0.5], center, radius, feather, aspect), 0.0);
    }
}
