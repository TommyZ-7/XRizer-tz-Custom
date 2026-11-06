//! Vulkan upscaler pipeline. Task 3 is a pass-through; later tasks add the
//! fast/SGSR/FSR/ML variants behind the same interface.

use super::source_view_cache::{ExtentTracker, SourceKey, SourceViewCache};
use crate::upscaler::model::{Model, ModelKind};
use crate::upscaler::{FLAG_ML_ACTIVE, FLAG_OUTPUT_UNORM, UpscalerParams, UpscalerPreset};
use ash::vk;
use log::warn;

#[repr(C)]
struct PushConstants {
    src_bounds: [f32; 4],
    center: [f32; 2],
    radius: f32,
    feather: f32,
    strength: f32,
    flags: u32,
    dst_width: u32,
    dst_height: u32,
    con0: [u32; 4],
    con1: [u32; 4],
    con2: [u32; 4],
    con3: [u32; 4],
    ml_origin: [u32; 2],
    ml_size: [u32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BalancedRoute {
    SgsrThenRcas,
    Fast,
}

fn balanced_route(has_flipped_bounds: bool) -> BalancedRoute {
    if has_flipped_bounds {
        BalancedRoute::Fast
    } else {
        BalancedRoute::SgsrThenRcas
    }
}

pub struct UpscalerPipeline {
    passthrough_pipeline: vk::Pipeline,
    fast_pipeline: vk::Pipeline,
    easu_pipeline: vk::Pipeline,
    rcas_pipeline: vk::Pipeline,
    sgsr_pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    rcas_layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
    easu_render_pass: vk::RenderPass,
    set_layout: vk::DescriptorSetLayout,
    rcas_set_layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    source_sets: [vk::DescriptorSet; 2],
    intermediate_sets: [vk::DescriptorSet; 2],
    rcas_fsr_sets: [vk::DescriptorSet; 2],
    output_unorm: bool,
    intermediate_image: vk::Image,
    intermediate_memory: vk::DeviceMemory,
    intermediate_views: [vk::ImageView; 2],
    intermediate_framebuffers: [vk::Framebuffer; 2],
    sampler: vk::Sampler,
    output_views: Vec<[vk::ImageView; 2]>,
    framebuffers: Vec<[vk::Framebuffer; 2]>,
    source_views: SourceViewCache,
    input_extent: ExtentTracker,
    ml: Option<MlResources>,
    width: u32,
    height: u32,
}

impl UpscalerPipeline {
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn new(
        device: &ash::Device,
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        target_format: vk::Format,
        images: &[vk::Image],
        width: u32,
        height: u32,
        ml_model: Option<&Model>,
    ) -> Result<Self, vk::Result> {
        unsafe {
            let attachment = vk::AttachmentDescription {
                format: target_format,
                samples: vk::SampleCountFlags::TYPE_1,
                load_op: vk::AttachmentLoadOp::DONT_CARE,
                store_op: vk::AttachmentStoreOp::STORE,
                stencil_load_op: vk::AttachmentLoadOp::DONT_CARE,
                stencil_store_op: vk::AttachmentStoreOp::DONT_CARE,
                initial_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                final_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                ..Default::default()
            };
            let color_reference = vk::AttachmentReference {
                attachment: 0,
                layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            };
            let subpass = vk::SubpassDescription::default()
                .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                .color_attachments(std::slice::from_ref(&color_reference));
            let render_pass = device.create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(std::slice::from_ref(&attachment))
                    .subpasses(std::slice::from_ref(&subpass)),
                None,
            )?;

            let intermediate_format = vk::Format::R8G8B8A8_SRGB;
            let easu_attachment = vk::AttachmentDescription {
                format: intermediate_format,
                samples: vk::SampleCountFlags::TYPE_1,
                load_op: vk::AttachmentLoadOp::DONT_CARE,
                store_op: vk::AttachmentStoreOp::STORE,
                stencil_load_op: vk::AttachmentLoadOp::DONT_CARE,
                stencil_store_op: vk::AttachmentStoreOp::DONT_CARE,
                initial_layout: vk::ImageLayout::UNDEFINED,
                final_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                ..Default::default()
            };
            let easu_render_pass = device.create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(std::slice::from_ref(&easu_attachment))
                    .subpasses(std::slice::from_ref(&subpass)),
                None,
            )?;

            let (intermediate_image, intermediate_memory) = create_image(
                device,
                instance,
                physical_device,
                intermediate_format,
                width,
                height,
                2,
                vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
            )?;
            let mut intermediate_views = [vk::ImageView::null(); 2];
            let mut intermediate_framebuffers = [vk::Framebuffer::null(); 2];
            for eye in 0..2 {
                intermediate_views[eye] = device.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(intermediate_image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(intermediate_format)
                        .components(vk::ComponentMapping::default())
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: eye as u32,
                            layer_count: 1,
                        }),
                    None,
                )?;
                intermediate_framebuffers[eye] = device.create_framebuffer(
                    &vk::FramebufferCreateInfo::default()
                        .render_pass(easu_render_pass)
                        .attachments(std::slice::from_ref(&intermediate_views[eye]))
                        .width(width)
                        .height(height)
                        .layers(1),
                    None,
                )?;
            }

