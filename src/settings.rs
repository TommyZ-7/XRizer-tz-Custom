use log::debug;
use openvr as vr;
use openvr::EVRSettingsError;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// ponytail: global scale, per-app map if needed
pub(crate) static SUPERSAMPLE_SCALE: AtomicU32 = AtomicU32::new(100);
static MANUAL_OVERRIDE: AtomicU32 = AtomicU32::new(1);
static LAST_POLL: AtomicU64 = AtomicU64::new(0);
// ponytail: 100 = normal, 0 = black. Game projection layers only, overlays untouched.
pub(crate) static BRIGHTNESS: AtomicU32 = AtomicU32::new(100);
static LAST_POLL_BRIGHTNESS: AtomicU64 = AtomicU64::new(0);

fn scale_file_candidates() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("XRIZER_SCALE_FILE") {
        v.push(p.into());
    }
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        v.push(std::path::PathBuf::from(d).join("xrizer_scale"));
    }
    if let Ok(h) = std::env::var("HOME") {
        v.push(std::path::PathBuf::from(h.clone()).join(".config/xrizer_scale"));
        v.push(std::path::PathBuf::from(h).join(".local/share/xrizer_scale"));
    }
    v.push(std::path::PathBuf::from("/tmp/xrizer_scale"));
    v
}

pub(crate) fn maybe_reload_scale() {
    if cfg!(test) && std::env::var("XRIZER_TEST_FILE_POLL").is_err() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let last = LAST_POLL.load(Ordering::Relaxed);
    if now.wrapping_sub(last) < 500 {
        return;
    }
    LAST_POLL.store(now, Ordering::Relaxed);
    for path in scale_file_candidates() {
        if let Ok(s) = std::fs::read_to_string(&path)
            && let Ok(val) = s.trim().parse::<f32>()
        {
            let val = if val > 4.0 { val / 100.0 } else { val };
            let clamped = val.clamp(0.1, 4.0);
            SUPERSAMPLE_SCALE.store((clamped * 100.0) as u32, Ordering::Relaxed);
            break;
        }
    }
}

fn persist_scale(clamped: f32) {
    if cfg!(test) && std::env::var("XRIZER_TEST_FILE_POLL").is_err() {
        return;
    }
    let txt = format!("{clamped:.3}");
    for path in scale_file_candidates() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, &txt);
    }
}

fn brightness_file_candidates() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("XRIZER_BRIGHTNESS_FILE") {
        v.push(p.into());
    }
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        v.push(std::path::PathBuf::from(d).join("xrizer_brightness"));
    }
    if let Ok(h) = std::env::var("HOME") {
        v.push(std::path::PathBuf::from(h.clone()).join(".config/xrizer_brightness"));
        v.push(std::path::PathBuf::from(h).join(".local/share/xrizer_brightness"));
    }
    v.push(std::path::PathBuf::from("/tmp/xrizer_brightness"));
    v
}

pub(crate) fn maybe_reload_brightness() {
    if cfg!(test) && std::env::var("XRIZER_TEST_FILE_POLL").is_err() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let last = LAST_POLL_BRIGHTNESS.load(Ordering::Relaxed);
    if now.wrapping_sub(last) < 500 {
        return;
    }
    LAST_POLL_BRIGHTNESS.store(now, Ordering::Relaxed);
    for path in brightness_file_candidates() {
        if let Ok(s) = std::fs::read_to_string(&path)
            && let Ok(val) = s.trim().parse::<f32>()
        {
            let val = if val > 1.0 { val / 100.0 } else { val };
            let clamped = val.clamp(0.0, 1.0);
            BRIGHTNESS.store((clamped * 100.0) as u32, Ordering::Relaxed);
            break;
        }
    }
}

fn persist_brightness(clamped: f32) {
    if cfg!(test) && std::env::var("XRIZER_TEST_FILE_POLL").is_err() {
        return;
    }
    let txt = format!("{clamped:.3}");
    for path in brightness_file_candidates() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, &txt);
    }
}

/// Current render scale, reloaded from disk at most every 500 ms.
pub(crate) fn current_supersample_scale() -> f32 {
    maybe_reload_scale();
    (SUPERSAMPLE_SCALE.load(Ordering::Relaxed) as f32 / 100.0).clamp(0.1, 4.0)
}

/// Set and persist the render scale, clamped to the supported range.
pub(crate) fn set_supersample_scale(scale: f32) {
    let clamped = scale.clamp(0.1, 4.0);
    SUPERSAMPLE_SCALE.store((clamped * 100.0) as u32, Ordering::Relaxed);
    persist_scale(clamped);
}

