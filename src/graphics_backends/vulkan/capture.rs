//! Opt-in training-capture readback.
//!
//! Disabled by default. When `XRIZER_CAPTURE_DIR` (or `XRIZER_CAPTURE=1`) is
//! set, a GPU pass copies each submitted eye texture into an RGBA8 image,
//! copies that image into a host-visible buffer, and a background thread writes
//! lossless PNGs plus metadata. The submit path only records a separate command
//! buffer and never waits for the PNG encoder.

use super::source_view_cache::{ExtentTracker, SourceKey, SourceViewCache};
use crate::capture::{CaptureMode, CaptureSettings};
use ash::vk;
use log::{info, warn};
use openvr as vr;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy)]
pub struct CaptureSource {
    pub image: vk::Image,
    pub format: vk::Format,
    pub layer: u32,
    pub width: u32,
    pub height: u32,
    pub bounds: [f32; 4],
    pub color_space: &'static str,
    pub sample_count: u32,
}

#[derive(Serialize, Deserialize, Clone)]
struct CaptureMetadata {
    capture_version: u32,
    game: String,
    scene: String,
    #[serde(default = "default_aa_mode")]
    aa_mode: String,
    #[serde(default = "default_color_space")]
    color_space: String,
    #[serde(default = "default_eye")]
    eye: String,
    session_started_unix_ms: u64,
    render_scale: f32,
    output_scale: f32,
    preset: String,
    #[serde(default = "default_mode")]
    mode: String,
    #[serde(default)]
    frames: Vec<FrameMeta>,
}

fn default_aa_mode() -> String {
    "unknown".to_string()
}

fn default_color_space() -> String {
    "unknown".to_string()
}

fn default_eye() -> String {
    "both".to_string()
}

fn default_mode() -> String {
    "interval".to_string()
}

#[derive(Serialize, Deserialize, Clone)]
struct FrameMeta {
    index: u32,
    timestamp_unix_ms: u64,
    frame_time_ms: f32,
    render_scale: f32,
    left: EyeMeta,
    right: EyeMeta,
}

#[derive(Serialize, Deserialize, Clone)]
struct EyeMeta {
    file: String,
    width: u32,
    height: u32,
    format: String,
    color_space: String,
    array_layer: u32,
    bounds: [f32; 4],
}

struct CaptureJob {
    notify: bool,
    left: Vec<u8>,
    right: Vec<u8>,
    left_path: PathBuf,
    right_path: PathBuf,
    width: u32,
    height: u32,
    frame: FrameMeta,
}

struct CaptureWriter {
    tx: Option<Sender<CaptureJob>>,
    join: Option<JoinHandle<()>>,
}

impl CaptureWriter {
    fn new(dir: PathBuf, metadata: Arc<Mutex<CaptureMetadata>>) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("could not create capture dir {}: {error}", dir.display()))?;
        write_metadata(&dir, &metadata)?;
        let (tx, rx) = mpsc::channel::<CaptureJob>();
        let worker_dir = dir.clone();
        let worker_metadata = metadata.clone();
        let join = std::thread::Builder::new()
            .name("xrizer-capture-writer".to_string())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    if let Err(error) = write_png(&job.left_path, job.width, job.height, &job.left)
                    {
                        warn!("capture PNG write failed: {error}");
                        continue;
                    }
                    if let Err(error) =
                        write_png(&job.right_path, job.width, job.height, &job.right)
                    {
                        warn!("capture PNG write failed: {error}");
                        continue;
                    }
                    let index = job.frame.index;
                    let notify = job.notify;
                    {
                        let mut metadata = worker_metadata
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        metadata.frames.push(job.frame);
                    }
                    if let Err(error) = write_metadata(&worker_dir, &worker_metadata) {
                        warn!("capture metadata write failed: {error}");
                    } else if notify {
                        crate::capture::notify_capture(index, &worker_dir);
                    }
                }
            })
            .map_err(|error| format!("could not start capture writer: {error}"))?;
        Ok(Self {
            tx: Some(tx),
            join: Some(join),
        })
    }

    fn send(&self, job: CaptureJob) {
        if let Some(tx) = &self.tx
            && let Err(error) = tx.send(job)
        {
            warn!("capture job dropped: {error}");
        }
    }
}