            let binding = vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT);
            let set_layout = device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default()
                    .bindings(std::slice::from_ref(&binding)),
                None,
            )?;
            let pool = device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(12)
                    .pool_sizes(&[vk::DescriptorPoolSize {
                        ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                        descriptor_count: 12,
                    }]),
                None,
            )?;
            let sets = device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(pool)
                    .set_layouts(&[set_layout, set_layout, set_layout, set_layout]),
            )?;
            let source_sets = [sets[0], sets[1]];
            let intermediate_sets = [sets[2], sets[3]];

            let push_constant_range = vk::PushConstantRange {
                stage_flags: vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                offset: 0,
                size: std::mem::size_of::<PushConstants>() as u32,
            };
            let layout = device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(std::slice::from_ref(&set_layout))
                    .push_constant_ranges(std::slice::from_ref(&push_constant_range)),
                None,
            )?;

            let rcas_bindings = [
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(1)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
            ];
            let rcas_set_layout = device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&rcas_bindings),
                None,
            )?;
            let rcas_layout = device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(std::slice::from_ref(&rcas_set_layout))
                    .push_constant_ranges(std::slice::from_ref(&push_constant_range)),
                None,
            )?;
            let rcas_sets = device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(pool)
                    .set_layouts(&[rcas_set_layout, rcas_set_layout]),
            )?;
            let rcas_fsr_sets = [rcas_sets[0], rcas_sets[1]];

            let vertex_module = create_shader_module(
                device,
                &include_bytes!(concat!(env!("OUT_DIR"), "/vert_upscaler.spv"))[..],
            )?;
            let passthrough_module = create_shader_module(
                device,
                &include_bytes!(concat!(env!("OUT_DIR"), "/frag_upscaler.spv"))[..],
            )?;
            let fast_module = create_shader_module(
                device,
                &include_bytes!(concat!(env!("OUT_DIR"), "/frag_fast.spv"))[..],
            )?;
            let easu_module = create_shader_module(
                device,
                &include_bytes!(concat!(env!("OUT_DIR"), "/frag_fsr_easu.spv"))[..],
            )?;
            let rcas_module = create_shader_module(
                device,
                &include_bytes!(concat!(env!("OUT_DIR"), "/frag_enhance.spv"))[..],
            )?;
            let sgsr_module = create_shader_module(
                device,
                &include_bytes!(concat!(env!("OUT_DIR"), "/frag_sgsr1.spv"))[..],
            )?;
            let passthrough_pipeline = create_graphics_pipeline(
                device,
                render_pass,
                layout,
                vertex_module,
                passthrough_module,
            )?;
            let fast_pipeline =
                create_graphics_pipeline(device, render_pass, layout, vertex_module, fast_module)?;
            let easu_pipeline = create_graphics_pipeline(
                device,
                easu_render_pass,
                layout,
                vertex_module,
                easu_module,
            )?;
            let rcas_pipeline = create_graphics_pipeline(
                device,
                render_pass,
                rcas_layout,
                vertex_module,
                rcas_module,
            )?;
            let sgsr_pipeline =
                create_graphics_pipeline(device, render_pass, layout, vertex_module, sgsr_module)?;
            device.destroy_shader_module(vertex_module, None);
            device.destroy_shader_module(passthrough_module, None);
            device.destroy_shader_module(fast_module, None);
            device.destroy_shader_module(easu_module, None);
            device.destroy_shader_module(rcas_module, None);
            device.destroy_shader_module(sgsr_module, None);

            let sampler = device.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .max_lod(0.0),
                None,
            )?;

            for eye in 0..2 {
                device.update_descriptor_sets(
                    &[vk::WriteDescriptorSet::default()
                        .dst_set(intermediate_sets[eye])
                        .dst_binding(0)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(&[vk::DescriptorImageInfo {
                            sampler,
                            image_view: intermediate_views[eye],
                            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        }])],
                    &[],
                );
            }

            for eye in 0..2 {
                device.update_descriptor_sets(
                    &[
                        vk::WriteDescriptorSet::default()
                            .dst_set(rcas_fsr_sets[eye])
                            .dst_binding(0)
                            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                            .image_info(&[vk::DescriptorImageInfo {
                                sampler,
                                image_view: intermediate_views[eye],
                                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                            }]),
                        vk::WriteDescriptorSet::default()
                            .dst_set(rcas_fsr_sets[eye])
                            .dst_binding(1)
                            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                            .image_info(&[vk::DescriptorImageInfo {
                                sampler,
                                image_view: intermediate_views[eye],
                                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                            }]),
                    ],
                    &[],
                );
            }

            let mut output_views = Vec::with_capacity(images.len());
            let mut framebuffers = Vec::with_capacity(images.len());
            for image in images {
                let mut views = [vk::ImageView::null(); 2];
                let mut buffers = [vk::Framebuffer::null(); 2];
                for (eye, (view, framebuffer)) in
                    views.iter_mut().zip(buffers.iter_mut()).enumerate()
                {
                    *view = device.create_image_view(
                        &vk::ImageViewCreateInfo::default()
                            .image(*image)
                            .view_type(vk::ImageViewType::TYPE_2D)
                            .format(target_format)
                            .components(vk::ComponentMapping::default())
                            .subresource_range(vk::ImageSubresourceRange {
                                aspect_mask: vk::ImageAspectFlags::COLOR,
                                base_mip_level: 0,
                                level_count: 1,
                                base_array_layer: eye as u32,
                                layer_count: 1,
                            }),
                        None,
                    )?;
                    *framebuffer = device.create_framebuffer(
                        &vk::FramebufferCreateInfo::default()
                            .render_pass(render_pass)
                            .attachments(std::slice::from_ref(view))
                            .width(width)
                            .height(height)
                            .layers(1),
                        None,
                    )?;
                }
                output_views.push(views);
                framebuffers.push(buffers);
            }

            let ml = if let Some(model) = ml_model {
                match MlResources::new(
                    device,
                    instance,
                    physical_device,
                    model,
                    sampler,
                    rcas_set_layout,
                    pool,
                    &intermediate_views,
                    width,
                    height,
                ) {
                    Some(ml) => Some(ml),
                    None => {
                        warn!("Failed to create ML upscaler resources; ML will fall back to FSR");
                        None
                    }
                }
            } else {
                None
            };

            Ok(Self {
                ml,
                passthrough_pipeline,
                fast_pipeline,
                easu_pipeline,
                rcas_pipeline,
                sgsr_pipeline,
                layout,
                rcas_layout,
                render_pass,
                easu_render_pass,
                set_layout,
                rcas_set_layout,
                pool,
                source_sets,
                intermediate_sets,
                rcas_fsr_sets,
                output_unorm: !format_is_srgb(target_format),
                intermediate_image,
                intermediate_memory,
                intermediate_views,
                intermediate_framebuffers,
                sampler,
                output_views,
                framebuffers,
                source_views: SourceViewCache::default(),
                input_extent: ExtentTracker::default(),
                width,
                height,
            })
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub unsafe fn record(
        &mut self,
        device: &ash::Device,
        command_buffer: vk::CommandBuffer,
        image_index: usize,
        eye: usize,
        source_image: vk::Image,
        source_format: vk::Format,
        source_layer: u32,
        source_extent: (u32, u32),
        params: &UpscalerParams,
    ) -> Result<(), vk::Result> {
        unsafe {
            let use_ml = params.preset == UpscalerPreset::Ml && self.ml.is_some() && params.enabled;
            let key = SourceKey::new(source_image, source_format, source_layer, source_extent);
            let source_view = if let Some(view) = self.source_views.get(key) {
                view
            } else {
                let view = device.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(source_image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(source_format)
                        .components(vk::ComponentMapping::default())
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: source_layer,
                            layer_count: 1,
                        }),
                    None,
                )?;
                self.source_views.insert(key, view);
                view
            };

            device.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default()
                    .dst_set(self.source_sets[eye])
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&[vk::DescriptorImageInfo {
                        sampler: self.sampler,
                        image_view: source_view,
                        image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    }])],
                &[],
            );

            let source_range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: source_layer,
                layer_count: 1,
            };
            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER | vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[vk::ImageMemoryBarrier {
                    src_access_mask: vk::AccessFlags::TRANSFER_READ,
                    dst_access_mask: vk::AccessFlags::SHADER_READ,
                    old_layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    new_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    image: source_image,
                    subresource_range: source_range,
                    ..Default::default()
                }],
            );

            let render_area = vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: vk::Extent2D {
                    width: self.width,
                    height: self.height,
                },
            };
            let viewport = vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: self.width as f32,
                height: self.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            };
            let con = params.fsr_easu_con(source_extent.0, source_extent.1);
            let (ml_x, ml_y, ml_w, ml_h) = params.ml_bounds(0.001);
            let mut flags = params.flags();
            if use_ml {
                flags |= FLAG_ML_ACTIVE;
            }
            if self.output_unorm {
                flags |= FLAG_OUTPUT_UNORM;
            }
            let push = PushConstants {
                src_bounds: [
                    params.src_bounds.uMin,
                    params.src_bounds.vMin,
                    params.src_bounds.uMax,
                    params.src_bounds.vMax,
                ],
                center: params.center,
                radius: params.radius,
                feather: params.feather,
                strength: params.strength,
                flags,
                dst_width: params.dst_width,
                dst_height: params.dst_height,
                con0: [con[0], con[1], con[2], con[3]],
                con1: [con[4], con[5], con[6], con[7]],
                con2: [con[8], con[9], con[10], con[11]],
                con3: [con[12], con[13], con[14], con[15]],
                ml_origin: [ml_x, ml_y],
                ml_size: [ml_w, ml_h],
            };

            if params.preset == UpscalerPreset::Fsr || params.preset == UpscalerPreset::Ml {
                device.cmd_begin_render_pass(
                    command_buffer,
                    &vk::RenderPassBeginInfo::default()
                        .render_pass(self.easu_render_pass)
                        .framebuffer(self.intermediate_framebuffers[eye])
                        .render_area(render_area),
                    vk::SubpassContents::INLINE,
                );
                device.cmd_bind_pipeline(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.easu_pipeline,
                );
                device.cmd_set_viewport(command_buffer, 0, std::slice::from_ref(&viewport));
                device.cmd_set_scissor(command_buffer, 0, std::slice::from_ref(&render_area));
                device.cmd_bind_descriptor_sets(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.layout,
                    0,
                    &[self.source_sets[eye]],
                    &[],
                );
                device.cmd_push_constants(
                    command_buffer,
                    self.layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    std::slice::from_raw_parts(
                        (&raw const push).cast::<u8>(),
                        std::mem::size_of::<PushConstants>(),
                    ),
                );
                device.cmd_draw(command_buffer, 3, 1, 0, 0);
                device.cmd_end_render_pass(command_buffer);

                device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::PipelineStageFlags::FRAGMENT_SHADER
                        | vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[vk::ImageMemoryBarrier {
                        src_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                        dst_access_mask: vk::AccessFlags::SHADER_READ,
                        old_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        new_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        image: self.intermediate_image,
                        subresource_range: vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: eye as u32,
                            layer_count: 1,
                        },
                        ..Default::default()
                    }],
                );

                let mut ml_output = None;
                if use_ml {
                    let ml = self.ml.as_mut().unwrap();
                    ml.update_source(device, eye, source_view);
                    if ml.record(device, command_buffer, eye, params) {
                        ml_output = Some(ml.output_set(eye));
                    }
                }
                let rcas_set = ml_output.unwrap_or(self.rcas_fsr_sets[eye]);

                device.cmd_begin_render_pass(
                    command_buffer,
                    &vk::RenderPassBeginInfo::default()
                        .render_pass(self.render_pass)
                        .framebuffer(self.framebuffers[image_index][eye])
                        .render_area(render_area),
                    vk::SubpassContents::INLINE,
                );
                device.cmd_bind_pipeline(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.rcas_pipeline,
                );
                device.cmd_set_viewport(command_buffer, 0, std::slice::from_ref(&viewport));
                device.cmd_set_scissor(command_buffer, 0, std::slice::from_ref(&render_area));
                device.cmd_bind_descriptor_sets(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.rcas_layout,
                    0,
                    &[rcas_set],
                    &[],
                );
                device.cmd_push_constants(
                    command_buffer,
                    self.rcas_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    std::slice::from_raw_parts(
                        (&raw const push).cast::<u8>(),
                        std::mem::size_of::<PushConstants>(),
                    ),
                );
                device.cmd_draw(command_buffer, 3, 1, 0, 0);
                device.cmd_end_render_pass(command_buffer);
            } else {
                let pipeline = match params.preset {
                    UpscalerPreset::Fast => self.fast_pipeline,
                    UpscalerPreset::Balanced => match balanced_route(params.has_flipped_bounds()) {
                        BalancedRoute::SgsrThenRcas => self.sgsr_pipeline,
                        BalancedRoute::Fast => self.fast_pipeline,
                    },
                    UpscalerPreset::Fsr | UpscalerPreset::Ml => self.passthrough_pipeline,
                };
                device.cmd_begin_render_pass(
                    command_buffer,
                    &vk::RenderPassBeginInfo::default()
                        .render_pass(self.render_pass)
                        .framebuffer(self.framebuffers[image_index][eye])
                        .render_area(render_area),
                    vk::SubpassContents::INLINE,
                );
                device.cmd_bind_pipeline(command_buffer, vk::PipelineBindPoint::GRAPHICS, pipeline);
                device.cmd_set_viewport(command_buffer, 0, std::slice::from_ref(&viewport));
                device.cmd_set_scissor(command_buffer, 0, std::slice::from_ref(&render_area));
                device.cmd_bind_descriptor_sets(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.layout,
                    0,
                    &[self.source_sets[eye]],
                    &[],
                );
                device.cmd_push_constants(
                    command_buffer,
                    self.layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    std::slice::from_raw_parts(
                        (&raw const push).cast::<u8>(),
                        std::mem::size_of::<PushConstants>(),
                    ),
                );
                device.cmd_draw(command_buffer, 3, 1, 0, 0);
                device.cmd_end_render_pass(command_buffer);
            }

            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[vk::ImageMemoryBarrier {
                    src_access_mask: vk::AccessFlags::SHADER_READ,
                    dst_access_mask: vk::AccessFlags::TRANSFER_READ,
                    old_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    new_layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    image: source_image,
                    subresource_range: source_range,
                    ..Default::default()
                }],
            );
            Ok(())
        }
    }

    /// Prepare the source-view cache before the first eye of a frame records.
    /// Clears all views when the input extent changed, or when the cache has
    /// grown to its bound. Never called between the two eyes.
    pub unsafe fn prepare_source_views(&mut self, device: &ash::Device, input_extent: (u32, u32)) {
        self.input_extent.observe(input_extent);
        if self.input_extent.take_dirty() || self.source_views.count() >= 32 {
            unsafe { self.clear_source_views(device) };
        }
    }

    unsafe fn clear_source_views(&mut self, device: &ash::Device) {
        unsafe {
            if self.source_views.count() == 0 {
                return;
            }
            let _ = device.device_wait_idle();
            for view in self.source_views.drain() {
                device.destroy_image_view(view, None);
            }
        }
    }

    pub unsafe fn destroy(&self, device: &ash::Device) {
        unsafe {
            if let Some(ml) = &self.ml {
                ml.destroy(device);
            }
            for views in &self.output_views {
                for view in views {
                    device.destroy_image_view(*view, None);
                }
            }
            for framebuffers in &self.framebuffers {
                for framebuffer in framebuffers {
                    device.destroy_framebuffer(*framebuffer, None);
                }
            }
            for view in self.source_views.iter() {
                device.destroy_image_view(view, None);
            }
            device.destroy_sampler(self.sampler, None);
            device.destroy_descriptor_pool(self.pool, None);
            device.destroy_pipeline(self.passthrough_pipeline, None);
            device.destroy_pipeline(self.fast_pipeline, None);
            device.destroy_pipeline(self.easu_pipeline, None);
            device.destroy_pipeline(self.rcas_pipeline, None);
            device.destroy_pipeline(self.sgsr_pipeline, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_pipeline_layout(self.rcas_layout, None);
            device.destroy_descriptor_set_layout(self.set_layout, None);
            device.destroy_descriptor_set_layout(self.rcas_set_layout, None);
            device.destroy_render_pass(self.render_pass, None);
            device.destroy_render_pass(self.easu_render_pass, None);
            for view in &self.intermediate_views {
                device.destroy_image_view(*view, None);
            }
            for framebuffer in &self.intermediate_framebuffers {
                device.destroy_framebuffer(*framebuffer, None);
            }
            device.destroy_image(self.intermediate_image, None);
            device.free_memory(self.intermediate_memory, None);
        }
    }
}

