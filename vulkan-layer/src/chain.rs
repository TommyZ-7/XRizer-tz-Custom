//! Owned rewrite of `VkDeviceCreateInfo` for opt-in feature injection.
//!
//! The rewrite copies every known structure into an owned arena, merges the
//! planned feature bits, and leaves the original create info untouched. If any
//! unknown application structure appears, the caller must fall back to the
//! original create info unchanged.

use crate::features::FeaturePlan;
use ash::vk;
use std::collections::HashSet;
use std::ffi::{CStr, c_char, c_void};

const P_NEXT_OFFSET: usize = 8;

struct Arena {
    blocks: Vec<Box<[u64]>>,
}

impl Arena {
    fn new() -> Self {
        Self { blocks: Vec::new() }
    }

    fn alloc(&mut self, size: usize) -> *mut u8 {
        let words = size.div_ceil(8).max(1);
        let block = vec![0u64; words].into_boxed_slice();
        let pointer = block.as_ptr() as *mut u8;
        self.blocks.push(block);
        pointer
    }
}

#[derive(Clone, Copy)]
struct Node {
    s_type: vk::StructureType,
    pointer: *mut u8,
}

pub struct OwnedDeviceCreateInfo {
    create_info: vk::DeviceCreateInfo<'static>,
    _arena: Arena,
    #[allow(dead_code)]
    nodes: Vec<Node>,
    _extensions: Vec<*const c_char>,
}

impl OwnedDeviceCreateInfo {
    pub fn as_raw(&self) -> &vk::DeviceCreateInfo<'static> {
        &self.create_info
    }

    /// Test/diagnostic helper: find a structure by `sType`.
    #[cfg(test)]
    pub fn find_node(&self, s_type: vk::StructureType) -> Option<*mut c_void> {
        self.nodes
            .iter()
            .find(|node| node.s_type == s_type)
            .map(|node| node.pointer.cast())
    }

    /// Test/diagnostic helper: enabled extension names after the rewrite.
    #[cfg(test)]
    pub fn extension_names(&self) -> Vec<String> {
        (0..self.create_info.enabled_extension_count)
            .map(|index| unsafe {
                let pointer = *self
                    .create_info
                    .pp_enabled_extension_names
                    .add(index as usize);
                CStr::from_ptr(pointer).to_string_lossy().into_owned()
            })
            .collect()
    }
}

macro_rules! known_feature_structs {
    ($($ty:ty),* $(,)?) => {
        fn known_struct_size(s_type: vk::StructureType) -> Option<usize> {
            $(
                if s_type == <$ty as vk::TaggedStructure>::STRUCTURE_TYPE {
                    return Some(std::mem::size_of::<$ty>());
                }
            )*
            if s_type == vk::StructureType::LOADER_DEVICE_CREATE_INFO {
                return Some(std::mem::size_of::<crate::LayerDeviceCreateInfo>());
            }
            None
        }
    };
}

