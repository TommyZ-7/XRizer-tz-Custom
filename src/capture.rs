//! Shared capture settings, shutter detection, and desktop notification.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureMode {
    Interval,
    Shutter,
}

#[derive(Debug, Clone)]
pub(crate) struct CaptureSettings {
    pub dir: PathBuf,
    pub interval: Duration,
    pub max_pairs: u32,
    pub scene: String,
    pub mode: CaptureMode,
    pub hold: Duration,
    pub lockout: Duration,
}

impl CaptureSettings {
    pub fn from_env() -> Option<Self> {
        let explicit = std::env::var("XRIZER_CAPTURE").ok();
        let dir = std::env::var_os("XRIZER_CAPTURE_DIR");
        let enabled = match explicit.as_deref() {
            Some("0" | "false" | "no" | "off") => false,
            Some(_) => true,
            None => dir.is_some(),
        };
        if !enabled {
            return None;
        }
        let Some(dir) = dir else {
            log::warn!("XRIZER_CAPTURE is enabled but XRIZER_CAPTURE_DIR is not set");
            return None;
        };
        if dir.is_empty() {
            log::warn!("XRIZER_CAPTURE_DIR is empty; capture disabled");
            return None;
        }
        let interval_ms = std::env::var("XRIZER_CAPTURE_INTERVAL_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(500)
            .clamp(100, 60_000);
        let max_pairs = std::env::var("XRIZER_CAPTURE_MAX_PAIRS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(300)
            .clamp(1, 100_000);
        let scene =
            std::env::var("XRIZER_CAPTURE_SCENE").unwrap_or_else(|_| "unspecified".to_string());
        let mode = match std::env::var("XRIZER_CAPTURE_MODE")
            .unwrap_or_else(|_| "interval".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "shutter" => CaptureMode::Shutter,
            "interval" => CaptureMode::Interval,
            other => {
                log::warn!("unknown XRIZER_CAPTURE_MODE={other}; using interval");
                CaptureMode::Interval
            }
        };
        let hold_ms = std::env::var("XRIZER_CAPTURE_HOLD_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(250)
            .clamp(100, 500);
        let lockout_ms = std::env::var("XRIZER_CAPTURE_LOCKOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1000)
            .clamp(200, 5000);
        Some(Self {
            dir: PathBuf::from(dir),
            interval: Duration::from_millis(interval_ms),
            max_pairs,
            scene,
            mode,
            hold: Duration::from_millis(hold_ms),
            lockout: Duration::from_millis(lockout_ms),
        })
    }

    pub fn is_shutter(&self) -> bool {
        self.mode == CaptureMode::Shutter
    }
}

/// One-shot detector for the four-point controller chord. The detector is
/// latched after a shot, so holding the chord does not repeat captures; the
/// lockout also rejects a quick release/repress.
#[derive(Debug, Default)]
pub(crate) struct ShutterDetector {
    hold_since: Option<Instant>,
    last_shot: Option<Instant>,
    latched: bool,
}

impl ShutterDetector {
    pub fn update(
        &mut self,
        pressed: bool,
        now: Instant,
        hold: Duration,
        lockout: Duration,
    ) -> bool {
        if !pressed {
            self.hold_since = None;
            self.latched = false;
            return false;
        }
        if self.latched {
            return false;
        }
        let hold_since = *self.hold_since.get_or_insert(now);
        if now.duration_since(hold_since) < hold {
            return false;
        }
        if let Some(last_shot) = self.last_shot
            && now.duration_since(last_shot) < lockout
        {
            return false;
        }
        self.last_shot = Some(now);
        self.hold_since = None;
        self.latched = true;
        true
    }
}

static SHUTTER_REQUEST: AtomicBool = AtomicBool::new(false);

pub(crate) fn request_shutter() {
    SHUTTER_REQUEST.store(true, Ordering::Release);
}

pub(crate) fn shutter_requested() -> bool {
    SHUTTER_REQUEST.load(Ordering::Acquire)
}

pub(crate) fn clear_shutter_request() {
    SHUTTER_REQUEST.store(false, Ordering::Release);
}

/// Best-effort freedesktop notification. Called from the capture writer thread,
/// never from the game's submit path.
pub(crate) fn notify_capture(index: u32, dir: &Path) {
    let body = format!("Saved frame {index} to {}", dir.display());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        notify_rust::Notification::new()
            .summary("XRizer capture")
            .appname("XRizer")
            .body(&body)
            .show()
    }));
    match result {
        Ok(Ok(_handle)) => {}
        Ok(Err(error)) => {
            crate::warn_once!("desktop capture notification failed (best effort)");
            log::debug!("desktop capture notification body: {body}; error: {error}");
        }
        Err(_) => {
            crate::warn_once!("desktop capture notification panicked (best effort)");
            log::debug!("desktop capture notification body: {body}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        unsafe {
            std::env::remove_var("XRIZER_CAPTURE");
            std::env::remove_var("XRIZER_CAPTURE_DIR");
            std::env::remove_var("XRIZER_CAPTURE_INTERVAL_MS");
            std::env::remove_var("XRIZER_CAPTURE_MAX_PAIRS");
            std::env::remove_var("XRIZER_CAPTURE_SCENE");
            std::env::remove_var("XRIZER_CAPTURE_MODE");
            std::env::remove_var("XRIZER_CAPTURE_HOLD_MS");
            std::env::remove_var("XRIZER_CAPTURE_LOCKOUT_MS");
        }
    }

    #[test]
    fn disabled_without_opt_in() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        assert!(CaptureSettings::from_env().is_none());
    }

    #[test]
    fn parses_interval_and_shutter_modes() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join("xrizer_capture_config_test");
        clear_env();
        unsafe {
            std::env::set_var("XRIZER_CAPTURE", "1");
            std::env::set_var("XRIZER_CAPTURE_DIR", &dir);
            std::env::set_var("XRIZER_CAPTURE_INTERVAL_MS", "1");
            std::env::set_var("XRIZER_CAPTURE_MAX_PAIRS", "999999");
            std::env::set_var("XRIZER_CAPTURE_SCENE", "test-scene");
            std::env::set_var("XRIZER_CAPTURE_MODE", "shutter");
            std::env::set_var("XRIZER_CAPTURE_HOLD_MS", "50");
            std::env::set_var("XRIZER_CAPTURE_LOCKOUT_MS", "100");
        }
        let config = CaptureSettings::from_env().unwrap();
        assert_eq!(config.dir, dir);
        assert_eq!(config.interval, Duration::from_millis(100));
        assert_eq!(config.max_pairs, 100_000);
        assert_eq!(config.scene, "test-scene");
        assert_eq!(config.mode, CaptureMode::Shutter);
        assert_eq!(config.hold, Duration::from_millis(100));
        assert_eq!(config.lockout, Duration::from_millis(200));
        clear_env();
    }

