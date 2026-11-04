//! Independent OpenXR actions for the training-capture shutter chord.
//!
//! These actions are only created when capture mode is `shutter`. They are
//! added to the internal action sets and synced alongside the game's actions;
//! the game's input state is never consumed or modified.

use super::InteractionProfile;
use super::profiles::paths::{DynComponent, DynSubpath};
use super::profiles::{DynInputPath, LegalPathsT};
use crate::capture::CaptureSettings;
use crate::openxr_data::Hand;
use openxr as xr;
use std::time::Duration;

pub(super) struct ShutterActions {
    pub set: xr::ActionSet,
    pub left_grip: xr::Action<f32>,
    pub right_grip: xr::Action<f32>,
    pub left_trigger: xr::Action<f32>,
    pub right_trigger: xr::Action<f32>,
    pub left_grip_click: xr::Action<bool>,
    pub right_grip_click: xr::Action<bool>,
    pub left_trigger_click: xr::Action<bool>,
    pub right_trigger_click: xr::Action<bool>,
    pub hold: Duration,
    pub lockout: Duration,
}

impl ShutterActions {
    pub fn new(
        instance: &xr::Instance,
        left_path: xr::Path,
        right_path: xr::Path,
        settings: &CaptureSettings,
    ) -> Option<Self> {
        let set = instance
            .create_action_set("xrizer-capture-set", "XRizer capture shutter set", 0)
            .ok()?;
        let left_grip = set
            .create_action(
                "capture-left-grip",
                "Capture Left Grip",
                &[left_path, right_path],
            )
            .ok()?;
        let right_grip = set
            .create_action(
                "capture-right-grip",
                "Capture Right Grip",
                &[left_path, right_path],
            )
            .ok()?;
        let left_trigger = set
            .create_action(
                "capture-left-trigger",
                "Capture Left Trigger",
                &[left_path, right_path],
            )
            .ok()?;
        let right_trigger = set
            .create_action(
                "capture-right-trigger",
                "Capture Right Trigger",
                &[left_path, right_path],
            )
            .ok()?;
        let left_grip_click = set
            .create_action(
                "capture-left-grip-click",
                "Capture Left Grip Click",
                &[left_path, right_path],
            )
            .ok()?;
        let right_grip_click = set
            .create_action(
                "capture-right-grip-click",
                "Capture Right Grip Click",
                &[left_path, right_path],
            )
            .ok()?;
        let left_trigger_click = set
            .create_action(
                "capture-left-trigger-click",
                "Capture Left Trigger Click",
                &[left_path, right_path],
            )
            .ok()?;
        let right_trigger_click = set
            .create_action(
                "capture-right-trigger-click",
                "Capture Right Trigger Click",
                &[left_path, right_path],
            )
            .ok()?;
        Some(Self {
            set,
            left_grip,
            right_grip,
            left_trigger,
            right_trigger,
            left_grip_click,
            right_grip_click,
            left_trigger_click,
            right_trigger_click,
            hold: settings.hold,
            lockout: settings.lockout,
        })
    }

    /// Suggest `squeeze/value` and `trigger/value` bindings where the profile
    /// supports them. Profiles without those paths simply do not get a shutter
    /// binding; no unsupported path is suggested.
    pub fn bindings<P: InteractionProfile>(&self, instance: &xr::Instance) -> Vec<xr::Binding<'_>> {
        let mut bindings = Vec::new();
        for (hand, hand_name, grip, trigger, grip_click, trigger_click) in [
            (
                Hand::Left,
                "left",
                &self.left_grip,
                &self.left_trigger,
                &self.left_grip_click,
                &self.left_trigger_click,
            ),
            (
                Hand::Right,
                "right",
                &self.right_grip,
                &self.right_trigger,
                &self.right_grip_click,
                &self.right_trigger_click,
            ),
        ] {
            for (subpath, value_action, click_action) in [
                (DynSubpath::Squeeze, grip, grip_click),
                (DynSubpath::Trigger, trigger, trigger_click),
            ] {
                let value_path = DynInputPath {
                    hand,
                    subpath,
                    component: Some(DynComponent::Value),
                };
                if P::LegalPaths::is_legal(value_path) {
                    let path = instance
                        .string_to_path(&format!("/user/hand/{hand_name}/input/{subpath}/value"))
                        .unwrap();
                    bindings.push(xr::Binding::new(value_action, path));
                }
                let click_path = DynInputPath {
                    hand,
                    subpath,
                    component: Some(DynComponent::Click),
                };
                if P::LegalPaths::is_legal(click_path) {
                    let path = instance
                        .string_to_path(&format!("/user/hand/{hand_name}/input/{subpath}/click"))
                        .unwrap();
                    bindings.push(xr::Binding::new(click_action, path));
                }
            }
        }
        bindings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clientcore::Injector;
    use crate::input::profiles::knuckles::Knuckles;
    use crate::input::profiles::oculus_touch::OculusTouch;
    use crate::input::profiles::vive_controller::ViveWands;
    use crate::openxr_data::OpenXrData;

    fn settings() -> CaptureSettings {
        CaptureSettings {
            dir: std::env::temp_dir().join("xrizer_shutter_binding_test"),
            interval: Duration::from_millis(500),
            max_pairs: 10,
            scene: "test".into(),
            mode: crate::capture::CaptureMode::Shutter,
            hold: Duration::from_millis(250),
            lockout: Duration::from_secs(1),
        }
    }

    #[test]
    fn supported_profiles_bind_grip_and_trigger_values() {
        let xr = OpenXrData::<crate::compositor::Compositor>::new(&Injector::default()).unwrap();
        let left = xr.instance.string_to_path("/user/hand/left").unwrap();
        let right = xr.instance.string_to_path("/user/hand/right").unwrap();
        let actions = ShutterActions::new(&xr.instance, left, right, &settings()).unwrap();
        assert_eq!(actions.bindings::<OculusTouch>(&xr.instance).len(), 4);
        assert_eq!(actions.bindings::<Knuckles>(&xr.instance).len(), 6);
        assert_eq!(actions.bindings::<ViveWands>(&xr.instance).len(), 6);
    }
}