#[repr(C)]
struct MlPushConstants {
    src_bounds: [f32; 4],
    center: [f32; 2],
    radius: f32,
    feather: f32,
    strength: f32,
    flags: u32,
    dst_width: u32,
    dst_height: u32,
    ml_origin: [u32; 2],
    ml_size: [u32; 2],
    hidden: u32,
    input_scale: f32,
    activation_limit: f32,
    output_scale: f32,
    residual_clamp: f32,
    scale: f32,
}

struct MlResources {
    image: vk::Image,
    memory: vk::DeviceMemory,
    views: [vk::ImageView; 2],
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    set_layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    sets: [vk::DescriptorSet; 2],
    output_sets: [vk::DescriptorSet; 2],
    weights_buffer: vk::Buffer,
    weights_memory: vk::DeviceMemory,
    sampler: vk::Sampler,
    weight_count: u32,
    kind: ModelKind,
    hidden: u32,
    input_scale: f32,
    output_scale: f32,
    residual_clamp: f32,
    scale: f32,
    initialized: [bool; 2],
}

impl MlResources {
    #[allow(clippy::too_many_arguments)]
    unsafe fn new(
        device: &ash::Device,
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        model: &Model,
        sampler: vk::Sampler,
        output_set_layout: vk::DescriptorSetLayout,
        output_pool: vk::DescriptorPool,
        base_views: &[vk::ImageView; 2],
        width: u32,
        height: u32,
    ) -> Option<Self> {
        unsafe {
            let (packed, shader_bytes, hidden, output_scale) = match model.kind {
                ModelKind::Mlp => (
                    model.packed_mlp_weights()?,
                    &include_bytes!(concat!(env!("OUT_DIR"), "/comp_mlp_dp4a.spv"))[..],
                    model.layers.first()?.output_count,
                    model.mlp_output_scale(),
                ),
                ModelKind::ResidualCnn => (
                    model.packed_cnn_weights()?,
                    &include_bytes!(concat!(env!("OUT_DIR"), "/comp_residual_cnn.spv"))[..],
                    model.channels as u32,
                    model.cnn_output_scale(),
                ),
                ModelKind::Offset => (
                    model.packed_srpo_weights()?,
                    &include_bytes!(concat!(env!("OUT_DIR"), "/comp_srpo.spv"))[..],
                    model.channels as u32,
                    model.srpo_output_scale(),
                ),
            };
            let (image, memory) = create_image(
                device,
                instance,
                physical_device,
                vk::Format::R8G8B8A8_UNORM,
                width,
                height,
                2,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED,
            )
            .ok()?;
            let mut views = [vk::ImageView::null(); 2];
            for (eye, view) in views.iter_mut().enumerate() {
                *view = device
                    .create_image_view(
                        &vk::ImageViewCreateInfo::default()
                            .image(image)
                            .view_type(vk::ImageViewType::TYPE_2D)
                            .format(vk::Format::R8G8B8A8_UNORM)
                            .components(vk::ComponentMapping::default())
                            .subresource_range(vk::ImageSubresourceRange {
                                aspect_mask: vk::ImageAspectFlags::COLOR,
                                base_mip_level: 0,
                                level_count: 1,
                                base_array_layer: eye as u32,
                                layer_count: 1,
                            }),
                        None,
                    )
                    .ok()?;
            }

            let weights_size = (packed.len() * 4) as u64;
            let (weights_buffer, weights_memory) = create_buffer(
                device,
                instance,
                physical_device,
                weights_size,
                vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .ok()?;
            let pointer = device
                .map_memory(weights_memory, 0, weights_size, vk::MemoryMapFlags::empty())
                .ok()?;
            std::ptr::copy_nonoverlapping(
                packed.as_ptr() as *const u8,
                pointer.cast::<u8>(),
                packed.len() * 4,
            );
            device.unmap_memory(weights_memory);

            let bindings = [
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(1)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(2)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(3)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
            ];
            let set_layout = device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                    None,
                )
                .ok()?;
            let pool = device
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .max_sets(2)
                        .pool_sizes(&[
                            vk::DescriptorPoolSize {
                                ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                                descriptor_count: 4,
                            },
                            vk::DescriptorPoolSize {
                                ty: vk::DescriptorType::STORAGE_IMAGE,
                                descriptor_count: 2,
                            },
                            vk::DescriptorPoolSize {
                                ty: vk::DescriptorType::STORAGE_BUFFER,
                                descriptor_count: 2,
                            },
                        ]),
                    None,
                )
                .ok()?;
            let sets = device
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(pool)
                        .set_layouts(&[set_layout, set_layout]),
                )
                .ok()?;
            let sets = [sets[0], sets[1]];