impl Drop for CaptureWriter {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let file = File::create(path)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| format!("PNG header failed: {error}"))?;
    writer
        .write_image_data(rgba)
        .map_err(|error| format!("PNG data failed: {error}"))
}

fn write_metadata(dir: &Path, metadata: &Arc<Mutex<CaptureMetadata>>) -> Result<(), String> {
    let metadata = metadata
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let bytes = serde_json::to_vec_pretty(&*metadata)
        .map_err(|error| format!("metadata serialization failed: {error}"))?;
    let temporary = dir.join("metadata.json.tmp");
    let final_path = dir.join("metadata.json");
    std::fs::write(&temporary, bytes)
        .map_err(|error| format!("metadata temporary write failed: {error}"))?;
    std::fs::rename(&temporary, &final_path)
        .map_err(|error| format!("metadata rename failed: {error}"))
}

struct CaptureFlight {
    fence: vk::Fence,
    buffers: [vk::Buffer; 2],
    memories: [vk::DeviceMemory; 2],
    sizes: [u64; 2],
    sources: [CaptureSource; 2],
    frame_time_ms: f32,
    timestamp_unix_ms: u64,
}

pub struct CaptureState {
    config: CaptureSettings,
    writer: CaptureWriter,
    pipeline: Option<CapturePipeline>,
    pipeline_key: Option<(vk::Format, u32, u32)>,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    next_due: Instant,
    last_capture: Instant,
    next_index: u32,
    saved_pairs: u32,
    pending: bool,
    sources: [Option<CaptureSource>; 2],
    flight: Option<CaptureFlight>,
    source_extent: ExtentTracker,
    stopped: bool,
}