known_feature_structs!(
    vk::PhysicalDeviceFeatures2<'static>,
    vk::PhysicalDeviceVulkan11Features<'static>,
    vk::PhysicalDeviceVulkan12Features<'static>,
    vk::PhysicalDeviceVulkan13Features<'static>,
    vk::PhysicalDeviceRobustness2FeaturesEXT<'static>,
    vk::PhysicalDeviceCustomBorderColorFeaturesEXT<'static>,
    vk::PhysicalDeviceExtendedDynamicStateFeaturesEXT<'static>,
    vk::PhysicalDeviceExtendedDynamicState2FeaturesEXT<'static>,
    vk::PhysicalDeviceExtendedDynamicState3FeaturesEXT<'static>,
    vk::PhysicalDeviceColorWriteEnableFeaturesEXT<'static>,
    vk::PhysicalDeviceLineRasterizationFeaturesEXT<'static>,
    vk::PhysicalDeviceImagelessFramebufferFeatures<'static>,
    vk::PhysicalDeviceSeparateDepthStencilLayoutsFeatures<'static>,
    vk::PhysicalDeviceShaderIntegerDotProductFeatures<'static>,
    vk::PhysicalDeviceCooperativeMatrixFeaturesKHR<'static>,
    vk::PhysicalDeviceShaderFloat16Int8Features<'static>,
    vk::PhysicalDevice8BitStorageFeatures<'static>,
    vk::PhysicalDeviceVulkanMemoryModelFeatures<'static>,
    vk::PhysicalDeviceTimelineSemaphoreFeatures<'static>,
    vk::PhysicalDeviceBufferDeviceAddressFeatures<'static>,
    vk::PhysicalDeviceDescriptorIndexingFeatures<'static>,
    vk::PhysicalDeviceHostQueryResetFeatures<'static>,
    vk::PhysicalDeviceVariablePointersFeatures<'static>,
    vk::PhysicalDeviceSynchronization2Features<'static>,
    vk::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures<'static>,
    vk::PhysicalDeviceMaintenance4Features<'static>,
    vk::PhysicalDeviceDynamicRenderingFeatures<'static>,
    vk::PhysicalDeviceDepthClipEnableFeaturesEXT<'static>,
    vk::PhysicalDeviceMemoryPriorityFeaturesEXT<'static>,
    vk::PhysicalDevicePageableDeviceLocalMemoryFeaturesEXT<'static>,
    vk::PhysicalDeviceShaderSubgroupUniformControlFlowFeaturesKHR<'static>,
    vk::PhysicalDevicePrimitiveTopologyListRestartFeaturesEXT<'static>,
    vk::PhysicalDeviceProvokingVertexFeaturesEXT<'static>,
    vk::PhysicalDeviceDepthClampZeroOneFeaturesEXT<'static>,
    vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT<'static>,
    vk::PhysicalDeviceGraphicsPipelineLibraryFeaturesEXT<'static>,
    vk::PhysicalDeviceShaderModuleIdentifierFeaturesEXT<'static>,
    vk::PhysicalDeviceMultiviewFeatures<'static>,
    vk::PhysicalDeviceTextureCompressionASTCHDRFeatures<'static>,
    vk::PhysicalDeviceFragmentShaderInterlockFeaturesEXT<'static>,
    vk::PhysicalDeviceAttachmentFeedbackLoopLayoutFeaturesEXT<'static>,
    vk::PhysicalDeviceCoherentMemoryFeaturesAMD<'static>,
    vk::PhysicalDeviceShaderImageAtomicInt64FeaturesEXT<'static>,
    vk::PhysicalDeviceConditionalRenderingFeaturesEXT<'static>,
    vk::PhysicalDeviceTransformFeedbackFeaturesEXT<'static>,
    vk::PhysicalDeviceBlendOperationAdvancedFeaturesEXT<'static>,
    vk::PhysicalDeviceVertexAttributeDivisorFeaturesEXT<'static>,
    vk::PhysicalDeviceFragmentShaderBarycentricFeaturesKHR<'static>,
    vk::PhysicalDeviceFragmentShadingRateFeaturesKHR<'static>,
    vk::PhysicalDeviceDescriptorBufferFeaturesEXT<'static>,
    vk::PhysicalDeviceMaintenance5FeaturesKHR<'static>,
    vk::PhysicalDeviceHostImageCopyFeaturesEXT<'static>,
    vk::PhysicalDeviceImageRobustnessFeatures<'static>,
    vk::PhysicalDevicePipelineRobustnessFeaturesEXT<'static>,
    vk::PhysicalDevicePrivateDataFeatures<'static>,
    vk::PhysicalDeviceProtectedMemoryFeatures<'static>,
    vk::PhysicalDevice16BitStorageFeatures<'static>,
    vk::PhysicalDeviceShaderDrawParametersFeatures<'static>,
    vk::PhysicalDeviceShaderAtomicInt64Features<'static>,
    vk::PhysicalDeviceUniformBufferStandardLayoutFeatures<'static>,
    vk::PhysicalDeviceScalarBlockLayoutFeatures<'static>,
    vk::PhysicalDeviceSubgroupSizeControlFeatures<'static>,
    vk::PhysicalDeviceVertexInputDynamicStateFeaturesEXT<'static>,
    vk::PhysicalDeviceRasterizationOrderAttachmentAccessFeaturesEXT<'static>,
    vk::PhysicalDeviceRGBA10X6FormatsFeaturesEXT<'static>,
    vk::PhysicalDeviceMutableDescriptorTypeFeaturesEXT<'static>,
    vk::PhysicalDeviceGlobalPriorityQueryFeaturesKHR<'static>,
    vk::PhysicalDeviceMultiDrawFeaturesEXT<'static>,
    vk::PhysicalDeviceNonSeamlessCubeMapFeaturesEXT<'static>,
    vk::PhysicalDevicePipelineExecutablePropertiesFeaturesKHR<'static>,
);