            let push_constant_range = vk::PushConstantRange {
                stage_flags: vk::ShaderStageFlags::COMPUTE,
                offset: 0,
                size: std::mem::size_of::<MlPushConstants>() as u32,
            };
            let layout = device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default()
                        .set_layouts(std::slice::from_ref(&set_layout))
                        .push_constant_ranges(std::slice::from_ref(&push_constant_range)),
                    None,
                )
                .ok()?;
            let pipeline = create_compute_pipeline(device, layout, shader_bytes).ok()?;

            for eye in 0..2 {
                device.update_descriptor_sets(
                    &[
                        vk::WriteDescriptorSet::default()
                            .dst_set(sets[eye])
                            .dst_binding(1)
                            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                            .image_info(&[vk::DescriptorImageInfo {
                                sampler,
                                image_view: base_views[eye],
                                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                            }]),
                        vk::WriteDescriptorSet::default()
                            .dst_set(sets[eye])
                            .dst_binding(2)
                            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                            .image_info(&[vk::DescriptorImageInfo {
                                sampler: vk::Sampler::null(),
                                image_view: views[eye],
                                image_layout: vk::ImageLayout::GENERAL,
                            }]),
                        vk::WriteDescriptorSet::default()
                            .dst_set(sets[eye])
                            .dst_binding(3)
                            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                            .buffer_info(&[vk::DescriptorBufferInfo {
                                buffer: weights_buffer,
                                offset: 0,
                                range: weights_size,
                            }]),
                    ],
                    &[],
                );
            }

            let output_sets = device
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(output_pool)
                        .set_layouts(&[output_set_layout, output_set_layout]),
                )
                .ok()?;
            let output_sets = [output_sets[0], output_sets[1]];
            for eye in 0..2 {
                device.update_descriptor_sets(
                    &[
                        vk::WriteDescriptorSet::default()
                            .dst_set(output_sets[eye])
                            .dst_binding(0)
                            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                            .image_info(&[vk::DescriptorImageInfo {
                                sampler,
                                image_view: views[eye],
                                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                            }]),
                        vk::WriteDescriptorSet::default()
                            .dst_set(output_sets[eye])
                            .dst_binding(1)
                            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                            .image_info(&[vk::DescriptorImageInfo {
                                sampler,
                                image_view: base_views[eye],
                                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                            }]),
                    ],
                    &[],
                );
            }

            Some(Self {
                image,
                memory,
                views,
                pipeline,
                layout,
                set_layout,
                pool,
                sets,
                output_sets,
                weights_buffer,
                weights_memory,
                sampler,
                weight_count: packed.len() as u32,
                kind: model.kind,
                hidden,
                input_scale: model.input_scale,
                output_scale,
                residual_clamp: model.residual_clamp,
                scale: model.scale,
                initialized: [false; 2],
            })
        }
    }

    unsafe fn update_source(&self, device: &ash::Device, eye: usize, view: vk::ImageView) {
        unsafe {
            device.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default()
                    .dst_set(self.sets[eye])
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&[vk::DescriptorImageInfo {
                        sampler: self.sampler,
                        image_view: view,
                        image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    }])],
                &[],
            );
        }
    }

    unsafe fn record(
        &mut self,
        device: &ash::Device,
        command_buffer: vk::CommandBuffer,
        eye: usize,
        params: &UpscalerParams,
    ) -> bool {
        unsafe {
            let (origin_x, origin_y, width, height) = params.ml_bounds(0.001);
            if width == 0 || height == 0 || self.weight_count == 0 {
                return false;
            }
            let range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: eye as u32,
                layer_count: 1,
            };
            let old_layout = if self.initialized[eye] {
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
            } else {
                vk::ImageLayout::UNDEFINED
            };
            let (src_stage, src_access) = if self.initialized[eye] {
                (
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::AccessFlags::SHADER_READ,
                )
            } else {
                (
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::AccessFlags::empty(),
                )
            };
            device.cmd_pipeline_barrier(
                command_buffer,
                src_stage,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[vk::ImageMemoryBarrier {
                    src_access_mask: src_access,
                    dst_access_mask: vk::AccessFlags::SHADER_WRITE,
                    old_layout,
                    new_layout: vk::ImageLayout::GENERAL,
                    image: self.image,
                    subresource_range: range,
                    ..Default::default()
                }],
            );

            let push = MlPushConstants {
                src_bounds: [
                    params.src_bounds.uMin,
                    params.src_bounds.vMin,
                    params.src_bounds.uMax,
                    params.src_bounds.vMax,
                ],
                center: params.center,
                radius: params.radius,
                feather: params.feather,
                strength: params.strength,
                flags: params.flags(),
                dst_width: params.dst_width,
                dst_height: params.dst_height,
                ml_origin: [origin_x, origin_y],
                ml_size: [width, height],
                hidden: self.hidden,
                input_scale: self.input_scale,
                activation_limit: 127.0,
                output_scale: self.output_scale,
                residual_clamp: self.residual_clamp,
                scale: self.scale,
            };
            device.cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline,
            );
            device.cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.layout,
                0,
                &[self.sets[eye]],
                &[],
            );
            device.cmd_push_constants(
                command_buffer,
                self.layout,
                vk::ShaderStageFlags::COMPUTE,
                0,
                std::slice::from_raw_parts(
                    (&raw const push).cast::<u8>(),
                    std::mem::size_of::<MlPushConstants>(),
                ),
            );
            device.cmd_dispatch(command_buffer, width.div_ceil(8), height.div_ceil(8), 1);
            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[vk::ImageMemoryBarrier {
                    src_access_mask: vk::AccessFlags::SHADER_WRITE,
                    dst_access_mask: vk::AccessFlags::SHADER_READ,
                    old_layout: vk::ImageLayout::GENERAL,
                    new_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    image: self.image,
                    subresource_range: range,
                    ..Default::default()
                }],
            );
            self.initialized[eye] = true;
            crate::upscaler::diagnostics::record_ml_model(self.kind);
            true
        }
    }

    fn output_set(&self, eye: usize) -> vk::DescriptorSet {
        self.output_sets[eye]
    }

    unsafe fn destroy(&self, device: &ash::Device) {
        unsafe {
            for view in &self.views {
                device.destroy_image_view(*view, None);
            }
            device.destroy_image(self.image, None);
            device.free_memory(self.memory, None);
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_descriptor_set_layout(self.set_layout, None);
            device.destroy_descriptor_pool(self.pool, None);
            device.destroy_buffer(self.weights_buffer, None);
            device.free_memory(self.weights_memory, None);
        }
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_buffer(
    device: &ash::Device,
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    size: u64,
    usage: vk::BufferUsageFlags,
) -> Result<(vk::Buffer, vk::DeviceMemory), vk::Result> {
    unsafe {
        let buffer = device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(size)
                .usage(usage)
                .sharing_mode(vk::SharingMode::EXCLUSIVE),
            None,
        )?;
        let requirements = device.get_buffer_memory_requirements(buffer);
        let properties = instance.get_physical_device_memory_properties(physical_device);
        let Some(memory_type) = (0..properties.memory_type_count).find(|index| {
            requirements.memory_type_bits & (1 << index) != 0
                && properties.memory_types[*index as usize]
                    .property_flags
                    .contains(
                        vk::MemoryPropertyFlags::HOST_VISIBLE
                            | vk::MemoryPropertyFlags::HOST_COHERENT,
                    )
        }) else {
            device.destroy_buffer(buffer, None);
            return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
        };
        let memory = device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type),
            None,
        )?;
        device.bind_buffer_memory(buffer, memory, 0)?;
        Ok((buffer, memory))
    }
}