impl CaptureState {
    pub fn new(
        config: CaptureSettings,
        device: &ash::Device,
        queue_family_index: u32,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(&config.dir).map_err(|error| {
            format!(
                "could not create capture dir {}: {error}",
                config.dir.display()
            )
        })?;
        let metadata_path = config.dir.join("metadata.json");
        let existing = std::fs::read(&metadata_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CaptureMetadata>(&bytes).ok());
        let next_index = existing
            .as_ref()
            .and_then(|metadata| metadata.frames.iter().map(|frame| frame.index).max())
            .map(|index| index.saturating_add(1))
            .unwrap_or_else(|| scan_next_index(&config.dir));
        let metadata = existing.unwrap_or_else(|| CaptureMetadata {
            capture_version: 1,
            game: game_name(),
            scene: config.scene.clone(),
            aa_mode: "unknown".to_string(),
            color_space: "unknown".to_string(),
            eye: "both".to_string(),
            session_started_unix_ms: now_unix_ms(),
            render_scale: crate::settings::current_supersample_scale(),
            output_scale: 1.0,
            preset: format!("{:?}", crate::upscaler::snapshot().antialias).to_lowercase(),
            mode: format!("{:?}", config.mode).to_lowercase(),
            frames: Vec::new(),
        });
        let metadata = Arc::new(Mutex::new(metadata));
        let writer = CaptureWriter::new(config.dir.clone(), metadata.clone())?;
        let command_pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(queue_family_index),
                None,
            )
        }
        .map_err(|error| format!("capture command pool failed: {error:?}"))?;
        let command_buffer = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|error| format!("capture command buffer failed: {error:?}"))?[0];
        crate::capture::clear_shutter_request();
        info!(
            "training capture enabled: dir={} mode={:?} interval_ms={} max_pairs={} next_index={next_index}",
            config.dir.display(),
            config.mode,
            config.interval.as_millis(),
            config.max_pairs
        );
        Ok(Self {
            next_due: Instant::now(),
            last_capture: Instant::now(),
            config,
            writer,
            pipeline: None,
            pipeline_key: None,
            command_pool,
            command_buffer,
            next_index,
            saved_pairs: 0,
            pending: false,
            sources: [None; 2],
            flight: None,
            source_extent: ExtentTracker::default(),
            stopped: false,
        })
    }

    pub fn note_eye(&mut self, eye: usize, source: CaptureSource, now: Instant) {
        if self.stopped {
            crate::capture::clear_shutter_request();
            return;
        }
        // Track the left eye's texture extent even while a previous flight is
        // pending: the game may resize (and recreate the image) between two
        // captures. Captures require equal eye sizes, so the left eye is
        // sufficient and avoids invalidating twice for per-eye differences.
        if eye == 0 && source.width > 0 && source.height > 0 {
            self.source_extent.observe((source.width, source.height));
        }
        if self.flight.is_some() {
            return;
        }
        if source.sample_count != 1 {
            crate::warn_once!("capture skips MSAA eye textures (sample_count > 1)");
            return;
        }
        if source.width == 0 || source.height == 0 {
            return;
        }
        if self.config.mode == CaptureMode::Shutter {
            if crate::capture::shutter_requested() && !self.pending && self.flight.is_none() {
                self.pending = true;
                self.sources = [None; 2];
            }
        } else if !self.pending && now >= self.next_due && self.flight.is_none() {
            self.pending = true;
            self.sources = [None; 2];
        }
        if self.pending {
            self.sources[eye] = Some(source);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn flush(
        &mut self,
        device: &ash::Device,
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        queue: vk::Queue,
        now: Instant,
    ) {
        if let Some(flight) = self.flight.as_ref() {
            match unsafe { device.get_fence_status(flight.fence) } {
                Ok(true) => {
                    let flight = self.flight.take().unwrap();
                    self.finish_flight(device, flight, now);
                }
                Ok(false) => {}
                Err(error) => {
                    warn!("capture fence poll failed: {error:?}");
                    let flight = self.flight.take().unwrap();
                    destroy_flight(device, flight);
                    self.next_due = now + self.config.interval;
                }
            }
        }

        // Destroying the pipeline is only safe once the previous readback has
        // completed. A resize between captures must still discard the cached
        // views, otherwise A -> B -> A could reuse a view of the old A image.
        if self.flight.is_none() {
            if self.source_extent.take_dirty() {
                if let Some(old) = self.pipeline.take() {
                    unsafe { old.destroy(device) };
                }
                self.pipeline_key = None;
            }
            if let Some(pipeline) = self.pipeline.as_mut() {
                unsafe { pipeline.prepare(device) };
            }
        }

        if !self.pending || self.stopped || self.flight.is_some() {
            return;
        }
        let (Some(left), Some(right)) = (self.sources[0], self.sources[1]) else {
            self.pending = false;
            self.next_due = now + self.config.interval;
            return;
        };
        self.pending = false;
        self.sources = [None; 2];
        if left.width != right.width || left.height != right.height {
            warn!("capture skipped: eye sizes differ");
            self.next_due = now + self.config.interval;
            return;
        }
        let key = (left.format, left.width, left.height);
        if self.pipeline_key != Some(key) {
            if let Some(old) = self.pipeline.take() {
                unsafe { old.destroy(device) };
            }
            match unsafe {
                CapturePipeline::new(
                    device,
                    instance,
                    physical_device,
                    left.format,
                    left.width,
                    left.height,
                )
            } {
                Ok(pipeline) => {
                    self.pipeline = Some(pipeline);
                    self.pipeline_key = Some(key);
                }
                Err(error) => {
                    warn!("capture pipeline creation failed: {error:?}");
                    self.next_due = now + self.config.interval;
                    return;
                }
            }
        }
        let size = u64::from(left.width) * u64::from(left.height) * 4;
        let (Some((left_buffer, left_memory)), Some((right_buffer, right_memory))) = (unsafe {
            (
                create_host_buffer(device, instance, physical_device, size),
                create_host_buffer(device, instance, physical_device, size),
            )
        }) else {
            warn!("capture host buffer creation failed");
            self.next_due = now + self.config.interval;
            return;
        };
        let fence = match unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) } {
            Ok(fence) => fence,
            Err(error) => {
                warn!("capture fence creation failed: {error:?}");
                unsafe {
                    device.destroy_buffer(left_buffer, None);
                    device.free_memory(left_memory, None);
                    device.destroy_buffer(right_buffer, None);
                    device.free_memory(right_memory, None);
                }
                self.next_due = now + self.config.interval;
                return;
            }
        };
        let pipeline = self.pipeline.as_mut().unwrap();
        unsafe {
            device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .ok();
            device
                .begin_command_buffer(
                    self.command_buffer,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .ok();
        }
        let left_ok =
            unsafe { pipeline.record(device, self.command_buffer, 0, &left, left_buffer) }.is_ok();
        let right_ok = left_ok
            && unsafe { pipeline.record(device, self.command_buffer, 1, &right, right_buffer) }
                .is_ok();
        unsafe {
            device.end_command_buffer(self.command_buffer).ok();
        }
        if !left_ok || !right_ok {
            warn!("capture command recording failed");
            unsafe {
                device.destroy_fence(fence, None);
                device.destroy_buffer(left_buffer, None);
                device.free_memory(left_memory, None);
                device.destroy_buffer(right_buffer, None);
                device.free_memory(right_memory, None);
            }
            self.next_due = now + self.config.interval;
            return;
        }
        let submit = unsafe {
            device.queue_submit(
                queue,
                &[vk::SubmitInfo::default().command_buffers(&[self.command_buffer])],
                fence,
            )
        };
        if let Err(error) = submit {
            warn!("capture queue submit failed: {error:?}");
            unsafe {
                device.destroy_fence(fence, None);
                device.destroy_buffer(left_buffer, None);
                device.free_memory(left_memory, None);
                device.destroy_buffer(right_buffer, None);
                device.free_memory(right_memory, None);
            }
            self.next_due = now + self.config.interval;
            return;
        }
        let frame_time_ms = now
            .checked_duration_since(self.last_capture)
            .map(|duration| duration.as_secs_f32() * 1000.0)
            .unwrap_or(0.0);
        self.last_capture = now;
        self.next_due = now + self.config.interval;
        if self.config.mode == CaptureMode::Shutter {
            crate::capture::clear_shutter_request();
        }
        self.flight = Some(CaptureFlight {
            fence,
            buffers: [left_buffer, right_buffer],
            memories: [left_memory, right_memory],
            sizes: [size, size],
            sources: [left, right],
            frame_time_ms,
            timestamp_unix_ms: now_unix_ms(),
        });
    }

    #[allow(clippy::needless_range_loop)]
    fn finish_flight(&mut self, device: &ash::Device, flight: CaptureFlight, _now: Instant) {
        let mut readbacks = [Vec::new(), Vec::new()];
        for eye in 0..2 {
            let result = unsafe {
                device.map_memory(
                    flight.memories[eye],
                    0,
                    flight.sizes[eye],
                    vk::MemoryMapFlags::empty(),
                )
            };
            match result {
                Ok(pointer) => {
                    let slice = unsafe {
                        std::slice::from_raw_parts(pointer.cast::<u8>(), flight.sizes[eye] as usize)
                    };
                    readbacks[eye].extend_from_slice(slice);
                    unsafe { device.unmap_memory(flight.memories[eye]) };
                }
                Err(error) => {
                    warn!("capture readback map failed: {error:?}");
                    destroy_flight(device, flight);
                    self.next_due = Instant::now() + self.config.interval;
                    return;
                }
            }
        }

        let index = self.next_index;
        let left_file = format!("frame_{index:06}_left.png");
        let right_file = format!("frame_{index:06}_right.png");
        let left_meta = eye_meta(&left_file, &flight.sources[0]);
        let right_meta = eye_meta(&right_file, &flight.sources[1]);
        let frame = FrameMeta {
            index,
            timestamp_unix_ms: flight.timestamp_unix_ms,
            frame_time_ms: flight.frame_time_ms,
            render_scale: crate::settings::current_supersample_scale(),
            left: left_meta,
            right: right_meta,
        };
        let width = flight.sources[0].width;
        let height = flight.sources[0].height;
        let [left, right] = readbacks;
        let job = CaptureJob {
            notify: self.config.mode == CaptureMode::Shutter,
            left,
            right,
            left_path: self.config.dir.join(&left_file),
            right_path: self.config.dir.join(&right_file),
            width,
            height,
            frame,
        };
        destroy_flight(device, flight);
        self.writer.send(job);
        self.next_index = self.next_index.saturating_add(1);
        self.saved_pairs = self.saved_pairs.saturating_add(1);
        if self.saved_pairs >= self.config.max_pairs {
            self.stopped = true;
            crate::capture::clear_shutter_request();
            info!(
                "training capture stopped after {} pairs (max_pairs={})",
                self.saved_pairs, self.config.max_pairs
            );
        }
    }

    pub fn destroy(&mut self, device: &ash::Device) {
        self.stopped = true;
        crate::capture::clear_shutter_request();
        if let Some(flight) = self.flight.take() {
            unsafe {
                let _ = device.wait_for_fences(&[flight.fence], true, u64::MAX);
            }
            destroy_flight(device, flight);
        }
        if let Some(pipeline) = self.pipeline.take() {
            unsafe { pipeline.destroy(device) };
        }
        unsafe {
            device.destroy_command_pool(self.command_pool, None);
        }
    }
}

