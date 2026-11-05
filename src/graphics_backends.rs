mod gl;
mod vulkan;

use crate::upscaler::UpscalerParams;
use derive_more::{From, TryInto};
pub use gl::GlData;
use openvr as vr;
use openxr as xr;
pub use vulkan::VulkanData;

pub trait GraphicsBackend: Into<SupportedBackend> {
    type Api: xr::Graphics + 'static;
    type OpenVrTexture: Copy;
    type NiceFormat: std::fmt::Debug;

    fn to_nice_format(format: <Self::Api as xr::Graphics>::Format) -> Self::NiceFormat;

    fn session_create_info(&self) -> <Self::Api as xr::Graphics>::SessionCreateInfo;

    /// Returns None if the texture is invalid.
    fn get_texture(texture: &vr::Texture_t) -> Option<Self::OpenVrTexture>;

    /// Full extent of the game-owned source texture, before bounds are applied.
    fn source_texture_extent(_texture: Self::OpenVrTexture) -> Option<(u32, u32)> {
        None
    }

    fn swapchain_info_for_texture(
        &self,
        texture: Self::OpenVrTexture,
        bounds: vr::VRTextureBounds_t,
        color_space: vr::EColorSpace,
    ) -> xr::SwapchainCreateInfo<Self::Api>;

    fn store_swapchain_images(
        &mut self,
        images: Vec<<Self::Api as xr::Graphics>::SwapchainImage>,
        format: <Self::Api as xr::Graphics>::Format,
        width: u32,
        height: u32,
    );

    /// Whether this backend can run the experimental upscaler. Keep this false
    /// by default so backends opt in explicitly.
    fn supports_upscaler(&self) -> bool {
        false
    }

    /// Whether this backend can run the ML preset with the current device.
    fn supports_ml_upscaler(&self) -> bool {
        false
    }

    /// Prepare cached views of application-owned source images for a frame.
    /// Called on the first eye before either eye records a copy; the backend
    /// clears its cache when `input_extent` changed or the cache is full.
    fn prepare_upscaler_source_views(&mut self, _input_extent: (u32, u32)) {}

    #[allow(clippy::too_many_arguments)]
    fn copy_texture_to_swapchain(
        &mut self,
        eye: vr::EVREye,
        texture: Self::OpenVrTexture,
        color_space: vr::EColorSpace,
        bounds: vr::VRTextureBounds_t,
        image_index: usize,
        submit_flags: vr::EVRSubmitFlags,
        params: Option<&UpscalerParams>,
    ) -> xr::Extent2Di;

    /// Submit eye copies staged by `copy_texture_to_swapchain` for `image_index`
    /// in a single `queue_submit`. Called once both eyes are in. Default is a
    /// no-op for backends without a queue (GL records nothing to submit).
    fn flush_copies(&mut self, _image_index: usize) {}

    fn copy_overlay_to_swapchain(
        &mut self,
        texture: Self::OpenVrTexture,
        bounds: vr::VRTextureBounds_t,
        image_index: usize,
    ) -> xr::Extent2Di;

    /// Release cached overlay resources. Called when the application clears an
    /// overlay texture, so the application is free to destroy it afterwards.
    fn clear_overlay_cache(&mut self) {}
}

#[derive(macros::Backends, TryInto, From)]
#[try_into(owned, ref)]
#[allow(clippy::large_enum_variant)]
pub enum SupportedBackend {
    Vulkan(VulkanData),
    OpenGL(GlData),
    #[cfg(test)]
    Fake(crate::compositor::FakeGraphicsData),
}

impl<B> GraphicsEnum<B> for SupportedBackend
where
    B: GraphicsBackend + TryFrom<Self>,
{
    type Inner = B;
}

#[allow(clippy::single_component_path_imports)]
pub(crate) use supported_apis_enum;
#[allow(clippy::single_component_path_imports)]
pub(crate) use supported_backends_enum;

// These traits are used to commit some type crimes, see macros::any_graphics
// None of them should be manually implemented
pub trait GraphicsEnum<G>: Sized {
    type Inner: TryFrom<Self>;
}

pub trait WithAnyGraphicsParams {
    type Args;
    type Ret;
}

pub trait WithAnyGraphics<G>: WithAnyGraphicsParams {
    type GraphicsEnum: GraphicsEnum<G>;
    fn with_any_graphics(
        inner: &<Self::GraphicsEnum as GraphicsEnum<G>>::Inner,
        args: Self::Args,
    ) -> Self::Ret;
}

pub trait WithAnyGraphicsMut<G>: WithAnyGraphicsParams {
    type GraphicsEnum: GraphicsEnum<G>;
    fn with_any_graphics(
        inner: &mut <Self::GraphicsEnum as GraphicsEnum<G>>::Inner,
        args: Self::Args,
    ) -> Self::Ret;
}

pub trait WithAnyGraphicsOwned<G>: WithAnyGraphicsParams {
    type GraphicsEnum: GraphicsEnum<G>;
    fn with_any_graphics(
        inner: <Self::GraphicsEnum as GraphicsEnum<G>>::Inner,
        args: Self::Args,
    ) -> Self::Ret;
}

impl SupportedBackend {
    pub fn is_texture_type_supported(texture_type: vr::ETextureType) -> bool {
        match texture_type {
            vr::ETextureType::Vulkan | vr::ETextureType::OpenGL => true,
            #[cfg(test)]
            vr::ETextureType::Reserved => true,
            _ => false,
        }
    }

    pub fn new(texture: &vr::Texture_t, _bounds: vr::VRTextureBounds_t) -> Option<Self> {
        match texture.eType {
            vr::ETextureType::Vulkan => {
                let vk_texture = unsafe { &*(texture.handle as *const vr::VRVulkanTextureData_t) };
                Some(Self::Vulkan(VulkanData::new(vk_texture)))
            }
            vr::ETextureType::OpenGL => GlData::new().map(Self::OpenGL),
            #[cfg(test)]
            vr::ETextureType::Reserved => Some(Self::Fake(
                crate::compositor::FakeGraphicsData::new(texture),
            )),
            other => panic!("Unsupported texture type: {other:?}"),
        }
    }
}