unsafe fn next_structure(pointer: *const c_void) -> *const c_void {
    unsafe { *(pointer.cast::<*const c_void>().add(1)) }
}

unsafe fn write_next(pointer: *mut u8, next: *mut c_void) {
    unsafe {
        *(pointer.add(P_NEXT_OFFSET) as *mut *mut c_void) = next;
    }
}

unsafe fn find_node(nodes: &[Node], s_type: vk::StructureType) -> Option<*mut u8> {
    nodes
        .iter()
        .find(|node| node.s_type == s_type)
        .map(|node| node.pointer)
}

unsafe fn ensure_node(
    nodes: &mut Vec<Node>,
    arena: &mut Arena,
    s_type: vk::StructureType,
) -> *mut u8 {
    if let Some(pointer) = unsafe { find_node(nodes, s_type) } {
        return pointer;
    }
    let size = known_struct_size(s_type).expect("requested structure must be in the known table");
    let pointer = arena.alloc(size);
    unsafe {
        *(pointer as *mut vk::StructureType) = s_type;
    }
    nodes.push(Node { s_type, pointer });
    pointer
}

pub fn rewrite_device_create_info(
    plan: &FeaturePlan,
    original: &vk::DeviceCreateInfo<'_>,
) -> Option<OwnedDeviceCreateInfo> {
    if plan.is_empty() {
        return None;
    }
    unsafe { rewrite_inner(plan, original) }
}

