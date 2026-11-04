//! Pass-through scaffold for the opt-in XRizer Vulkan implicit layer.
//!
//! The layer is deliberately conservative: until the feature plan is built it
//! forwards every call to the next layer/loader exactly as it received it. The
//! dispatch chain is kept in thread-safe maps keyed by the loader's handles so
//! multiple instances and devices can coexist.

use ash::vk;
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

const VK_LAYER_LINK_INFO: u32 = 0;
const LAYER_NEGOTIATE_INTERFACE_STRUCT: u32 = 1;
const CURRENT_LOADER_LAYER_INTERFACE_VERSION: u32 = 2;

type NextGetPhysicalDeviceProcAddr =
    unsafe extern "system" fn(vk::Instance, *const c_char) -> vk::PFN_vkVoidFunction;

#[repr(C)]
struct LayerInstanceLink {
    p_next: *mut LayerInstanceLink,
    pfn_next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pfn_next_get_physical_device_proc_addr: Option<NextGetPhysicalDeviceProcAddr>,
}

#[repr(C)]
struct LayerInstanceCreateInfo {
    s_type: vk::StructureType,
    p_next: *const c_void,
    function: u32,
    _padding: u32,
    p_layer_info: *mut LayerInstanceLink,
}

#[repr(C)]
struct LayerDeviceLink {
    p_next: *mut LayerDeviceLink,
    pfn_next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pfn_next_get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
}

#[repr(C)]
struct LayerDeviceCreateInfo {
    s_type: vk::StructureType,
    p_next: *const c_void,
    function: u32,
    _padding: u32,
    p_layer_info: *mut LayerDeviceLink,
}

#[derive(Clone, Copy)]
struct InstanceState {
    next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    next_get_device_proc_addr: Option<vk::PFN_vkGetDeviceProcAddr>,
}

#[derive(Clone, Copy)]
struct DeviceState {
    next_get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
    _instance: vk::Instance,
}

static INSTANCES: LazyLock<Mutex<HashMap<usize, InstanceState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static DEVICES: LazyLock<Mutex<HashMap<usize, DeviceState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static PHYSICAL_DEVICES: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static LAST_INSTANCE: AtomicUsize = AtomicUsize::new(0);

fn raw(handle: impl vk::Handle) -> usize {
    handle.as_raw() as usize
}

unsafe fn instance_state(instance: vk::Instance) -> Option<InstanceState> {
    INSTANCES.lock().unwrap().get(&raw(instance)).copied()
}

unsafe fn device_state(device: vk::Device) -> Option<DeviceState> {
    DEVICES.lock().unwrap().get(&raw(device)).copied()
}

/// Every Vulkan structure starts with `sType` followed by `pNext`. On the
/// 64-bit Linux target this puts `pNext` at byte offset 8.
unsafe fn next_structure(pointer: *const c_void) -> *const c_void {
    unsafe { *(pointer.cast::<*const c_void>().add(1)) }
}

unsafe fn find_instance_link(mut pointer: *const c_void) -> Option<*mut LayerInstanceCreateInfo> {
    while !pointer.is_null() {
        let candidate = pointer as *mut LayerInstanceCreateInfo;
        unsafe {
            if (*candidate).s_type == vk::StructureType::LOADER_INSTANCE_CREATE_INFO
                && (*candidate).function == VK_LAYER_LINK_INFO
            {
                return Some(candidate);
            }
            pointer = next_structure(pointer);
        }
    }
    None
}

unsafe fn find_device_link(mut pointer: *const c_void) -> Option<*mut LayerDeviceCreateInfo> {
    while !pointer.is_null() {
        let candidate = pointer as *mut LayerDeviceCreateInfo;
        unsafe {
            if (*candidate).s_type == vk::StructureType::LOADER_DEVICE_CREATE_INFO
                && (*candidate).function == VK_LAYER_LINK_INFO
            {
                return Some(candidate);
            }
            pointer = next_structure(pointer);
        }
    }
    None
}

unsafe fn advance_instance_link(link: *mut LayerInstanceCreateInfo) {
    unsafe {
        if let Some(layer_info) = (*link).p_layer_info.as_mut() {
            (*link).p_layer_info = layer_info.p_next;
        }
    }
}