/// Apply the supersampling/render scale to the runtime's base recommended
/// per-eye size. The scale is clamped to the range accepted by
/// `SetFloat("steamvr", "supersampleScale")`.
pub(crate) fn apply_render_scale(base: (u32, u32), scale: f32) -> (u32, u32) {
    let scale = scale.clamp(0.1, 4.0);
    (
        ((base.0 as f32 * scale).round() as u32).max(1),
        ((base.1 as f32 * scale).round() as u32).max(1),
    )
}

/// Reloaded + clamped 0.0..=1.0 brightness. 1.0 = normal, 0.0 = black.
pub(crate) fn brightness_value() -> f32 {
    maybe_reload_brightness();
    (BRIGHTNESS.load(Ordering::Relaxed) as f32 / 100.0).clamp(0.0, 1.0)
}

#[derive(Default, macros::InterfaceImpl)]
#[interface = "IVRSettings"]
#[versions(003, 002)]
pub struct Settings {
    vtables: Vtables,
}

impl vr::IVRSettings003_Interface for Settings {
    fn GetSettingsErrorNameFromEnum(&self, error: EVRSettingsError) -> *const c_char {
        #[allow(unreachable_patterns)]
        let error: &'static CStr = match error {
            EVRSettingsError::None => c"",
            EVRSettingsError::IPCFailed => c"IPC Failed",
            EVRSettingsError::WriteFailed => c"Write Failed",
            EVRSettingsError::ReadFailed => c"Read Failed",
            EVRSettingsError::JsonParseFailed => c"JSON Parse Failed",
            EVRSettingsError::UnsetSettingHasNoDefault => c"Unset setting has no default",
            EVRSettingsError::AccessDenied => c"Access denied",
            _ => c"Unknown error",
        };
        error.as_ptr()
    }

    fn SetBool(
        &self,
        section: *const c_char,
        settings_key: *const c_char,
        value: bool,
        error: *mut EVRSettingsError,
    ) {
        let section = unsafe { CStr::from_ptr(section) }.to_string_lossy();
        let key = unsafe { CStr::from_ptr(settings_key) }.to_string_lossy();
        if section == "steamvr" && key == "supersampleManualOverride" {
            MANUAL_OVERRIDE.store(u32::from(value), Ordering::Relaxed);
            debug!("Set supersampleManualOverride={value}");
        } else {
            debug!("Setting bool on {section}/{key} to {value}");
        }
        if let Some(error) = unsafe { error.as_mut() } {
            *error = EVRSettingsError::None;
        }
    }

    fn SetInt32(
        &self,
        section: *const c_char,
        settings_key: *const c_char,
        value: i32,
        error: *mut EVRSettingsError,
    ) {
        let section = unsafe { CStr::from_ptr(section) }.to_string_lossy();
        let key = unsafe { CStr::from_ptr(settings_key) }.to_string_lossy();
        if section == "steamvr" && key == "supersampleManualOverride" {
            MANUAL_OVERRIDE.store(u32::from(value != 0), Ordering::Relaxed);
            debug!("Set supersampleManualOverride={value}");
        } else {
            debug!("Setting int on {section}/{key} to {value}");
        }
        if let Some(error) = unsafe { error.as_mut() } {
            *error = EVRSettingsError::None;
        }
    }

    fn SetFloat(
        &self,
        section: *const c_char,
        settings_key: *const c_char,
        value: f32,
        error: *mut EVRSettingsError,
    ) {
        let section = unsafe { CStr::from_ptr(section) }.to_string_lossy();
        let key = unsafe { CStr::from_ptr(settings_key) }.to_string_lossy();
        if section == "steamvr" && key == "supersampleScale" {
            set_supersample_scale(value);
            debug!(
                "Set supersampleScale={} (raw {value})",
                current_supersample_scale()
            );
        } else if section == "xrizer" && key == "brightness" {
            let clamped = value.clamp(0.0, 1.0);
            BRIGHTNESS.store((clamped * 100.0) as u32, Ordering::Relaxed);
            persist_brightness(clamped);
            debug!("Set brightness={clamped} (raw {value})");
        } else {
            debug!("Setting float on {section}/{key} to {value}");
        }
        if let Some(error) = unsafe { error.as_mut() } {
            *error = EVRSettingsError::None;
        }
    }

    fn SetString(
        &self,
        section: *const c_char,
        settings_key: *const c_char,
        value: *const c_char,
        error: *mut EVRSettingsError,
    ) {
        let section = unsafe { CStr::from_ptr(section) }.to_string_lossy();
        let key = unsafe { CStr::from_ptr(settings_key) }.to_string_lossy();
        let value = unsafe { CStr::from_ptr(value) }.to_string_lossy();
        debug!("Setting string on {section}/{key} to {value}");
        if let Some(error) = unsafe { error.as_mut() } {
            *error = EVRSettingsError::None;
        }
    }

    fn GetBool(
        &self,
        section: *const c_char,
        settings_key: *const c_char,
        error: *mut EVRSettingsError,
    ) -> bool {
        let section = unsafe { CStr::from_ptr(section) }.to_string_lossy();
        let key = unsafe { CStr::from_ptr(settings_key) }.to_string_lossy();
        if let Some(error) = unsafe { error.as_mut() } {
            *error = EVRSettingsError::None;
        }
        if section == "steamvr" && key == "supersampleManualOverride" {
            return MANUAL_OVERRIDE.load(Ordering::Relaxed) != 0;
        }
        debug!("Getting bool on {section}/{key}");
        false
    }

    fn GetInt32(
        &self,
        section: *const c_char,
        settings_key: *const c_char,
        error: *mut EVRSettingsError,
    ) -> i32 {
        let section = unsafe { CStr::from_ptr(section) }.to_string_lossy();
        let key = unsafe { CStr::from_ptr(settings_key) }.to_string_lossy();
        if let Some(error) = unsafe { error.as_mut() } {
            *error = EVRSettingsError::None;
        }
        if section == "steamvr" && key == "supersampleManualOverride" {
            return MANUAL_OVERRIDE.load(Ordering::Relaxed) as i32;
        }
        debug!("Getting int on {section}/{key}");
        0
    }

    fn GetFloat(
        &self,
        section: *const c_char,
        settings_key: *const c_char,
        error: *mut EVRSettingsError,
    ) -> f32 {
        let section = unsafe { CStr::from_ptr(section) }.to_string_lossy();
        let key = unsafe { CStr::from_ptr(settings_key) }.to_string_lossy();
        if let Some(error) = unsafe { error.as_mut() } {
            *error = EVRSettingsError::None;
        }
        if section == "steamvr" && key == "supersampleScale" {
            return current_supersample_scale();
        }
        if section == "xrizer" && key == "brightness" {
            return brightness_value();
        }
        debug!("Getting float on {section}/{key}");
        0.0
    }

    fn GetString(
        &self,
        section: *const c_char,
        settings_key: *const c_char,
        value: *mut c_char,
        value_len: u32,
        error: *mut EVRSettingsError,
    ) {
        let section = unsafe { CStr::from_ptr(section) }.to_string_lossy();
        let key = unsafe { CStr::from_ptr(settings_key) }.to_string_lossy();
        if let Some(error) = unsafe { error.as_mut() } {
            *error = EVRSettingsError::None;
        }
        if value_len > 0 {
            unsafe {
                *value = 0;
            }
        }
        debug!("Getting string on {section}/{key}");
    }

    fn RemoveSection(&self, section: *const c_char, error: *mut EVRSettingsError) {
        let section = unsafe { CStr::from_ptr(section) }.to_string_lossy();
        if let Some(error) = unsafe { error.as_mut() } {
            *error = EVRSettingsError::None;
        }
        debug!("Removing section {section}");
    }

    fn RemoveKeyInSection(
        &self,
        section: *const c_char,
        settings_key: *const c_char,
        error: *mut EVRSettingsError,
    ) {
        let section = unsafe { CStr::from_ptr(section) }.to_string_lossy();
        let key = unsafe { CStr::from_ptr(settings_key) }.to_string_lossy();
        if let Some(error) = unsafe { error.as_mut() } {
            *error = EVRSettingsError::None;
        }
        debug!("Removing {section}/{key}");
    }
}