pub(super) unsafe fn probe_dp4a(device: &ash::Device) -> bool {
    unsafe {
        let binding = vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE);
        let Ok(set_layout) = device.create_descriptor_set_layout(
            &vk::DescriptorSetLayoutCreateInfo::default().bindings(std::slice::from_ref(&binding)),
            None,
        ) else {
            return false;
        };
        let Ok(layout) = device.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default().set_layouts(std::slice::from_ref(&set_layout)),
            None,
        ) else {
            device.destroy_descriptor_set_layout(set_layout, None);
            return false;
        };
        let probe = create_compute_pipeline(
            device,
            layout,
            &include_bytes!(concat!(env!("OUT_DIR"), "/comp_dp4a_probe.spv"))[..],
        );
        let supported = if let Ok(pipeline) = probe {
            device.destroy_pipeline(pipeline, None);
            true
        } else {
            false
        };
        device.destroy_pipeline_layout(layout, None);
        device.destroy_descriptor_set_layout(set_layout, None);
        supported
    }
}

unsafe fn create_compute_pipeline(
    device: &ash::Device,
    layout: vk::PipelineLayout,
    bytes: &[u8],
) -> Result<vk::Pipeline, vk::Result> {
    unsafe {
        let module = create_shader_module(device, bytes)?;
        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(module)
            .name(c"main");
        let info = vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(layout);
        let result = device.create_compute_pipelines(
            vk::PipelineCache::null(),
            std::slice::from_ref(&info),
            None,
        );
        device.destroy_shader_module(module, None);
        result
            .map_err(|(_, error)| error)
            .map(|pipelines| pipelines[0])
    }
}