unsafe fn advance_device_link(link: *mut LayerDeviceCreateInfo) {
    unsafe {
        if let Some(layer_info) = (*link).p_layer_info.as_mut() {
            (*link).p_layer_info = layer_info.p_next;
        }
    }
}

unsafe fn void_fn<T>(function: T) -> vk::PFN_vkVoidFunction {
    unsafe { std::mem::transmute_copy(&function) }
}

unsafe extern "system" fn create_instance(
    create_info: *const vk::InstanceCreateInfo,
    allocator: *const vk::AllocationCallbacks<'_>,
    instance: *mut vk::Instance,
) -> vk::Result {
    unsafe {
        let Some(link) = find_instance_link((*create_info).p_next) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let Some(next_link) = (*link).p_layer_info.as_ref() else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let next_gipa = next_link.pfn_next_get_instance_proc_addr;
        advance_instance_link(link);

        let Some(next_create) = next_gipa(vk::Instance::null(), c"vkCreateInstance".as_ptr())
        else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let next_create: vk::PFN_vkCreateInstance = std::mem::transmute(next_create);
        let result = next_create(create_info, allocator, instance);
        if result == vk::Result::SUCCESS {
            let created = *instance;
            let next_gdpa = next_gipa(created, c"vkGetDeviceProcAddr".as_ptr())
                .map(|function| std::mem::transmute(function));
            INSTANCES.lock().unwrap().insert(
                raw(created),
                InstanceState {
                    next_get_instance_proc_addr: next_gipa,
                    next_get_device_proc_addr: next_gdpa,
                },
            );
            LAST_INSTANCE.store(raw(created), Ordering::Relaxed);
        }
        result
    }
}

unsafe extern "system" fn destroy_instance(
    instance: vk::Instance,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    unsafe {
        let Some(state) = instance_state(instance) else {
            return;
        };
        let Some(destroy) = (state
            .next_get_instance_proc_addr)(instance, c"vkDestroyInstance".as_ptr())
        else {
            return;
        };
        let destroy: vk::PFN_vkDestroyInstance = std::mem::transmute(destroy);
        destroy(instance, allocator);

        INSTANCES.lock().unwrap().remove(&raw(instance));
        PHYSICAL_DEVICES
            .lock()
            .unwrap()
            .retain(|_, owner| *owner != raw(instance));
    }
}

unsafe extern "system" fn enumerate_physical_devices(
    instance: vk::Instance,
    count: *mut u32,
    devices: *mut vk::PhysicalDevice,
) -> vk::Result {
    unsafe {
        let Some(state) = instance_state(instance) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let Some(function) = (state
            .next_get_instance_proc_addr)(instance, c"vkEnumeratePhysicalDevices".as_ptr())
        else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let function: vk::PFN_vkEnumeratePhysicalDevices = std::mem::transmute(function);
        let result = function(instance, count, devices);
        if result == vk::Result::SUCCESS && !devices.is_null() && !count.is_null() {
            let mut map = PHYSICAL_DEVICES.lock().unwrap();
            for device in std::slice::from_raw_parts(devices, *count as usize) {
                map.insert(raw(*device), raw(instance));
            }
        }
        result
    }
}

unsafe extern "system" fn create_device(
    physical_device: vk::PhysicalDevice,
    create_info: *const vk::DeviceCreateInfo,
    allocator: *const vk::AllocationCallbacks<'_>,
    device: *mut vk::Device,
) -> vk::Result {
    unsafe {
        let Some(link) = find_device_link((*create_info).p_next) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let Some(next_link) = (*link).p_layer_info.as_ref() else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let next_gipa = next_link.pfn_next_get_instance_proc_addr;
        let next_gdpa = next_link.pfn_next_get_device_proc_addr;
        advance_device_link(link);

        let instance = PHYSICAL_DEVICES
            .lock()
            .unwrap()
            .get(&raw(physical_device))
            .copied()
            .map(|value| <vk::Instance as vk::Handle>::from_raw(value as u64))
            .unwrap_or_else(|| {
                <vk::Instance as vk::Handle>::from_raw(
                    LAST_INSTANCE.load(Ordering::Relaxed) as u64
                )
            });

        let Some(next_create) = next_gipa(instance, c"vkCreateDevice".as_ptr()) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let next_create: vk::PFN_vkCreateDevice = std::mem::transmute(next_create);
        let result = next_create(physical_device, create_info, allocator, device);
        if result == vk::Result::SUCCESS {
            DEVICES.lock().unwrap().insert(
                raw(*device),
                DeviceState {
                    next_get_device_proc_addr: next_gdpa,
                    _instance: instance,
                },
            );
        }
        result
    }
}

