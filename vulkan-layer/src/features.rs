//! Pure feature-plan decisions for the opt-in layer.
//!
//! The layer only ever enables features that the physical device reports and
//! whose extensions are available. All decisions live here so they can be
//! tested without a GPU.

use ash::vk;
use std::ffi::{CStr, CString};

pub const EXT_SHADER_INTEGER_DOT_PRODUCT: &CStr = c"VK_KHR_shader_integer_dot_product";
pub const EXT_COOPERATIVE_MATRIX: &CStr = c"VK_KHR_cooperative_matrix";
pub const EXT_SHADER_FLOAT16_INT8: &CStr = c"VK_KHR_shader_float16_int8";
pub const EXT_8BIT_STORAGE: &CStr = c"VK_KHR_8bit_storage";
pub const EXT_VULKAN_MEMORY_MODEL: &CStr = c"VK_KHR_vulkan_memory_model";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FeaturePlan {
    pub dot_product: bool,
    pub coopmat: bool,
    pub shader_int8: bool,
    pub storage_8bit: bool,
    pub memory_model: bool,
    pub extensions: Vec<&'static CStr>,
}

impl FeaturePlan {
    pub fn is_empty(&self) -> bool {
        !self.dot_product
            && !self.coopmat
            && !self.shader_int8
            && !self.storage_8bit
            && !self.memory_model
            && self.extensions.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct DeviceSupport {
    pub api_version: u32,
    pub shader_integer_dot_product: bool,
    pub cooperative_matrix: bool,
    pub shader_int8: bool,
    pub storage_buffer8_bit_access: bool,
    pub vulkan_memory_model: bool,
    pub extensions: Vec<CString>,
}

impl DeviceSupport {
    pub fn has_extension(&self, extension: &CStr) -> bool {
        self.extensions
            .iter()
            .any(|candidate| candidate.as_c_str() == extension)
    }
}

fn push_extension(plan: &mut FeaturePlan, extension: &'static CStr) {
    if !plan.extensions.contains(&extension) {
        plan.extensions.push(extension);
    }
}

/// Build the feature plan from queried device support. DP4A is the primary
/// target; cooperative matrix is enabled only when every dependency is present.
pub fn build_feature_plan(support: &DeviceSupport) -> FeaturePlan {
    let mut plan = FeaturePlan::default();

    let dot_product = support.shader_integer_dot_product
        && (support.api_version >= vk::API_VERSION_1_3
            || support.has_extension(EXT_SHADER_INTEGER_DOT_PRODUCT));
    if dot_product {
        plan.dot_product = true;
        if support.api_version < vk::API_VERSION_1_3 {
            push_extension(&mut plan, EXT_SHADER_INTEGER_DOT_PRODUCT);
        }
    }

    let coopmat = support.cooperative_matrix
        && support.shader_int8
        && support.storage_buffer8_bit_access
        && support.vulkan_memory_model
        && support.has_extension(EXT_COOPERATIVE_MATRIX);
    if coopmat {
        plan.coopmat = true;
        plan.shader_int8 = true;
        plan.storage_8bit = true;
        plan.memory_model = true;
        push_extension(&mut plan, EXT_COOPERATIVE_MATRIX);
    }

    if plan.shader_int8
        && support.api_version < vk::API_VERSION_1_2
        && support.has_extension(EXT_SHADER_FLOAT16_INT8)
    {
        push_extension(&mut plan, EXT_SHADER_FLOAT16_INT8);
    }
    if plan.storage_8bit
        && support.api_version < vk::API_VERSION_1_2
        && support.has_extension(EXT_8BIT_STORAGE)
    {
        push_extension(&mut plan, EXT_8BIT_STORAGE);
    }
    if plan.memory_model
        && support.api_version < vk::API_VERSION_1_2
        && support.has_extension(EXT_VULKAN_MEMORY_MODEL)
    {
        push_extension(&mut plan, EXT_VULKAN_MEMORY_MODEL);
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn support(
        api_version: u32,
        dot: bool,
        coopmat: bool,
        shader_int8: bool,
        storage_8bit: bool,
        memory_model: bool,
        extensions: &[&CStr],
    ) -> DeviceSupport {
        DeviceSupport {
            api_version,
            shader_integer_dot_product: dot,
            cooperative_matrix: coopmat,
            shader_int8,
            storage_buffer8_bit_access: storage_8bit,
            vulkan_memory_model: memory_model,
            extensions: extensions
                .iter()
                .map(|extension| CString::from(*extension))
                .collect(),
        }
    }

    #[test]
    fn supported_dot_product_adds_feature() {
        let plan = build_feature_plan(&support(
            vk::API_VERSION_1_1,
            true,
            false,
            false,
            false,
            false,
            &[EXT_SHADER_INTEGER_DOT_PRODUCT],
        ));
        assert!(plan.dot_product);
        assert!(plan.extensions.contains(&EXT_SHADER_INTEGER_DOT_PRODUCT));

        // On Vulkan 1.3 the feature is core and no extension is needed.
        let plan = build_feature_plan(&support(
            vk::API_VERSION_1_3,
            true,
            false,
            false,
            false,
            false,
            &[],
        ));
        assert!(plan.dot_product);
        assert!(plan.extensions.is_empty());
    }

    #[test]
    fn unsupported_dot_product_leaves_chain_unchanged() {
        let plan = build_feature_plan(&support(
            vk::API_VERSION_1_3,
            false,
            false,
            false,
            false,
            false,
            &[],
        ));
        assert!(plan.is_empty());
    }

    #[test]
    fn coopmat_requires_all_dependencies() {
        let base = |int8, storage, model| {
            support(
                vk::API_VERSION_1_2,
                true,
                true,
                int8,
                storage,
                model,
                &[EXT_COOPERATIVE_MATRIX],
            )
        };
        assert!(build_feature_plan(&base(true, true, true)).coopmat);
        assert!(!build_feature_plan(&base(false, true, true)).coopmat);
        assert!(!build_feature_plan(&base(true, false, true)).coopmat);
        assert!(!build_feature_plan(&base(true, true, false)).coopmat);
    }

    #[test]
    fn coopmat_requires_the_extension_even_when_features_exist() {
        let plan = build_feature_plan(&support(
            vk::API_VERSION_1_3,
            true,
            true,
            true,
            true,
            true,
            &[],
        ));
        assert!(!plan.coopmat);
    }

    #[test]
    fn feature_plan_is_empty_by_default() {
        assert!(FeaturePlan::default().is_empty());
    }
}
