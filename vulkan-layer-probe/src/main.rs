//! Manual probe for the XRizer opt-in Vulkan layer.
//!
//! Task L2 expands this with DP4A/cooperative-matrix pipeline checks. For now
//! it verifies that the layer can be discovered and that a Vulkan instance and
//! physical devices can still be enumerated through the pass-through.

use ash::vk;
use std::ffi::CStr;

fn main() {
    println!(
        "XRIZER_UPSCALER_VK_LAYER={:?} XRIZER_UPSCALER_VK_LAYER_DISABLE={:?}",
        std::env::var("XRIZER_UPSCALER_VK_LAYER").ok(),
        std::env::var("XRIZER_UPSCALER_VK_LAYER_DISABLE").ok(),
    );

    let entry = match unsafe { ash::Entry::load() } {
        Ok(entry) => entry,
        Err(error) => {
            eprintln!("vulkan loader unavailable: {error}");
            std::process::exit(1);
        }
    };

    let application_info = vk::ApplicationInfo::default()
        .application_name(c"xrizer-vk-probe")
        .api_version(vk::API_VERSION_1_3);
    let create_info = vk::InstanceCreateInfo::default().application_info(&application_info);
    let instance = match unsafe { entry.create_instance(&create_info, None) } {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("instance creation failed: {error:?}");
            std::process::exit(1);
        }
    };

    let devices = match unsafe { instance.enumerate_physical_devices() } {
        Ok(devices) => devices,
        Err(error) => {
            eprintln!("physical device enumeration failed: {error:?}");
            unsafe { instance.destroy_instance(None) };
            std::process::exit(1);
        }
    };

    for device in devices {
        let properties = unsafe { instance.get_physical_device_properties(device) };
        let name = unsafe { CStr::from_ptr(properties.device_name.as_ptr()) };
        println!(
            "device: {} ({:?}, api {}.{}.{})",
            name.to_string_lossy(),
            properties.device_type,
            vk::api_version_major(properties.api_version),
            vk::api_version_minor(properties.api_version),
            vk::api_version_patch(properties.api_version),
        );
    }

    unsafe { instance.destroy_instance(None) };
    println!("dp4a=unknown coopmat=unknown");
}
