use super::*;
use openvr as vr;

fn full_bounds() -> vr::VRTextureBounds_t {
    vr::VRTextureBounds_t {
        uMin: 0.0,
        vMin: 0.0,
        uMax: 1.0,
        vMax: 1.0,
    }
}

#[test]
fn default_config_is_aa_only() {
    let c = UpscalerConfig::default();
    assert!(!c.enabled);
    assert_eq!(c.antialias, AntialiasMethod::Fxaa);
    assert_eq!(c.weight_mode, WeightMode::Inverted);
    assert_eq!(c.center, [0.5, 0.5]);
    assert_eq!(c.center_offset, 0.0);
    assert_eq!(c.radius, 0.35);
    assert_eq!(c.feather, 0.10);
    assert_eq!(c.strength, 1.0);
    assert!(c.preserve_alpha);
    assert_eq!(c.fxaa_quality, FxaaQuality::Quality);
    assert_eq!(c.smaa_threshold, 0.1);
    assert!(!c.compare);
    assert_eq!(c.compare_interval_ms, 2000);
    assert!(!c.show_region);
    assert!(!c.auto_scale);
    assert_eq!(c.min_scale, 0.5);
    assert_eq!(c.max_scale, 1.5);
}

#[test]
fn contract_aa_default_matches_runtime_defaults() {
    let text = include_str!("../../tests/fixtures/settings-v1/aa-default.json");
    let contract = serde_json::from_str::<UpscalerConfig>(text)
        .unwrap()
        .sanitized();
    let mut expected = contract;
    // B1b temporary: the final contract default is cmaa2; B2 flips this.
    expected.antialias = AntialiasMethod::Fxaa;
    assert_eq!(UpscalerConfig::default(), expected);
}

#[test]
fn params_carry_fxaa_quality() {
    let c = UpscalerConfig {
        enabled: true,
        fxaa_quality: FxaaQuality::Balanced,
        ..Default::default()
    };
    let p = c.params_for_size(100, 100, full_bounds()).unwrap();
    assert_eq!(p.fxaa_quality, FxaaQuality::Balanced);
}