unsafe extern "system" fn destroy_device(
    device: vk::Device,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    unsafe {
        let Some(state) = device_state(device) else {
            return;
        };
        let Some(destroy) = (state
            .next_get_device_proc_addr)(device, c"vkDestroyDevice".as_ptr())
        else {
            return;
        };
        let destroy: vk::PFN_vkDestroyDevice = std::mem::transmute(destroy);
        destroy(device, allocator);
        DEVICES.lock().unwrap().remove(&raw(device));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn xrizer_layer_GetInstanceProcAddr(
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if name.is_null() {
        return None;
    }
    let name = unsafe { CStr::from_ptr(name) }.to_bytes();
    match name {
        b"vkGetInstanceProcAddr" => unsafe { void_fn::<vk::PFN_vkGetInstanceProcAddr>(xrizer_layer_GetInstanceProcAddr) },
        b"vkCreateInstance" => unsafe { void_fn::<vk::PFN_vkCreateInstance>(create_instance) },
        b"vkDestroyInstance" => unsafe { void_fn::<vk::PFN_vkDestroyInstance>(destroy_instance) },
        b"vkEnumeratePhysicalDevices" => unsafe { void_fn::<vk::PFN_vkEnumeratePhysicalDevices>(enumerate_physical_devices) },
        b"vkCreateDevice" => unsafe { void_fn::<vk::PFN_vkCreateDevice>(create_device) },
        b"vkDestroyDevice" => unsafe { void_fn::<vk::PFN_vkDestroyDevice>(destroy_device) },
        _ => {
            if instance == vk::Instance::null() {
                return None;
            }
            let state = unsafe { instance_state(instance) }?;
            unsafe { (state.next_get_instance_proc_addr)(instance, name.as_ptr().cast()) }
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn xrizer_layer_GetDeviceProcAddr(
    device: vk::Device,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if name.is_null() {
        return None;
    }
    let name = unsafe { CStr::from_ptr(name) }.to_bytes();
    match name {
        b"vkGetDeviceProcAddr" => unsafe { void_fn::<vk::PFN_vkGetDeviceProcAddr>(xrizer_layer_GetDeviceProcAddr) },
        b"vkDestroyDevice" => unsafe { void_fn::<vk::PFN_vkDestroyDevice>(destroy_device) },
        _ => {
            let state = unsafe { device_state(device) }?;
            unsafe { (state.next_get_device_proc_addr)(device, name.as_ptr().cast()) }
        }
    }
}

#[repr(C)]
pub struct VkNegotiateLayerInterface {
    s_type: u32,
    p_next: *mut c_void,
    loader_layer_interface_version: u32,
    pfn_get_instance_proc_addr: Option<vk::PFN_vkGetInstanceProcAddr>,
    pfn_get_device_proc_addr: Option<vk::PFN_vkGetDeviceProcAddr>,
    pfn_get_physical_device_proc_addr: Option<NextGetPhysicalDeviceProcAddr>,
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkNegotiateLoaderLayerInterfaceVersion(
    interface: *mut VkNegotiateLayerInterface,
) -> vk::Result {
    let Some(interface) = (unsafe { interface.as_mut() }) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    interface.s_type = LAYER_NEGOTIATE_INTERFACE_STRUCT;
    interface.p_next = std::ptr::null_mut();
    interface.loader_layer_interface_version = CURRENT_LOADER_LAYER_INTERFACE_VERSION;
    interface.pfn_get_instance_proc_addr = Some(xrizer_layer_GetInstanceProcAddr);
    interface.pfn_get_device_proc_addr = Some(xrizer_layer_GetDeviceProcAddr);
    interface.pfn_get_physical_device_proc_addr = None;
    vk::Result::SUCCESS
}
