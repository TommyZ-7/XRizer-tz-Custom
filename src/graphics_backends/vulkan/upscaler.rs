//! Equal-size post-process pass. B1b adds the FXAA shader behind this
//! interface; B1a only removes the scaling/ML paths and keeps a valid
//! pass-through image.

use super::source_view_cache::{ExtentTracker, SourceKey, SourceViewCache};
use crate::upscaler::UpscalerParams;
use ash::vk;

#[repr(C)]
#[derive(Clone, Copy)]
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

pub struct UpscalerPipeline {
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
    set_layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    source_sets: [vk::DescriptorSet; 2],
    sampler: vk::Sampler,
    output_views: Vec<[vk::ImageView; 2]>,
    framebuffers: Vec<[vk::Framebuffer; 2]>,
    source_views: SourceViewCache,
    input_extent: ExtentTracker,
    width: u32,
    height: u32,
}

impl UpscalerPipeline {
    pub unsafe fn new(
        device: &ash::Device,
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
                    .max_sets(2)
                    .pool_sizes(&[vk::DescriptorPoolSize {
                        ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                        descriptor_count: 2,
                    }]),
                None,
            )?;
            let sets = device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(pool)
                    .set_layouts(&[set_layout, set_layout]),
            )?;
            let source_sets = [sets[0], sets[1]];

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
            let fragment_module = create_shader_module(
                device,
                &include_bytes!(concat!(env!("OUT_DIR"), "/frag_upscaler.spv"))[..],
            )?;
            let pipeline = create_graphics_pipeline(
                device,
                render_pass,
                layout,
                vertex_module,
                fragment_module,
            )?;
            device.destroy_shader_module(vertex_module, None);
            device.destroy_shader_module(fragment_module, None);

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
                pipeline,
                layout,
                render_pass,
                set_layout,
                pool,
                source_sets,
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
            let push = PushConstants {
                src_bounds: [
                    params.src_bounds.uMin,
                    params.src_bounds.vMin,
                    params.src_bounds.uMax,
                    params.src_bounds.vMax,
                ],
                center: params.center_for_eye(eye),
                radius: params.radius,
                feather: params.feather,
                strength: params.strength,
                flags: params.flags(),
                dst_width: params.dst_width,
                dst_height: params.dst_height,
                con0: [0; 4],
                con1: [0; 4],
                con2: [0; 4],
                con3: [0; 4],
                ml_origin: [0; 2],
                ml_size: [0; 2],
            };

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
                self.pipeline,
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
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_descriptor_set_layout(self.set_layout, None);
            device.destroy_render_pass(self.render_pass, None);
        }
    }
}

unsafe fn create_shader_module(
    device: &ash::Device,
    bytes: &[u8],
) -> Result<vk::ShaderModule, vk::Result> {
    unsafe {
        let words = ash::util::read_spv(&mut std::io::Cursor::new(bytes)).expect("valid AA shader");
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