impl vr::IVRSettings002On003 for Settings {
    fn Sync(&self, _force: bool, error: *mut EVRSettingsError) -> bool {
        if let Some(error) = unsafe { error.as_mut() } {
            *error = EVRSettingsError::None;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openvr::IVRSettings003_Interface;
    use std::ffi::CString;
    static TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn set_float(sec: &str, key: &str, v: f32) {
        let s = Settings::default();
        let cs = CString::new(sec).unwrap();
        let ck = CString::new(key).unwrap();
        let mut err = vr::EVRSettingsError::None;
        s.SetFloat(cs.as_ptr(), ck.as_ptr(), v, &mut err as *mut _);
        assert_eq!(err, vr::EVRSettingsError::None);
    }
    fn get_float(sec: &str, key: &str) -> f32 {
        let s = Settings::default();
        let cs = CString::new(sec).unwrap();
        let ck = CString::new(key).unwrap();
        let mut err = vr::EVRSettingsError::None;
        s.GetFloat(cs.as_ptr(), ck.as_ptr(), &mut err as *mut _)
    }

    #[test]
    fn current_and_set_supersample_scale() {
        let _g = TEST_MUTEX.lock().unwrap();
        SUPERSAMPLE_SCALE.store(100, Ordering::Relaxed);
        set_supersample_scale(0.65);
        assert!((current_supersample_scale() - 0.65).abs() < 0.01);
        set_supersample_scale(99.0);
        assert_eq!(current_supersample_scale(), 4.0);
        set_supersample_scale(0.0);
        assert_eq!(current_supersample_scale(), 0.1);
        SUPERSAMPLE_SCALE.store(100, Ordering::Relaxed);
    }

    #[test]
    fn render_scale_rounds_and_clamps() {
        assert_eq!(apply_render_scale((1000, 800), 0.7), (700, 560));
        assert_eq!(apply_render_scale((1000, 800), 0.0), (100, 80));
        assert_eq!(apply_render_scale((1000, 800), 10.0), (4000, 3200));
        assert_eq!(apply_render_scale((1, 1), 0.1), (1, 1));
    }

    #[test]
    fn supersample_scale_roundtrip() {
        let _g = TEST_MUTEX.lock().unwrap();
        SUPERSAMPLE_SCALE.store(100, Ordering::Relaxed);
        set_float("steamvr", "supersampleScale", 0.7);
        let v = get_float("steamvr", "supersampleScale");
        assert!((v - 0.7).abs() < 0.01, "got {v}");
        set_float("steamvr", "supersampleScale", 0.8);
        assert!((get_float("steamvr", "supersampleScale") - 0.8).abs() < 0.01);
        SUPERSAMPLE_SCALE.store(100, Ordering::Relaxed);
    }

    #[test]
    fn supersample_scale_clamp() {
        let _g = TEST_MUTEX.lock().unwrap();
        SUPERSAMPLE_SCALE.store(100, Ordering::Relaxed);
        set_float("steamvr", "supersampleScale", 10.0);
        assert_eq!(get_float("steamvr", "supersampleScale"), 4.0);
        set_float("steamvr", "supersampleScale", 0.01);
        assert_eq!(get_float("steamvr", "supersampleScale"), 0.1);
        set_float("steamvr", "supersampleScale", 1.0);
        assert!((get_float("steamvr", "supersampleScale") - 1.0).abs() < 0.01);
        SUPERSAMPLE_SCALE.store(100, Ordering::Relaxed);
    }

    #[test]
    fn brightness_roundtrip() {
        let _g = TEST_MUTEX.lock().unwrap();
        BRIGHTNESS.store(100, Ordering::Relaxed);
        set_float("xrizer", "brightness", 0.5);
        let v = get_float("xrizer", "brightness");
        assert!((v - 0.5).abs() < 0.01, "got {v}");
        set_float("xrizer", "brightness", 1.0);
        assert!((get_float("xrizer", "brightness") - 1.0).abs() < 0.01);
        BRIGHTNESS.store(100, Ordering::Relaxed);
    }

    #[test]
    fn brightness_clamp() {
        let _g = TEST_MUTEX.lock().unwrap();
        BRIGHTNESS.store(100, Ordering::Relaxed);
        set_float("xrizer", "brightness", 2.0);
        assert_eq!(get_float("xrizer", "brightness"), 1.0);
        set_float("xrizer", "brightness", -1.0);
        assert_eq!(get_float("xrizer", "brightness"), 0.0);
        set_float("xrizer", "brightness", 0.0);
        assert_eq!(get_float("xrizer", "brightness"), 0.0);
        assert_eq!(brightness_value(), 0.0);
        BRIGHTNESS.store(100, Ordering::Relaxed);
    }

    #[test]
    fn brightness_file_poll_reload() {
        let _g = TEST_MUTEX.lock().unwrap();
        unsafe { std::env::set_var("XRIZER_TEST_FILE_POLL", "1") };
        let tmp = std::env::temp_dir().join("xrizer_test_brightness_file_poll");
        let _ = std::fs::remove_file(&tmp);
        unsafe { std::env::set_var("XRIZER_BRIGHTNESS_FILE", tmp.to_str().unwrap()) };
        LAST_POLL_BRIGHTNESS.store(0, Ordering::Relaxed);
        BRIGHTNESS.store(100, Ordering::Relaxed);
        std::fs::write(&tmp, "0.5").unwrap();
        LAST_POLL_BRIGHTNESS.store(0, Ordering::Relaxed);
        maybe_reload_brightness();
        assert!(
            (BRIGHTNESS.load(Ordering::Relaxed) as f32 / 100.0 - 0.5).abs() < 0.01,
            "got {}",
            BRIGHTNESS.load(Ordering::Relaxed)
        );
        std::fs::write(&tmp, "50").unwrap();
        LAST_POLL_BRIGHTNESS.store(0, Ordering::Relaxed);
        maybe_reload_brightness();
        assert!((BRIGHTNESS.load(Ordering::Relaxed) as f32 / 100.0 - 0.5).abs() < 0.01);
        let _ = std::fs::remove_file(&tmp);
        unsafe { std::env::remove_var("XRIZER_BRIGHTNESS_FILE") };
        unsafe { std::env::remove_var("XRIZER_TEST_FILE_POLL") };
        LAST_POLL_BRIGHTNESS.store(0, Ordering::Relaxed);
        BRIGHTNESS.store(100, Ordering::Relaxed);
    }

    #[test]
    fn manual_override_roundtrip() {
        let _g = TEST_MUTEX.lock().unwrap();
        MANUAL_OVERRIDE.store(1, Ordering::Relaxed);
        let s = Settings::default();
        let sec = CString::new("steamvr").unwrap();
        let key = CString::new("supersampleManualOverride").unwrap();
        let mut err = vr::EVRSettingsError::None;
        s.SetInt32(sec.as_ptr(), key.as_ptr(), 0, &mut err as *mut _);
        assert_eq!(
            s.GetInt32(sec.as_ptr(), key.as_ptr(), &mut err as *mut _),
            0
        );
        assert!(!s.GetBool(sec.as_ptr(), key.as_ptr(), &mut err as *mut _));
        s.SetBool(sec.as_ptr(), key.as_ptr(), true, &mut err as *mut _);
        assert_eq!(
            s.GetInt32(sec.as_ptr(), key.as_ptr(), &mut err as *mut _),
            1
        );
        assert!(s.GetBool(sec.as_ptr(), key.as_ptr(), &mut err as *mut _));
        MANUAL_OVERRIDE.store(1, Ordering::Relaxed);
    }

    #[test]
    fn file_poll_reload() {
        let _g = TEST_MUTEX.lock().unwrap();
        unsafe { std::env::set_var("XRIZER_TEST_FILE_POLL", "1") };
        let tmp = std::env::temp_dir().join("xrizer_test_scale_file_poll");
        let _ = std::fs::remove_file(&tmp);
        unsafe { std::env::set_var("XRIZER_SCALE_FILE", tmp.to_str().unwrap()) };
        LAST_POLL.store(0, Ordering::Relaxed);
        SUPERSAMPLE_SCALE.store(100, Ordering::Relaxed);
        std::fs::write(&tmp, "0.55").unwrap();
        LAST_POLL.store(0, Ordering::Relaxed);
        maybe_reload_scale();
        assert!(
            (SUPERSAMPLE_SCALE.load(Ordering::Relaxed) as f32 / 100.0 - 0.55).abs() < 0.01,
            "got {}",
            SUPERSAMPLE_SCALE.load(Ordering::Relaxed)
        );
        std::fs::write(&tmp, "150").unwrap();
        LAST_POLL.store(0, Ordering::Relaxed);
        maybe_reload_scale();
        assert!((SUPERSAMPLE_SCALE.load(Ordering::Relaxed) as f32 / 100.0 - 1.5).abs() < 0.01);
        let _ = std::fs::remove_file(&tmp);
        unsafe { std::env::remove_var("XRIZER_SCALE_FILE") };
        unsafe { std::env::remove_var("XRIZER_TEST_FILE_POLL") };
        LAST_POLL.store(0, Ordering::Relaxed);
        SUPERSAMPLE_SCALE.store(100, Ordering::Relaxed);
    }
}