fn destroy_flight(device: &ash::Device, flight: CaptureFlight) {
    unsafe {
        device.destroy_fence(flight.fence, None);
        for eye in 0..2 {
            device.destroy_buffer(flight.buffers[eye], None);
            device.free_memory(flight.memories[eye], None);
        }
    }
}

fn eye_meta(file: &str, source: &CaptureSource) -> EyeMeta {
    EyeMeta {
        file: file.to_string(),
        width: source.width,
        height: source.height,
        format: format!("{:?}", source.format),
        color_space: source.color_space.to_string(),
        array_layer: source.layer,
        bounds: source.bounds,
    }
}

fn scan_next_index(dir: &Path) -> u32 {
    let mut next = 1;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(rest) = name.strip_prefix("frame_")
                && let Some(number) = rest.split('_').next()
                && let Ok(index) = number.parse::<u32>()
            {
                next = next.max(index.saturating_add(1));
            }
        }
    }
    next
}

fn game_name() -> String {
    std::fs::read_link("/proc/self/exe")
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

struct CapturePipeline {
    render_pass: vk::RenderPass,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    set_layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    sets: [vk::DescriptorSet; 2],
    sampler: vk::Sampler,
    image: vk::Image,
    memory: vk::DeviceMemory,
    views: [vk::ImageView; 2],
    framebuffers: [vk::Framebuffer; 2],
    source_views: SourceViewCache,
    width: u32,
    height: u32,
}

impl CapturePipeline {
    unsafe fn new(
        device: &ash::Device,
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        source_format: vk::Format,
        width: u32,
        height: u32,
    ) -> Result<Self, vk::Result> {
        unsafe {
            let _ = source_format;
            let target_format = vk::Format::R8G8B8A8_UNORM;
            let attachment = vk::AttachmentDescription {
                format: target_format,
                samples: vk::SampleCountFlags::TYPE_1,
                load_op: vk::AttachmentLoadOp::DONT_CARE,
                store_op: vk::AttachmentStoreOp::STORE,
                stencil_load_op: vk::AttachmentLoadOp::DONT_CARE,
                stencil_store_op: vk::AttachmentStoreOp::DONT_CARE,
                initial_layout: vk::ImageLayout::UNDEFINED,
                final_layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
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
            let layout = device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(std::slice::from_ref(&set_layout)),
                None,
            )?;
            let vertex_module = create_shader_module(
                device,
                &include_bytes!(concat!(env!("OUT_DIR"), "/vert_capture.spv"))[..],
            )?;
            let fragment_module = create_shader_module(
                device,
                &include_bytes!(concat!(env!("OUT_DIR"), "/frag_capture.spv"))[..],
            )?;
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
            let pipeline = device.create_graphics_pipelines(
                vk::PipelineCache::null(),
                std::slice::from_ref(&pipeline_info),
                None,
            );
            device.destroy_shader_module(vertex_module, None);
            device.destroy_shader_module(fragment_module, None);
            let pipeline = pipeline.map_err(|(_, error)| error)?[0];
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
            let (image, memory) =
                create_capture_image(device, instance, physical_device, width, height)?;
            let mut views = [vk::ImageView::null(); 2];
            let mut framebuffers = [vk::Framebuffer::null(); 2];
            for eye in 0..2 {
                views[eye] = device.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
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
                framebuffers[eye] = device.create_framebuffer(
                    &vk::FramebufferCreateInfo::default()
                        .render_pass(render_pass)
                        .attachments(std::slice::from_ref(&views[eye]))
                        .width(width)
                        .height(height)
                        .layers(1),
                    None,
                )?;
            }
            Ok(Self {
                render_pass,
                pipeline,
                layout,
                set_layout,
                pool,
                sets: [sets[0], sets[1]],
                sampler,
                image,
                memory,
                views,
                framebuffers,
                source_views: SourceViewCache::default(),
                width,
                height,
            })
        }
    }

    /// Bound the cache at a safe point (after any previous readback finished).
    unsafe fn prepare(&mut self, device: &ash::Device) {
        unsafe {
            if self.source_views.count() < 16 {
                return;
            }
            let _ = device.device_wait_idle();
            for view in self.source_views.drain() {
                device.destroy_image_view(view, None);
            }
        }
    }

    unsafe fn record(
        &mut self,
        device: &ash::Device,
        command_buffer: vk::CommandBuffer,
        eye: usize,
        source: &CaptureSource,
        buffer: vk::Buffer,
    ) -> Result<(), vk::Result> {
        unsafe {
            let source_extent = (source.width, source.height);
            let key = SourceKey::new(source.image, source.format, source.layer, source_extent);
            let view = if let Some(view) = self.source_views.get(key) {
                view
            } else {
                let view = device.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(source.image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(raw_view_format(source.format))
                        .components(vk::ComponentMapping::default())
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: source.layer,
                            layer_count: 1,
                        }),
                    None,
                )?;
                self.source_views.insert(key, view);
                view
            };
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
            let range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: source.layer,
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
                    image: source.image,
                    subresource_range: range,
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
            device.cmd_begin_render_pass(
                command_buffer,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(self.render_pass)
                    .framebuffer(self.framebuffers[eye])
                    .render_area(render_area),
                vk::SubpassContents::INLINE,
            );
            device.cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline,
            );
            device.cmd_set_viewport(
                command_buffer,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: self.width as f32,
                    height: self.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            device.cmd_set_scissor(command_buffer, 0, std::slice::from_ref(&render_area));
            device.cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                self.layout,
                0,
                &[self.sets[eye]],
                &[],
            );
            device.cmd_draw(command_buffer, 3, 1, 0, 0);
            device.cmd_end_render_pass(command_buffer);
            device.cmd_copy_image_to_buffer(
                command_buffer,
                self.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer,
                &[vk::BufferImageCopy {
                    buffer_offset: 0,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: eye as u32,
                        layer_count: 1,
                    },
                    image_offset: vk::Offset3D::default(),
                    image_extent: vk::Extent3D {
                        width: self.width,
                        height: self.height,
                        depth: 1,
                    },
                }],
            );
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
                    image: source.image,
                    subresource_range: range,
                    ..Default::default()
                }],
            );
            Ok(())
        }
    }

    unsafe fn destroy(&self, device: &ash::Device) {
        unsafe {
            for view in self.source_views.iter() {
                device.destroy_image_view(view, None);
            }
            for view in &self.views {
                device.destroy_image_view(*view, None);
            }
            for framebuffer in &self.framebuffers {
                device.destroy_framebuffer(*framebuffer, None);
            }
            device.destroy_image(self.image, None);
            device.free_memory(self.memory, None);
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
        let words =
            ash::util::read_spv(&mut std::io::Cursor::new(bytes)).expect("valid capture shader");
        device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
    }
}

