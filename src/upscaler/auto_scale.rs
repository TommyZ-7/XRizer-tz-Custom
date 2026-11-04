//! Frame-time controller for XRizer's dynamic render scale.

pub const SCALE_STEP: f32 = 0.05;
pub const SLOW_FRAMES: u32 = 30;
pub const FAST_FRAMES: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoScaleConfig {
    pub min_scale: f32,
    pub max_scale: f32,
    pub step: f32,
    pub slow_frames: u32,
    pub fast_frames: u32,
}

impl Default for AutoScaleConfig {
    fn default() -> Self {
        Self {
            min_scale: 0.5,
            max_scale: 1.5,
            step: SCALE_STEP,
            slow_frames: SLOW_FRAMES,
            fast_frames: FAST_FRAMES,
        }
    }
}

impl AutoScaleConfig {
    pub fn sanitized(mut self) -> Self {
        self.min_scale = self.min_scale.clamp(0.1, 4.0);
        self.max_scale = self.max_scale.clamp(0.1, 4.0);
        if self.min_scale > self.max_scale {
            std::mem::swap(&mut self.min_scale, &mut self.max_scale);
        }
        self.step = self.step.clamp(0.01, 0.5);
        self.slow_frames = self.slow_frames.max(1);
        self.fast_frames = self.fast_frames.max(1);
        self
    }
}

#[derive(Debug, Clone)]
pub struct AutoScaleController {
    scale: f32,
    slow_frames: u32,
    fast_frames: u32,
}

impl AutoScaleController {
    pub fn new(scale: f32) -> Self {
        Self {
            scale: scale.clamp(0.1, 4.0),
            slow_frames: 0,
            fast_frames: 0,
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale.clamp(0.1, 4.0);
        self.slow_frames = 0;
        self.fast_frames = 0;
    }

    /// Feed one frame interval. Returns the new scale when an adjustment is
    /// due. Slow and fast counters reset each other, so alternating frame times
    /// cannot oscillate the scale.
    pub fn update(
        &mut self,
        frame_ms: f32,
        target_ms: f32,
        config: AutoScaleConfig,
    ) -> Option<f32> {
        let config = config.sanitized();
        if !frame_ms.is_finite() || !target_ms.is_finite() || target_ms <= 0.0 {
            return None;
        }

        if frame_ms > target_ms * 1.1 {
            self.slow_frames = self.slow_frames.saturating_add(1);
            self.fast_frames = 0;
        } else if frame_ms < target_ms * 0.9 {
            self.fast_frames = self.fast_frames.saturating_add(1);
            self.slow_frames = 0;
        } else {
            self.slow_frames = 0;
            self.fast_frames = 0;
            return None;
        }

        if self.slow_frames >= config.slow_frames {
            self.slow_frames = 0;
            let next = (self.scale - config.step).clamp(config.min_scale, config.max_scale);
            if (next - self.scale).abs() > f32::EPSILON {
                self.scale = next;
                return Some(next);
            }
        } else if self.fast_frames >= config.fast_frames {
            self.fast_frames = 0;
            let next = (self.scale + config.step).clamp(config.min_scale, config.max_scale);
            if (next - self.scale).abs() > f32::EPSILON {
                self.scale = next;
                return Some(next);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AutoScaleConfig {
        AutoScaleConfig {
            min_scale: 0.5,
            max_scale: 1.5,
            ..Default::default()
        }
    }

    #[test]
    fn steps_down_after_30_slow_frames() {
        let mut controller = AutoScaleController::new(1.0);
        for _ in 0..29 {
            assert_eq!(controller.update(20.0, 11.1, config()), None);
        }
        assert_eq!(controller.update(20.0, 11.1, config()), Some(0.95));
        assert_eq!(controller.scale(), 0.95);
    }

    #[test]
    fn steps_up_after_60_fast_frames() {
        let mut controller = AutoScaleController::new(1.0);
        for _ in 0..59 {
            assert_eq!(controller.update(8.0, 11.1, config()), None);
        }
        assert_eq!(controller.update(8.0, 11.1, config()), Some(1.05));
        assert_eq!(controller.scale(), 1.05);
    }

    #[test]
    fn clamps_at_limits() {
        let mut controller = AutoScaleController::new(0.5);
        for _ in 0..60 {
            controller.update(20.0, 11.1, config());
        }
        assert_eq!(controller.scale(), 0.5);

        let mut controller = AutoScaleController::new(1.5);
        for _ in 0..120 {
            controller.update(8.0, 11.1, config());
        }
        assert_eq!(controller.scale(), 1.5);
    }

    #[test]
    fn alternating_frames_do_not_oscillate() {
        let mut controller = AutoScaleController::new(1.0);
        for index in 0..200 {
            let frame_time = if index % 2 == 0 { 20.0 } else { 8.0 };
            controller.update(frame_time, 11.1, config());
        }
        assert_eq!(controller.scale(), 1.0);
    }

    #[test]
    fn zero_target_is_ignored() {
        let mut controller = AutoScaleController::new(1.0);
        assert_eq!(controller.update(100.0, 0.0, config()), None);
        assert_eq!(controller.scale(), 1.0);
    }

    #[test]
    fn min_and_max_are_sanitized() {
        let config = AutoScaleConfig {
            min_scale: 2.0,
            max_scale: 0.5,
            ..Default::default()
        }
        .sanitized();
        assert!(config.min_scale <= config.max_scale);
    }
}