#[test]
fn parse_all_fxaa_qualities() {
    for (quality, expected) in [
        ("performance", FxaaQuality::Performance),
        ("balanced", FxaaQuality::Balanced),
        ("quality", FxaaQuality::Quality),
    ] {
        let text = format!(r#"{{"fxaa_quality":"{quality}"}}"#);
        let c: UpscalerConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(c.fxaa_quality, expected);
    }
}

#[test]
fn params_are_always_equal_size() {
    let c = UpscalerConfig {
        enabled: true,
        ..Default::default()
    };
    let p = c.params_for_size(1000, 800, full_bounds()).unwrap();
    assert_eq!((p.dst_width, p.dst_height), (1000, 800));
}

#[test]
fn legacy_upscaling_keys_are_ignored() {
    let c: UpscalerConfig = serde_json::from_str(
        r#"{"enabled":true,"preset":"ml","output_scale":0.5,"model_path":"/tmp/x"}"#,
    )
    .unwrap();
    assert!(c.enabled);
    assert_eq!(c.antialias, AntialiasMethod::Fxaa);
}

#[test]
fn parse_all_aa_methods_and_weight_modes() {
    for (method, expected) in [
        ("off", AntialiasMethod::Off),
        ("cmaa2", AntialiasMethod::Cmaa2),
        ("smaa", AntialiasMethod::Smaa),
        ("fxaa", AntialiasMethod::Fxaa),
    ] {
        let text = format!(r#"{{"antialias":"{method}"}}"#);
        let c: UpscalerConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(c.antialias, expected);
    }
    for (mode, expected) in [
        ("center", WeightMode::Center),
        ("inverted", WeightMode::Inverted),
        ("whole", WeightMode::Whole),
    ] {
        let text = format!(r#"{{"weight_mode":"{mode}"}}"#);
        let c: UpscalerConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(c.weight_mode, expected);
    }
}

#[test]
fn compare_phase_uses_the_configured_interval() {
    assert!(compare_phase(0, 2000));
    assert!(compare_phase(1999, 2000));
    assert!(!compare_phase(2000, 2000));
    assert!(!compare_phase(3999, 2000));
    assert!(compare_phase(4000, 2000));
}

#[test]
fn sanitize_clamps_to_contract_ranges() {
    let c = UpscalerConfig {
        center: [-1.0, 2.0],
        center_offset: 9.0,
        radius: 9.0,
        feather: -1.0,
        strength: 9.0,
        smaa_threshold: 9.0,
        compare_interval_ms: 1,
        min_scale: 9.0,
        max_scale: 0.0,
        ..Default::default()
    }
    .sanitized();
    assert_eq!(c.center, [0.0, 1.0]);
    assert_eq!(c.center_offset, 0.2);
    assert_eq!(c.radius, 1.0);
    assert_eq!(c.feather, 0.0);
    assert_eq!(c.strength, 1.0);
    assert_eq!(c.smaa_threshold, 0.2);
    assert_eq!(c.compare_interval_ms, 500);
    assert_eq!(c.min_scale, 0.1);
    assert_eq!(c.max_scale, 4.0);
}

#[test]
fn center_offset_moves_each_eye_in_opposite_directions() {
    let c = UpscalerConfig {
        enabled: true,
        center: [0.5, 0.5],
        center_offset: 0.1,
        radius: 0.2,
        feather: 0.0,
        weight_mode: WeightMode::Center,
        ..Default::default()
    };
    let p = c.params_for_size(100, 100, full_bounds()).unwrap();
    assert_eq!(p.center_for_eye(0), [0.4, 0.5]);
    assert_eq!(p.center_for_eye(1), [0.6, 0.5]);
    assert_eq!(p.weight_at_for_eye([0.4, 0.5], 0), 1.0);
    assert_eq!(p.weight_at_for_eye([0.4, 0.5], 1), 0.0);
}

#[test]
fn whole_mode_is_uniform_even_with_radius_zero() {
    let c = UpscalerConfig {
        enabled: true,
        radius: 0.0,
        feather: 0.0,
        weight_mode: WeightMode::Whole,
        ..Default::default()
    };
    let p = c.params_for_size(100, 100, full_bounds()).unwrap();
    assert_eq!(p.weight_at([0.0, 0.0]), 1.0);
    assert_eq!(p.weight_at([0.5, 0.5]), 1.0);
    assert_eq!(p.weight_at([1.0, 1.0]), 1.0);
}

#[test]
fn compare_does_not_change_output_extent() {
    let b = full_bounds();
    let c = UpscalerConfig {
        enabled: true,
        compare: true,
        ..Default::default()
    };
    let on = c.params_for_size_at(1000, 800, b, true).unwrap();
    let off = c.params_for_size_at(1000, 800, b, false).unwrap();
    assert_eq!((on.dst_width, on.dst_height), (1000, 800));
    assert_eq!((off.dst_width, off.dst_height), (1000, 800));
    assert_ne!(on.enabled, off.enabled);
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
        antialias: AntialiasMethod::Fxaa,
        weight_mode: WeightMode::Center,
        center: [0.4, 0.6],
        center_offset: -0.05,
        radius: 0.5,
        feather: 0.2,
        strength: 0.8,
        preserve_alpha: false,
        fxaa_quality: FxaaQuality::Balanced,
        smaa_threshold: 0.15,
        compare: true,
        compare_interval_ms: 3000,
        show_region: true,
        auto_scale: true,
        min_scale: 0.6,
        max_scale: 1.4,
    };
    let json = serde_json::to_string(&c).unwrap();
    let parsed: UpscalerConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, c);

    assert!(serde_json::from_str::<UpscalerConfig>("not json").is_err());
    assert!(serde_json::from_str::<UpscalerConfig>("{\"antialias\":\"nope\"}").is_err());
}

#[test]
fn missing_fields_use_defaults() {
    let c: UpscalerConfig = serde_json::from_str("{\"enabled\":true}").unwrap();
    assert!(c.enabled);
    assert_eq!(c.antialias, AntialiasMethod::Fxaa);
    assert_eq!(c.weight_mode, WeightMode::Inverted);
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
    assert_eq!(c.radius, 1.0);
    assert_eq!(c.feather, 0.0);
    assert_eq!(c.strength, 0.0);
    let _ = std::fs::remove_file(&path);
}