unsafe fn create_capture_image(
    device: &ash::Device,
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    width: u32,
    height: u32,
) -> Result<(vk::Image, vk::DeviceMemory), vk::Result> {
    unsafe {
        let image = device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(2)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        )?;
        let requirements = device.get_image_memory_requirements(image);
        let properties = instance.get_physical_device_memory_properties(physical_device);
        let Some(memory_type) = (0..properties.memory_type_count).find(|index| {
            requirements.memory_type_bits & (1 << index) != 0
                && properties.memory_types[*index as usize]
                    .property_flags
                    .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        }) else {
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

unsafe fn create_host_buffer(
    device: &ash::Device,
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    size: u64,
) -> Option<(vk::Buffer, vk::DeviceMemory)> {
    unsafe {
        let buffer = device
            .create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size)
                    .usage(vk::BufferUsageFlags::TRANSFER_DST)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )
            .ok()?;
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
            return None;
        };
        let memory = device
            .allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(memory_type),
                None,
            )
            .ok()?;
        device.bind_buffer_memory(buffer, memory, 0).ok()?;
        Some((buffer, memory))
    }
}

fn raw_view_format(format: vk::Format) -> vk::Format {
    match format {
        vk::Format::R8G8B8A8_SRGB => vk::Format::R8G8B8A8_UNORM,
        vk::Format::B8G8R8A8_SRGB => vk::Format::B8G8R8A8_UNORM,
        vk::Format::BC1_RGB_SRGB_BLOCK => vk::Format::BC1_RGB_UNORM_BLOCK,
        vk::Format::BC1_RGBA_SRGB_BLOCK => vk::Format::BC1_RGBA_UNORM_BLOCK,
        vk::Format::BC2_SRGB_BLOCK => vk::Format::BC2_UNORM_BLOCK,
        vk::Format::BC3_SRGB_BLOCK => vk::Format::BC3_UNORM_BLOCK,
        vk::Format::BC7_SRGB_BLOCK => vk::Format::BC7_UNORM_BLOCK,
        vk::Format::ETC2_R8G8B8_SRGB_BLOCK => vk::Format::ETC2_R8G8B8_UNORM_BLOCK,
        vk::Format::ETC2_R8G8B8A1_SRGB_BLOCK => vk::Format::ETC2_R8G8B8A1_UNORM_BLOCK,
        vk::Format::ETC2_R8G8B8A8_SRGB_BLOCK => vk::Format::ETC2_R8G8B8A8_UNORM_BLOCK,
        vk::Format::ASTC_4X4_SRGB_BLOCK => vk::Format::ASTC_4X4_UNORM_BLOCK,
        vk::Format::ASTC_5X4_SRGB_BLOCK => vk::Format::ASTC_5X4_UNORM_BLOCK,
        vk::Format::ASTC_5X5_SRGB_BLOCK => vk::Format::ASTC_5X5_UNORM_BLOCK,
        vk::Format::ASTC_6X5_SRGB_BLOCK => vk::Format::ASTC_6X5_UNORM_BLOCK,
        vk::Format::ASTC_6X6_SRGB_BLOCK => vk::Format::ASTC_6X6_UNORM_BLOCK,
        vk::Format::ASTC_8X5_SRGB_BLOCK => vk::Format::ASTC_8X5_UNORM_BLOCK,
        vk::Format::ASTC_8X6_SRGB_BLOCK => vk::Format::ASTC_8X6_UNORM_BLOCK,
        vk::Format::ASTC_8X8_SRGB_BLOCK => vk::Format::ASTC_8X8_UNORM_BLOCK,
        vk::Format::ASTC_10X5_SRGB_BLOCK => vk::Format::ASTC_10X5_UNORM_BLOCK,
        vk::Format::ASTC_10X6_SRGB_BLOCK => vk::Format::ASTC_10X6_UNORM_BLOCK,
        vk::Format::ASTC_10X8_SRGB_BLOCK => vk::Format::ASTC_10X8_UNORM_BLOCK,
        vk::Format::ASTC_10X10_SRGB_BLOCK => vk::Format::ASTC_10X10_UNORM_BLOCK,
        vk::Format::ASTC_12X10_SRGB_BLOCK => vk::Format::ASTC_12X10_UNORM_BLOCK,
        vk::Format::ASTC_12X12_SRGB_BLOCK => vk::Format::ASTC_12X12_UNORM_BLOCK,
        other => other,
    }
}

