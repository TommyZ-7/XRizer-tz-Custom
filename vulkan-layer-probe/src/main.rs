use ash::vk;
use std::ffi::CStr;
use std::io::Cursor;

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
    let Some(physical_device) = devices
        .iter()
        .copied()
        .find(|device| {
            let properties = unsafe { instance.get_physical_device_properties(*device) };
            unsafe { CStr::from_ptr(properties.device_name.as_ptr()) }
                .to_string_lossy()
                .contains("7900 XTX")
        })
        .or_else(|| devices.first().copied())
    else {
        eprintln!("no Vulkan physical devices found");
        unsafe { instance.destroy_instance(None) };
        std::process::exit(1);
    };

    let properties = unsafe { instance.get_physical_device_properties(physical_device) };
    let name = unsafe { CStr::from_ptr(properties.device_name.as_ptr()) };
    println!(
        "device: {} ({:?}, api {}.{}.{})",
        name.to_string_lossy(),
        properties.device_type,
        vk::api_version_major(properties.api_version),
        vk::api_version_minor(properties.api_version),
        vk::api_version_patch(properties.api_version),
    );

    let queue_family_index =
        unsafe { instance.get_physical_device_queue_family_properties(physical_device) }
            .iter()
            .position(|family| family.queue_flags.contains(vk::QueueFlags::COMPUTE))
            .unwrap_or(0) as u32;
    let priorities = [1.0f32];
    let queue_info = vk::DeviceQueueCreateInfo::default()
        .queue_family_index(queue_family_index)
        .queue_priorities(&priorities);
    let device_info =
        vk::DeviceCreateInfo::default().queue_create_infos(std::slice::from_ref(&queue_info));

    let device = match unsafe { instance.create_device(physical_device, &device_info, None) } {
        Ok(device) => device,
        Err(error) => {
            eprintln!("device creation failed: {error:?}");
            unsafe { instance.destroy_instance(None) };
            std::process::exit(1);
        }
    };

    let extensions = unsafe { instance.enumerate_device_extension_properties(physical_device) }
        .unwrap_or_default();
    let has_extension = |name: &CStr| {
        extensions
            .iter()
            .any(|extension| (unsafe { CStr::from_ptr(extension.extension_name.as_ptr()) }) == name)
    };
    let has_dot_product = properties.api_version >= vk::API_VERSION_1_3
        || has_extension(c"VK_KHR_shader_integer_dot_product");
    let has_coopmat = has_extension(c"VK_KHR_cooperative_matrix");

    if has_dot_product {
        let dp4a = unsafe {
            create_pipeline(
                &device,
                &include_bytes!(concat!(env!("OUT_DIR"), "/dp4a.comp.spv"))[..],
                3,
            )
        };
        match dp4a {
            Ok(pipeline) => {
                println!("dp4a=ok");
                unsafe { device.destroy_pipeline(pipeline, None) };
            }
            Err(error) => println!("dp4a=fallback ({error:?})"),
        }
    } else {
        println!("dp4a=fallback (extension unavailable)");
    }

    if has_coopmat {
        let coopmat = unsafe {
            create_pipeline(
                &device,
                &include_bytes!(concat!(env!("OUT_DIR"), "/coopmat.comp.spv"))[..],
                2,
            )
        };
        match coopmat {
            Ok(pipeline) => {
                println!("coopmat=ok");
                unsafe { device.destroy_pipeline(pipeline, None) };
            }
            Err(error) => println!("coopmat=fallback ({error:?})"),
        }
    } else {
        println!("coopmat=fallback (extension unavailable)");
    }

    unsafe {
        device.destroy_device(None);
        instance.destroy_instance(None);
    }
}

unsafe fn create_pipeline(
    device: &ash::Device,
    spirv: &[u8],
    binding_count: u32,
) -> Result<vk::Pipeline, vk::Result> {
    unsafe {
        let words = ash::util::read_spv(&mut Cursor::new(spirv)).unwrap();
        let module = device
            .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)?;

        let bindings: Vec<_> = (0..binding_count)
            .map(|binding| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(binding)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE)
            })
            .collect();
        let set_layout = device.create_descriptor_set_layout(
            &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
            None,
        )?;
        let layout = device.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default().set_layouts(std::slice::from_ref(&set_layout)),
            None,
        )?;

        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(module)
            .name(c"main");
        let pipeline_info = vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(layout);
        let pipeline = device.create_compute_pipelines(
            vk::PipelineCache::null(),
            std::slice::from_ref(&pipeline_info),
            None,
        );

        device.destroy_shader_module(module, None);
        device.destroy_pipeline_layout(layout, None);
        device.destroy_descriptor_set_layout(set_layout, None);

        match pipeline {
            Ok(pipelines) => Ok(pipelines[0]),
            Err((_, error)) => Err(error),
        }
    }
}