unsafe fn create_shader_module(
    device: &ash::Device,
    bytes: &[u8],
) -> Result<vk::ShaderModule, vk::Result> {
    unsafe {
        let words =
            ash::util::read_spv(&mut std::io::Cursor::new(bytes)).expect("valid upscaler shader");
        device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
    }
}

unsafe fn create_graphics_pipeline(
    device: &ash::Device,
    render_pass: vk::RenderPass,
    layout: vk::PipelineLayout,
    vertex_module: vk::ShaderModule,
    fragment_module: vk::ShaderModule,
) -> Result<vk::Pipeline, vk::Result> {
    unsafe {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vertex_module)
                .name(c"main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fragment_module)
                .name(c"main"),
        ];
        let input_state = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly_state = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster_state = vk::PipelineRasterizationStateCreateInfo::default()
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let multisample_state = vk::PipelineMultisampleStateCreateInfo::default();
        let blend = vk::PipelineColorBlendAttachmentState {
            blend_enable: vk::FALSE,
            color_write_mask: vk::ColorComponentFlags::RGBA,
            ..Default::default()
        };
        let blend_state = vk::PipelineColorBlendStateCreateInfo::default()
            .attachments(std::slice::from_ref(&blend));
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic_state =
            vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&input_state)
            .input_assembly_state(&assembly_state)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster_state)
            .multisample_state(&multisample_state)
            .color_blend_state(&blend_state)
            .dynamic_state(&dynamic_state)
            .render_pass(render_pass)
            .subpass(0)
            .layout(layout);
        device
            .create_graphics_pipelines(
                vk::PipelineCache::null(),
                std::slice::from_ref(&pipeline_info),
                None,
            )
            .map_err(|(_, error)| error)
            .map(|pipelines| pipelines[0])
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_image(
    device: &ash::Device,
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    format: vk::Format,
    width: u32,
    height: u32,
    layers: u32,
    usage: vk::ImageUsageFlags,
) -> Result<(vk::Image, vk::DeviceMemory), vk::Result> {
    unsafe {
        let image = device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(layers)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(usage)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        )?;
        let requirements = device.get_image_memory_requirements(image);
        let memory_properties = instance.get_physical_device_memory_properties(physical_device);
        let Some(memory_type) = (0..memory_properties.memory_type_count)
            .find(|index| {
                requirements.memory_type_bits & (1 << index) != 0
                    && memory_properties.memory_types[*index as usize]
                        .property_flags
                        .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            })
            .or_else(|| {
                (0..memory_properties.memory_type_count)
                    .find(|index| requirements.memory_type_bits & (1 << index) != 0)
            })
        else {
            device.destroy_image(image, None);
            return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
        };
        let memory = device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type),
            None,
        )?;
        device.bind_image_memory(image, memory, 0)?;
        Ok((image, memory))
    }
}