pub fn color_space_name(color_space: vr::EColorSpace) -> &'static str {
    match color_space {
        vr::EColorSpace::Auto | vr::EColorSpace::Gamma => "srgb",
        vr::EColorSpace::Linear => "linear",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_next_index_skips_existing_pairs() {
        let dir =
            std::env::temp_dir().join(format!("xrizer_capture_scan_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("frame_000007_left.png"), b"x").unwrap();
        std::fs::write(dir.join("frame_000007_right.png"), b"x").unwrap();
        std::fs::write(dir.join("frame_000012_left.png"), b"x").unwrap();
        assert_eq!(scan_next_index(&dir), 13);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn srgb_views_use_unorm_formats() {
        assert_eq!(
            raw_view_format(vk::Format::R8G8B8A8_SRGB),
            vk::Format::R8G8B8A8_UNORM
        );
        assert_eq!(
            raw_view_format(vk::Format::BC3_SRGB_BLOCK),
            vk::Format::BC3_UNORM_BLOCK
        );
        assert_eq!(
            raw_view_format(vk::Format::R8G8B8A8_UNORM),
            vk::Format::R8G8B8A8_UNORM
        );
    }

    #[test]
    fn filenames_and_metadata_are_dataset_compatible() {
        let source = CaptureSource {
            image: vk::Image::null(),
            format: vk::Format::R8G8B8A8_SRGB,
            layer: 0,
            width: 16,
            height: 8,
            bounds: [0.0, 0.0, 1.0, 1.0],
            color_space: "srgb",
            sample_count: 1,
        };
        let frame = FrameMeta {
            index: 42,
            timestamp_unix_ms: 1,
            frame_time_ms: 500.0,
            render_scale: 0.7,
            left: eye_meta("frame_000042_left.png", &source),
            right: eye_meta("frame_000042_right.png", &source),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains("frame_000042_left.png"));
        assert!(json.contains("\"render_scale\":0.7"));
        assert!(json.contains("\"color_space\":\"srgb\""));
    }

    #[test]
    fn png_writer_roundtrip() {
        let dir =
            std::env::temp_dir().join(format!("xrizer_capture_png_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let metadata = Arc::new(Mutex::new(CaptureMetadata {
            capture_version: 1,
            game: "test".into(),
            scene: "test".into(),
            aa_mode: "unknown".into(),
            color_space: "unknown".into(),
            eye: "both".into(),
            session_started_unix_ms: 1,
            render_scale: 1.0,
            output_scale: 1.0,
            preset: "fast".into(),
            mode: "interval".into(),
            frames: Vec::new(),
        }));
        let writer = CaptureWriter::new(dir.clone(), metadata.clone()).unwrap();
        let frame = FrameMeta {
            index: 1,
            timestamp_unix_ms: 1,
            frame_time_ms: 500.0,
            render_scale: 1.0,
            left: EyeMeta {
                file: "frame_000001_left.png".into(),
                width: 1,
                height: 1,
                format: "R8G8B8A8_UNORM".into(),
                color_space: "srgb".into(),
                array_layer: 0,
                bounds: [0.0, 0.0, 1.0, 1.0],
            },
            right: EyeMeta {
                file: "frame_000001_right.png".into(),
                width: 1,
                height: 1,
                format: "R8G8B8A8_UNORM".into(),
                color_space: "srgb".into(),
                array_layer: 0,
                bounds: [0.0, 0.0, 1.0, 1.0],
            },
        };
        writer.send(CaptureJob {
            notify: false,
            left: vec![255, 0, 0, 255],
            right: vec![0, 255, 0, 255],
            left_path: dir.join("frame_000001_left.png"),
            right_path: dir.join("frame_000001_right.png"),
            width: 1,
            height: 1,
            frame,
        });
        drop(writer);
        assert!(dir.join("frame_000001_left.png").exists());
        assert!(dir.join("frame_000001_right.png").exists());
        let decoder = png::Decoder::new(File::open(dir.join("frame_000001_left.png")).unwrap());
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (1, 1));
        assert_eq!(&pixels[..4], &[255, 0, 0, 255]);

        let parsed: CaptureMetadata =
            serde_json::from_slice(&std::fs::read(dir.join("metadata.json")).unwrap()).unwrap();
        assert_eq!(parsed.frames.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