    #[test]
    fn defaults_to_interval_mode() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        let dir = std::env::temp_dir().join("xrizer_capture_default_mode_test");
        unsafe {
            std::env::set_var("XRIZER_CAPTURE", "1");
            std::env::set_var("XRIZER_CAPTURE_DIR", &dir);
        }
        let config = CaptureSettings::from_env().unwrap();
        assert_eq!(config.mode, CaptureMode::Interval);
        assert_eq!(config.hold, Duration::from_millis(250));
        assert_eq!(config.lockout, Duration::from_millis(1000));
        clear_env();
    }

    #[test]
    fn explicit_zero_disables_capture() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        let dir = std::env::temp_dir().join("xrizer_capture_zero_test");
        unsafe {
            std::env::set_var("XRIZER_CAPTURE", "0");
            std::env::set_var("XRIZER_CAPTURE_DIR", &dir);
        }
        assert!(CaptureSettings::from_env().is_none());
        clear_env();
    }

    #[test]
    fn shutter_requires_hold() {
        let mut detector = ShutterDetector::default();
        let start = Instant::now();
        assert!(!detector.update(
            true,
            start,
            Duration::from_millis(250),
            Duration::from_secs(1)
        ));
        assert!(!detector.update(
            true,
            start + Duration::from_millis(249),
            Duration::from_millis(250),
            Duration::from_secs(1)
        ));
        assert!(detector.update(
            true,
            start + Duration::from_millis(250),
            Duration::from_millis(250),
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn shutter_is_one_shot_while_held_and_has_lockout() {
        let mut detector = ShutterDetector::default();
        let start = Instant::now();
        let hold = Duration::from_millis(250);
        let lockout = Duration::from_secs(1);
        assert!(!detector.update(true, start, hold, lockout));
        assert!(detector.update(true, start + Duration::from_millis(300), hold, lockout));
        // Still held after the lockout: the latch prevents a repeat.
        assert!(!detector.update(true, start + Duration::from_secs(3), hold, lockout));
        assert!(!detector.update(
            false,
            start + Duration::from_secs(3) + Duration::from_millis(100),
            hold,
            lockout
        ));
        // Release/re-press inside the lockout is rejected.
        assert!(!detector.update(true, start + Duration::from_millis(500), hold, lockout));
        assert!(!detector.update(true, start + Duration::from_millis(800), hold, lockout));
        assert!(!detector.update(false, start + Duration::from_millis(900), hold, lockout));
        // After the lockout, a fresh hold fires.
        assert!(!detector.update(true, start + Duration::from_millis(1500), hold, lockout));
        assert!(detector.update(true, start + Duration::from_millis(1800), hold, lockout));
    }

    #[test]
    fn release_resets_hold() {
        let mut detector = ShutterDetector::default();
        let start = Instant::now();
        assert!(!detector.update(
            true,
            start,
            Duration::from_millis(250),
            Duration::from_secs(1)
        ));
        assert!(!detector.update(
            false,
            start + Duration::from_millis(200),
            Duration::from_millis(250),
            Duration::from_secs(1)
        ));
        assert!(!detector.update(
            true,
            start + Duration::from_millis(300),
            Duration::from_millis(250),
            Duration::from_secs(1)
        ));
        assert!(!detector.update(
            true,
            start + Duration::from_millis(500),
            Duration::from_millis(250),
            Duration::from_secs(1)
        ));
        assert!(detector.update(
            true,
            start + Duration::from_millis(550),
            Duration::from_millis(250),
            Duration::from_secs(1)
        ));
    }
}