fn format_is_srgb(format: vk::Format) -> bool {
    matches!(
        format,
        vk::Format::R8G8B8A8_SRGB
            | vk::Format::B8G8R8A8_SRGB
            | vk::Format::BC1_RGB_SRGB_BLOCK
            | vk::Format::BC1_RGBA_SRGB_BLOCK
            | vk::Format::BC2_SRGB_BLOCK
            | vk::Format::BC3_SRGB_BLOCK
            | vk::Format::BC7_SRGB_BLOCK
            | vk::Format::ETC2_R8G8B8_SRGB_BLOCK
            | vk::Format::ETC2_R8G8B8A1_SRGB_BLOCK
            | vk::Format::ETC2_R8G8B8A8_SRGB_BLOCK
            | vk::Format::ASTC_4X4_SRGB_BLOCK
            | vk::Format::ASTC_5X4_SRGB_BLOCK
            | vk::Format::ASTC_5X5_SRGB_BLOCK
            | vk::Format::ASTC_6X5_SRGB_BLOCK
            | vk::Format::ASTC_6X6_SRGB_BLOCK
            | vk::Format::ASTC_8X5_SRGB_BLOCK
            | vk::Format::ASTC_8X6_SRGB_BLOCK
            | vk::Format::ASTC_8X8_SRGB_BLOCK
            | vk::Format::ASTC_10X5_SRGB_BLOCK
            | vk::Format::ASTC_10X6_SRGB_BLOCK
            | vk::Format::ASTC_10X8_SRGB_BLOCK
            | vk::Format::ASTC_10X10_SRGB_BLOCK
            | vk::Format::ASTC_12X10_SRGB_BLOCK
            | vk::Format::ASTC_12X12_SRGB_BLOCK
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_format_detection() {
        assert!(format_is_srgb(vk::Format::R8G8B8A8_SRGB));
        assert!(format_is_srgb(vk::Format::B8G8R8A8_SRGB));
        assert!(!format_is_srgb(vk::Format::R8G8B8A8_UNORM));
        assert!(!format_is_srgb(vk::Format::B8G8R8A8_UNORM));
    }

    #[test]
    fn balanced_route_uses_sgsr_then_rcas_unless_flipped() {
        assert_eq!(balanced_route(false), BalancedRoute::SgsrThenRcas);
        assert_eq!(balanced_route(true), BalancedRoute::Fast);
    }
}