unsafe fn rewrite_inner(
    plan: &FeaturePlan,
    original: &vk::DeviceCreateInfo<'_>,
) -> Option<OwnedDeviceCreateInfo> {
    let mut arena = Arena::new();
    let mut nodes = Vec::new();

    let mut pointer = original.p_next;
    while !pointer.is_null() {
        let s_type = unsafe { *(pointer as *const vk::StructureType) };
        let size = known_struct_size(s_type)?;
        let destination = arena.alloc(size);
        unsafe {
            std::ptr::copy_nonoverlapping(pointer as *const u8, destination, size);
        }
        nodes.push(Node {
            s_type,
            pointer: destination,
        });
        pointer = unsafe { next_structure(pointer) };
    }

    // The lifetime parameter on DeviceCreateInfo is phantom data; the raw
    // pointers are what actually matter and are all owned by this rewrite.
    let mut create_info: vk::DeviceCreateInfo<'static> =
        unsafe { std::mem::transmute_copy(original) };
    let mut features2 = unsafe { find_node(&nodes, vk::StructureType::PHYSICAL_DEVICE_FEATURES_2) };
    if features2.is_none() {
        let pointer = unsafe {
            ensure_node(
                &mut nodes,
                &mut arena,
                vk::StructureType::PHYSICAL_DEVICE_FEATURES_2,
            )
        };
        if !original.p_enabled_features.is_null() {
            unsafe {
                (*(pointer as *mut vk::PhysicalDeviceFeatures2<'static>)).features =
                    *original.p_enabled_features;
            }
        }
        features2 = Some(pointer);
    }
    if let Some(pointer) = features2 {
        if !original.p_enabled_features.is_null() {
            unsafe {
                (*(pointer as *mut vk::PhysicalDeviceFeatures2<'static>)).features =
                    *original.p_enabled_features;
            }
        }
        create_info.p_enabled_features = std::ptr::null();
    }

    if plan.dot_product {
        let mut applied = false;
        if let Some(pointer) = unsafe {
            find_node(
                &nodes,
                vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_3_FEATURES,
            )
        } {
            unsafe {
                (*(pointer as *mut vk::PhysicalDeviceVulkan13Features<'static>))
                    .shader_integer_dot_product = vk::TRUE;
            }
            applied = true;
        }
        if let Some(pointer) = unsafe {
            find_node(
                &nodes,
                vk::StructureType::PHYSICAL_DEVICE_SHADER_INTEGER_DOT_PRODUCT_FEATURES,
            )
        } {
            unsafe {
                (*(pointer as *mut vk::PhysicalDeviceShaderIntegerDotProductFeatures<'static>))
                    .shader_integer_dot_product = vk::TRUE;
            }
            applied = true;
        }
        if !applied {
            let pointer = unsafe {
                ensure_node(
                    &mut nodes,
                    &mut arena,
                    vk::StructureType::PHYSICAL_DEVICE_SHADER_INTEGER_DOT_PRODUCT_FEATURES,
                )
            };
            unsafe {
                (*(pointer as *mut vk::PhysicalDeviceShaderIntegerDotProductFeatures<'static>))
                    .shader_integer_dot_product = vk::TRUE;
            }
        }
    }

    if plan.coopmat {
        let pointer = unsafe {
            ensure_node(
                &mut nodes,
                &mut arena,
                vk::StructureType::PHYSICAL_DEVICE_COOPERATIVE_MATRIX_FEATURES_KHR,
            )
        };
        unsafe {
            (*(pointer as *mut vk::PhysicalDeviceCooperativeMatrixFeaturesKHR<'static>))
                .cooperative_matrix = vk::TRUE;
        }
    }

    if plan.shader_int8 {
        unsafe {
            set_vulkan12_or_specific::<
                vk::PhysicalDeviceVulkan12Features<'static>,
                vk::PhysicalDeviceShaderFloat16Int8Features<'static>,
            >(
                &mut nodes,
                &mut arena,
                vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_2_FEATURES,
                vk::StructureType::PHYSICAL_DEVICE_SHADER_FLOAT16_INT8_FEATURES,
                |v12: &mut vk::PhysicalDeviceVulkan12Features<'static>| &mut v12.shader_int8,
                |specific: &mut vk::PhysicalDeviceShaderFloat16Int8Features<'static>| {
                    &mut specific.shader_int8
                },
            );
        }
    }
    if plan.storage_8bit {
        unsafe {
            set_vulkan12_or_specific::<
                vk::PhysicalDeviceVulkan12Features<'static>,
                vk::PhysicalDevice8BitStorageFeatures<'static>,
            >(
                &mut nodes,
                &mut arena,
                vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_2_FEATURES,
                vk::StructureType::PHYSICAL_DEVICE_8BIT_STORAGE_FEATURES,
                |v12: &mut vk::PhysicalDeviceVulkan12Features<'static>| {
                    &mut v12.storage_buffer8_bit_access
                },
                |specific: &mut vk::PhysicalDevice8BitStorageFeatures<'static>| {
                    &mut specific.storage_buffer8_bit_access
                },
            );
        }
    }
    if plan.memory_model {
        unsafe {
            set_vulkan12_or_specific::<
                vk::PhysicalDeviceVulkan12Features<'static>,
                vk::PhysicalDeviceVulkanMemoryModelFeatures<'static>,
            >(
                &mut nodes,
                &mut arena,
                vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_2_FEATURES,
                vk::StructureType::PHYSICAL_DEVICE_VULKAN_MEMORY_MODEL_FEATURES,
                |v12: &mut vk::PhysicalDeviceVulkan12Features<'static>| {
                    &mut v12.vulkan_memory_model
                },
                |specific: &mut vk::PhysicalDeviceVulkanMemoryModelFeatures<'static>| {
                    &mut specific.vulkan_memory_model
                },
            );
        }
    }

    let mut extension_pointers: Vec<*const c_char> = Vec::new();
    let mut seen_extensions: HashSet<Vec<u8>> = HashSet::new();
    if original.enabled_extension_count > 0 {
        if original.pp_enabled_extension_names.is_null() {
            return None;
        }
        for index in 0..original.enabled_extension_count {
            let extension = unsafe { *original.pp_enabled_extension_names.add(index as usize) };
            if extension.is_null() {
                return None;
            }
            let bytes = unsafe { CStr::from_ptr(extension) }.to_bytes().to_vec();
            if seen_extensions.insert(bytes) {
                extension_pointers.push(extension);
            }
        }
    }
    for extension in &plan.extensions {
        if seen_extensions.insert(extension.to_bytes().to_vec()) {
            extension_pointers.push(extension.as_ptr());
        }
    }

    let mut previous: Option<*mut u8> = None;
    for node in &nodes {
        if let Some(previous) = previous {
            unsafe {
                write_next(previous, node.pointer.cast());
            }
        }
        previous = Some(node.pointer);
    }
    if let Some(last) = previous {
        unsafe {
            write_next(last, std::ptr::null_mut());
        }
    }

    create_info.p_next = nodes
        .first()
        .map(|node| node.pointer as *const c_void)
        .unwrap_or(std::ptr::null());
    create_info.enabled_extension_count = extension_pointers.len() as u32;
    create_info.pp_enabled_extension_names = if extension_pointers.is_empty() {
        std::ptr::null()
    } else {
        extension_pointers.as_ptr()
    };

    Some(OwnedDeviceCreateInfo {
        create_info,
        _arena: arena,
        nodes,
        _extensions: extension_pointers,
    })
}

unsafe fn set_vulkan12_or_specific<V12, Specific>(
    nodes: &mut Vec<Node>,
    arena: &mut Arena,
    vulkan12_type: vk::StructureType,
    specific_type: vk::StructureType,
    vulkan12_field: impl FnOnce(&mut V12) -> &mut vk::Bool32,
    specific_field: impl Fn(&mut Specific) -> &mut vk::Bool32,
) where
    V12: Default + 'static,
    Specific: Default + 'static,
{
    let mut applied = false;
    if let Some(pointer) = unsafe { find_node(nodes, vulkan12_type) } {
        let value: &mut V12 = unsafe { &mut *(pointer as *mut V12) };
        *vulkan12_field(value) = vk::TRUE;
        applied = true;
    }
    if let Some(pointer) = unsafe { find_node(nodes, specific_type) } {
        let value: &mut Specific = unsafe { &mut *(pointer as *mut Specific) };
        *specific_field(value) = vk::TRUE;
        applied = true;
    }
    if !applied {
        let pointer = unsafe { ensure_node(nodes, arena, specific_type) };
        let value: &mut Specific = unsafe { &mut *(pointer as *mut Specific) };
        *specific_field(value) = vk::TRUE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{
        DeviceSupport, EXT_SHADER_INTEGER_DOT_PRODUCT, FeaturePlan, build_feature_plan,
    };
    use crate::{LayerDeviceCreateInfo, LayerDeviceLink};
    use std::any::Any;

    struct TestChain {
        _keep: Vec<Box<dyn Any>>,
        create_info: vk::DeviceCreateInfo<'static>,
    }

    impl TestChain {
        fn new(keep: Vec<Box<dyn Any>>, head: *mut c_void) -> Self {
            let create_info = vk::DeviceCreateInfo {
                p_next: head as *const c_void,
                ..Default::default()
            };
            Self {
                _keep: keep,
                create_info,
            }
        }
    }

    fn plan_dot_product() -> FeaturePlan {
        FeaturePlan {
            dot_product: true,
            ..Default::default()
        }
    }

    fn support_with_extensions(extensions: &[&'static CStr]) -> DeviceSupport {
        DeviceSupport {
            api_version: vk::API_VERSION_1_1,
            shader_integer_dot_product: true,
            cooperative_matrix: false,
            shader_int8: false,
            storage_buffer8_bit_access: false,
            vulkan_memory_model: false,
            extensions: extensions
                .iter()
                .map(|extension| std::ffi::CString::from(*extension))
                .collect(),
        }
    }

    #[test]
    fn supported_dot_product_adds_feature_to_chain() {
        let plan = build_feature_plan(&support_with_extensions(&[EXT_SHADER_INTEGER_DOT_PRODUCT]));
        let mut v13 = Box::new(vk::PhysicalDeviceVulkan13Features::default());
        v13.p_next = std::ptr::null_mut();
        let mut features2 = Box::new(vk::PhysicalDeviceFeatures2::default());
        features2.p_next = &mut *v13 as *mut _ as *mut c_void;
        let head = &mut *features2 as *mut _ as *mut c_void;
        let chain = TestChain::new(vec![v13, features2], head);

        let owned = rewrite_device_create_info(&plan, &chain.create_info).unwrap();
        let pointer = owned
            .find_node(vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_3_FEATURES)
            .unwrap();
        let v13 = unsafe { &*(pointer as *const vk::PhysicalDeviceVulkan13Features<'static>) };
        assert_eq!(v13.shader_integer_dot_product, vk::TRUE);
        assert_eq!(
            owned.extension_names(),
            vec!["VK_KHR_shader_integer_dot_product".to_string()]
        );
    }

    #[test]
    fn unsupported_dot_product_leaves_chain_unchanged() {
        let support = DeviceSupport {
            api_version: vk::API_VERSION_1_3,
            shader_integer_dot_product: false,
            cooperative_matrix: false,
            shader_int8: false,
            storage_buffer8_bit_access: false,
            vulkan_memory_model: false,
            extensions: Vec::new(),
        };
        let plan = build_feature_plan(&support);
        assert!(plan.is_empty());

        let mut features2 = Box::new(vk::PhysicalDeviceFeatures2::default());
        features2.p_next = std::ptr::null_mut();
        let head = &mut *features2 as *mut _ as *mut c_void;
        let chain = TestChain::new(vec![features2], head);
        assert!(rewrite_device_create_info(&plan, &chain.create_info).is_none());
    }

    #[test]
    fn features2_is_added_when_missing() {
        let create_info = vk::DeviceCreateInfo::default();
        let owned = rewrite_device_create_info(&plan_dot_product(), &create_info).unwrap();
        assert!(
            owned
                .find_node(vk::StructureType::PHYSICAL_DEVICE_FEATURES_2)
                .is_some()
        );
        assert!(!owned.as_raw().p_next.is_null());
    }

    #[test]
    fn unknown_pnext_structure_fails_open() {
        #[repr(C)]
        struct UnknownNode {
            s_type: vk::StructureType,
            p_next: *mut c_void,
        }

        let mut unknown = Box::new(UnknownNode {
            s_type: vk::StructureType::from_raw(0x1234_5678),
            p_next: std::ptr::null_mut(),
        });
        let head = &mut *unknown as *mut _ as *mut c_void;
        let chain = TestChain::new(vec![unknown], head);
        assert!(rewrite_device_create_info(&plan_dot_product(), &chain.create_info).is_none());
    }

    #[test]
    fn existing_extensions_are_not_duplicated() {
        let extension = std::ffi::CString::new("VK_KHR_shader_integer_dot_product").unwrap();
        let pointers = [extension.as_ptr()];
        let mut features2 = Box::new(vk::PhysicalDeviceFeatures2::default());
        features2.p_next = std::ptr::null_mut();
        let head = &mut *features2 as *mut _ as *mut c_void;
        let mut chain = TestChain::new(vec![features2], head);
        chain.create_info.enabled_extension_count = 1;
        chain.create_info.pp_enabled_extension_names = pointers.as_ptr();

        let owned = rewrite_device_create_info(&plan_dot_product(), &chain.create_info).unwrap();
        assert_eq!(
            owned.extension_names(),
            vec!["VK_KHR_shader_integer_dot_product".to_string()]
        );
    }

    #[test]
    fn p_enabled_features_are_preserved_when_adding_features2() {
        let features = vk::PhysicalDeviceFeatures {
            robust_buffer_access: vk::TRUE,
            ..Default::default()
        };
        let create_info = vk::DeviceCreateInfo {
            p_enabled_features: &features,
            ..Default::default()
        };

        let owned = rewrite_device_create_info(&plan_dot_product(), &create_info).unwrap();
        assert!(owned.create_info.p_enabled_features.is_null());
        let pointer = owned
            .find_node(vk::StructureType::PHYSICAL_DEVICE_FEATURES_2)
            .unwrap();
        let features2 = unsafe { &*(pointer as *const vk::PhysicalDeviceFeatures2<'static>) };
        assert_eq!(features2.features.robust_buffer_access, vk::TRUE);
    }

    #[test]
    fn dxvk_like_chain_is_rewritten_and_preserved() {
        unsafe extern "system" fn dummy_gipa(
            _: vk::Instance,
            _: *const c_char,
        ) -> vk::PFN_vkVoidFunction {
            None
        }
        unsafe extern "system" fn dummy_gdpa(
            _: vk::Device,
            _: *const c_char,
        ) -> vk::PFN_vkVoidFunction {
            None
        }

        let mut v13 = Box::new(vk::PhysicalDeviceVulkan13Features::default());
        v13.p_next = std::ptr::null_mut();
        let mut v12 = Box::new(vk::PhysicalDeviceVulkan12Features::default());
        v12.p_next = &mut *v13 as *mut _ as *mut c_void;
        let mut v11 = Box::new(vk::PhysicalDeviceVulkan11Features::default());
        v11.p_next = &mut *v12 as *mut _ as *mut c_void;
        let mut robustness = Box::new(vk::PhysicalDeviceRobustness2FeaturesEXT::default());
        robustness.p_next = &mut *v11 as *mut _ as *mut c_void;
        let mut custom_border = Box::new(vk::PhysicalDeviceCustomBorderColorFeaturesEXT::default());
        custom_border.p_next = &mut *robustness as *mut _ as *mut c_void;
        let mut link = Box::new(LayerDeviceLink {
            p_next: std::ptr::null_mut(),
            pfn_next_get_instance_proc_addr: dummy_gipa,
            pfn_next_get_device_proc_addr: dummy_gdpa,
        });
        let mut loader = Box::new(LayerDeviceCreateInfo {
            s_type: vk::StructureType::LOADER_DEVICE_CREATE_INFO,
            p_next: &mut *custom_border as *mut _ as *const c_void,
            function: 0,
            _padding: 0,
            p_layer_info: &mut *link,
        });
        let head = &mut *loader as *mut _ as *mut c_void;
        let chain = TestChain::new(
            vec![v13, v12, v11, robustness, custom_border, link, loader],
            head,
        );

        let owned = rewrite_device_create_info(&plan_dot_product(), &chain.create_info).unwrap();
        for s_type in [
            vk::StructureType::LOADER_DEVICE_CREATE_INFO,
            vk::StructureType::PHYSICAL_DEVICE_CUSTOM_BORDER_COLOR_FEATURES_EXT,
            vk::StructureType::PHYSICAL_DEVICE_ROBUSTNESS_2_FEATURES_EXT,
            vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_1_FEATURES,
            vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_2_FEATURES,
            vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_3_FEATURES,
        ] {
            assert!(owned.find_node(s_type).is_some(), "missing {s_type:?}");
        }
        let pointer = owned
            .find_node(vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_3_FEATURES)
            .unwrap();
        let v13 = unsafe { &*(pointer as *const vk::PhysicalDeviceVulkan13Features<'static>) };
        assert_eq!(v13.shader_integer_dot_product, vk::TRUE);
    }

    #[test]
    fn coopmat_dependencies_are_merged_into_vulkan12() {
        let plan = FeaturePlan {
            coopmat: true,
            shader_int8: true,
            storage_8bit: true,
            memory_model: true,
            ..Default::default()
        };
        let mut v12 = Box::new(vk::PhysicalDeviceVulkan12Features::default());
        v12.p_next = std::ptr::null_mut();
        let head = &mut *v12 as *mut _ as *mut c_void;
        let chain = TestChain::new(vec![v12], head);

        let owned = rewrite_device_create_info(&plan, &chain.create_info).unwrap();
        let pointer = owned
            .find_node(vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_2_FEATURES)
            .unwrap();
        let v12 = unsafe { &*(pointer as *const vk::PhysicalDeviceVulkan12Features<'static>) };
        assert_eq!(v12.shader_int8, vk::TRUE);
        assert_eq!(v12.storage_buffer8_bit_access, vk::TRUE);
        assert_eq!(v12.vulkan_memory_model, vk::TRUE);
        assert!(
            owned
                .find_node(vk::StructureType::PHYSICAL_DEVICE_COOPERATIVE_MATRIX_FEATURES_KHR)
                .is_some()
        );
    }
}
