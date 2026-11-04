//! Vulkan upscaler pipeline. Task 3 is a pass-through; later tasks add the
//! fast/SGSR/FSR/ML variants behind the same interface.

use crate::upscaler::{UpscalerParams, UpscalerPreset};
use ash::vk;
use ash::vk::Handle;
use std::collections::HashMap;

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
}

pub struct UpscalerPipeline {
    passthrough_pipeline: vk::Pipeline,
    fast_pipeline: vk::Pipeline,
    easu_pipeline: vk::Pipeline,
    rcas_pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
    easu_render_pass: vk::RenderPass,
    set_layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    source_sets: [vk::DescriptorSet; 2],
    intermediate_sets: [vk::DescriptorSet; 2],
    intermediate_image: vk::Image,
    intermediate_memory: vk::DeviceMemory,
    intermediate_views: [vk::ImageView; 2],
    intermediate_framebuffers: [vk::Framebuffer; 2],
    sampler: vk::Sampler,
    output_views: Vec<[vk::ImageView; 2]>,
    framebuffers: Vec<[vk::Framebuffer; 2]>,
    source_views: HashMap<(u64, i32, u32), vk::ImageView>,
    width: u32,
    height: u32,
}

impl UpscalerPipeline {
    pub unsafe fn new(
        device: &ash::Device,
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        target_format: vk::Format,
        images: &[vk::Image],
        width: u32,
        height: u32,
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

            let easu_attachment = vk::AttachmentDescription {
                format: target_format,
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
                target_format,
                width,
                height,
                2,
                vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::STORAGE,
            )?;
            let mut intermediate_views = [vk::ImageView::null(); 2];
            let mut intermediate_framebuffers = [vk::Framebuffer::null(); 2];
            for eye in 0..2 {
                intermediate_views[eye] = device.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(intermediate_image)
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
                    .max_sets(4)
                    .pool_sizes(&[vk::DescriptorPoolSize {
                        ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                        descriptor_count: 4,
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
            let rcas_pipeline =
                create_graphics_pipeline(device, render_pass, layout, vertex_module, rcas_module)?;
            device.destroy_shader_module(vertex_module, None);
            device.destroy_shader_module(passthrough_module, None);
            device.destroy_shader_module(fast_module, None);
            device.destroy_shader_module(easu_module, None);
            device.destroy_shader_module(rcas_module, None);

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

            Ok(Self {
                passthrough_pipeline,
                fast_pipeline,
                easu_pipeline,
                rcas_pipeline,
                layout,
                render_pass,
                easu_render_pass,
                set_layout,
                pool,
                source_sets,
                intermediate_sets,
                intermediate_image,
                intermediate_memory,
                intermediate_views,
                intermediate_framebuffers,
                sampler,
                output_views,
                framebuffers,
                source_views: HashMap::new(),
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
            let key = (source_image.as_raw(), source_format.as_raw(), source_layer);
            let source_view = if let Some(view) = self.source_views.get(&key) {
                *view
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
                vk::PipelineStageFlags::FRAGMENT_SHADER,
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
                flags: params.flags(),
                dst_width: params.dst_width,
                dst_height: params.dst_height,
                con0: [con[0], con[1], con[2], con[3]],
                con1: [con[4], con[5], con[6], con[7]],
                con2: [con[8], con[9], con[10], con[11]],
                con3: [con[12], con[13], con[14], con[15]],
            };

            if params.preset == UpscalerPreset::Fsr {
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
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
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
                    self.layout,
                    0,
                    &[self.intermediate_sets[eye]],
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
            } else {
                let pipeline = if matches!(
                    params.preset,
                    UpscalerPreset::Fast | UpscalerPreset::Balanced
                ) {
                    self.fast_pipeline
                } else {
                    self.passthrough_pipeline
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

    pub unsafe fn destroy(&self, device: &ash::Device) {
        unsafe {
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
            for view in self.source_views.values() {
                device.destroy_image_view(*view, None);
            }
            device.destroy_sampler(self.sampler, None);
            device.destroy_descriptor_pool(self.pool, None);
            device.destroy_pipeline(self.passthrough_pipeline, None);
            device.destroy_pipeline(self.fast_pipeline, None);
            device.destroy_pipeline(self.easu_pipeline, None);
            device.destroy_pipeline(self.rcas_pipeline, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_descriptor_set_layout(self.set_layout, None);
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
        let memory_type = (0..memory_properties.memory_type_count)
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
            .expect("upscaler intermediate image needs a memory type");
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
