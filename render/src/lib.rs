//! Vulkan 1.3 renderer for the terminal grid.
// Ash leaves handle lifetime and external synchronization to the caller; this owner retains
// created resources until device idle, and frame fences guard every mapped-buffer reuse.

use std::ffi::{CStr, CString};
use std::mem::size_of;
use std::ptr;

use ash::{Device, Entry, Instance, vk};
use aterm_core::{Cell, CellFlags, DEFAULT_COLOR, INDEXED_COLOR_TAG, RGB_COLOR_TAG, Terminal};
use aterm_font::{Font, GlyphUpload};
use bytemuck::{Pod, Zeroable};

const FRAMES: usize = 2;
const ATLAS_SIZE: u32 = 1024;
const MAX_COLS: usize = 512;
const MAX_ROWS: usize = 256;

#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
pub struct GpuCell {
    pub glyph: u32,
    pub fg: u32,
    pub bg: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Pod, Zeroable)]
struct GpuGlyph {
    uv: [f32; 4],
    size_bearing: [f32; 4],
    bearing_advance: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PushGrid {
    cols: u32,
    rows: u32,
    cell_width: f32,
    cell_height: f32,
    ascent: f32,
    cursor: u32,
    cursor_visible: u32,
    selection_start: u32,
    selection_end: u32,
    cursor_style: u32,
}

/// Maps a DECSCUSR cursor shape to the push-constant encoding.
fn push_cursor_style(style: aterm_core::CursorStyle) -> u32 {
    match style {
        aterm_core::CursorStyle::Block => 0,
        aterm_core::CursorStyle::Underline => 1,
        aterm_core::CursorStyle::Bar => 2,
    }
}

/// First and one-past-end cell rows of the underline decoration.
/// Mirrors the `thick` computation in the background shader.
#[cfg(test)]
fn underline_rows(cell_height: u32) -> (u32, u32) {
    let thick = (cell_height >> 4).max(1);
    (cell_height - thick, cell_height)
}

/// First and one-past-end cell rows of the strikethrough decoration.
/// Mirrors the background shader centering.
#[cfg(test)]
fn strikethrough_rows(cell_height: u32) -> (u32, u32) {
    let thick = (cell_height >> 4).max(1);
    let start = cell_height / 2 - thick / 2;
    (start, start + thick)
}

/// Pixel span of thin (bar/underline) cursors. Mirrors the background shader.
#[cfg(test)]
fn cursor_span(cell_extent: u32) -> u32 {
    2 * (cell_extent >> 4).max(1)
}

struct Buffer {
    handle: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped: *mut u8,
    size: vk::DeviceSize,
}

struct Frame {
    command_pool: vk::CommandPool,
    command: vk::CommandBuffer,
    image_available: vk::Semaphore,
    fence: vk::Fence,
    cells: Buffer,
    glyphs: Buffer,
    descriptors: vk::DescriptorSet,
    dirty_rows: Vec<bool>,
    glyphs_dirty: bool,
}

struct SwapchainState {
    handle: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    format: vk::Format,
    extent: vk::Extent2D,
}

struct SurfaceContext<'a> {
    device: &'a Device,
    surface_loader: &'a ash::khr::surface::Instance,
    swapchain_loader: &'a ash::khr::swapchain::Device,
    physical: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
    queue_family: u32,
}

struct FrameSetup<'a> {
    instance: &'a Instance,
    device: &'a Device,
    physical: vk::PhysicalDevice,
    queue_family: u32,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
    atlas_view: vk::ImageView,
    sampler: vk::Sampler,
    color_atlas_view: vk::ImageView,
    color_sampler: vk::Sampler,
}

struct AtlasUploadContext<'a> {
    instance: &'a Instance,
    device: &'a Device,
    physical: vk::PhysicalDevice,
    queue: vk::Queue,
    queue_family: u32,
    atlas: vk::Image,
}

struct FrameDraw {
    image: vk::Image,
    view: vk::ImageView,
    background: vk::Pipeline,
    text: vk::Pipeline,
    layout: vk::PipelineLayout,
    push: PushGrid,
    instances: u32,
    extent: vk::Extent2D,
    final_layout: vk::ImageLayout,
    readback: Option<vk::Buffer>,
}

/// Owns the surface-bound Vulkan device and all frame-in-flight resources.
pub struct Renderer {
    _entry: Entry,
    instance: Instance,
    surface_loader: ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
    physical: vk::PhysicalDevice,
    device: Device,
    queue: vk::Queue,
    queue_family: u32,
    swapchain_loader: ash::khr::swapchain::Device,
    swapchain: vk::SwapchainKHR,
    swapchain_images: Vec<vk::Image>,
    image_views: Vec<vk::ImageView>,
    present_ready: Vec<vk::Semaphore>,
    format: vk::Format,
    extent: vk::Extent2D,
    descriptor_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    pipeline_layout: vk::PipelineLayout,
    background_pipeline: vk::Pipeline,
    text_pipeline: vk::Pipeline,
    atlas: vk::Image,
    atlas_memory: vk::DeviceMemory,
    atlas_view: vk::ImageView,
    atlas_sampler: vk::Sampler,
    color_atlas: vk::Image,
    color_atlas_memory: vk::DeviceMemory,
    color_atlas_view: vk::ImageView,
    color_atlas_sampler: vk::Sampler,
    frames: Vec<Frame>,
    frame_index: usize,
    gpu_cells: Vec<GpuCell>,
    gpu_glyphs: Vec<GpuGlyph>,
    uploaded_atlas: bool,
    uploaded_color_atlas: bool,
    pending_glyph_uploads: Vec<GlyphUpload>,
    glyphs_dirty: bool,
    grid_size: Option<(usize, usize)>,
}

/// Vulkan renderer for deterministic image readback without a window system.
pub struct OffscreenRenderer {
    _entry: Entry,
    instance: Instance,
    physical: vk::PhysicalDevice,
    device: Device,
    queue: vk::Queue,
    queue_family: u32,
    descriptor_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    pipeline_layout: vk::PipelineLayout,
    background_pipeline: vk::Pipeline,
    text_pipeline: vk::Pipeline,
    atlas: vk::Image,
    atlas_memory: vk::DeviceMemory,
    atlas_view: vk::ImageView,
    atlas_sampler: vk::Sampler,
    color_atlas: vk::Image,
    color_atlas_memory: vk::DeviceMemory,
    color_atlas_view: vk::ImageView,
    color_atlas_sampler: vk::Sampler,
    target: vk::Image,
    target_memory: vk::DeviceMemory,
    target_view: vk::ImageView,
    extent: vk::Extent2D,
    frames: Vec<Frame>,
    atlas_uploaded: bool,
    color_uploaded: bool,
    show_cursor: bool,
}

impl OffscreenRenderer {
    /// Creates Vulkan 1.3 graphics resources without requesting any surface extensions.
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("offscreen target dimensions must be nonzero".into());
        }
        let entry = unsafe { Entry::load() }.map_err(|error| format!("Vulkan loader: {error}"))?;
        let app_name = CString::new("aterm-offscreen").expect("static string has no nul");
        let app = vk::ApplicationInfo::default()
            .application_name(&app_name)
            .application_version(vk::make_api_version(0, 0, 1, 0))
            .api_version(vk::API_VERSION_1_3);
        let instance = unsafe {
            entry.create_instance(
                &vk::InstanceCreateInfo::default().application_info(&app),
                None,
            )
        }
        .map_err(vk_error("offscreen Vulkan instance creation"))?;
        let (physical, queue_family) = choose_offscreen_device(&instance)?;
        let priority = [1.0f32];
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priority)];
        let mut features13 = vk::PhysicalDeviceVulkan13Features::default()
            .dynamic_rendering(true)
            .synchronization2(true);
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .push_next(&mut features13);
        let device = unsafe { instance.create_device(physical, &device_info, None) }
            .map_err(vk_error("offscreen Vulkan logical device creation"))?;
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        let descriptor_layout = create_descriptor_layout(&device)?;
        let push_range = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(size_of::<PushGrid>() as u32)];
        let pipeline_layout = unsafe {
            device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(&[descriptor_layout])
                    .push_constant_ranges(&push_range),
                None,
            )
        }
        .map_err(vk_error("offscreen pipeline layout"))?;
        let format = vk::Format::R8G8B8A8_UNORM;
        let background_pipeline = create_pipeline(
            &device,
            pipeline_layout,
            format,
            include_bytes!(concat!(env!("OUT_DIR"), "/background.vert.spv")),
            include_bytes!(concat!(env!("OUT_DIR"), "/background.frag.spv")),
            false,
        )?;
        let text_pipeline = create_pipeline(
            &device,
            pipeline_layout,
            format,
            include_bytes!(concat!(env!("OUT_DIR"), "/text.vert.spv")),
            include_bytes!(concat!(env!("OUT_DIR"), "/text.frag.spv")),
            true,
        )?;
        let (atlas, atlas_memory, atlas_view, atlas_sampler) =
            create_atlas(&instance, &device, physical, vk::Format::R8_UNORM)?;
        let (color_atlas, color_atlas_memory, color_atlas_view, color_atlas_sampler) =
            create_atlas(&instance, &device, physical, vk::Format::R8G8B8A8_UNORM)?;
        let (target, target_memory, target_view) =
            create_offscreen_target(&instance, &device, physical, width, height)?;
        let descriptor_pool = create_descriptor_pool(&device)?;
        let frames = create_frames(FrameSetup {
            instance: &instance,
            device: &device,
            physical,
            queue_family,
            pool: descriptor_pool,
            layout: descriptor_layout,
            atlas_view,
            sampler: atlas_sampler,
            color_atlas_view,
            color_sampler: color_atlas_sampler,
        })?;
        Ok(Self {
            _entry: entry,
            instance,
            physical,
            device,
            queue,
            queue_family,
            descriptor_layout,
            descriptor_pool,
            pipeline_layout,
            background_pipeline,
            text_pipeline,
            atlas,
            atlas_memory,
            atlas_view,
            atlas_sampler,
            color_atlas,
            color_atlas_memory,
            color_atlas_view,
            color_atlas_sampler,
            target,
            target_memory,
            target_view,
            extent: vk::Extent2D { width, height },
            frames,
            atlas_uploaded: false,
            color_uploaded: false,
            show_cursor: false,
        })
    }

    /// Enables the terminal cursor in snapshots; snapshots hide it by default.
    pub const fn set_show_cursor(&mut self, show: bool) {
        self.show_cursor = show;
    }

    /// Marks the atlas as needing a full re-upload on the next render call.
    pub fn invalidate_atlas(&mut self) {
        self.atlas_uploaded = false;
        self.color_uploaded = false;
    }

    /// Renders the current viewport and returns tightly packed RGBA8 pixels.
    pub fn render(&mut self, terminal: &mut Terminal, font: &mut Font) -> Result<Vec<u8>, String> {
        let grid = terminal.active_grid();
        let (cols, rows) = (grid.width(), grid.height());
        let metrics = font.metrics();
        let expected_width = (cols as u32)
            .checked_mul(metrics.width)
            .ok_or("offscreen grid width overflow")?;
        let expected_height = (rows as u32)
            .checked_mul(metrics.height)
            .ok_or("offscreen grid height overflow")?;
        if expected_width != self.extent.width || expected_height != self.extent.height {
            return Err("offscreen target must exactly match terminal pixel dimensions".into());
        }
        let count = cols
            .checked_mul(rows)
            .ok_or("terminal grid size overflow")?;
        let mut cells = vec![GpuCell::zeroed(); count];
        let mut glyphs = vec![GpuGlyph::zeroed()];
        let mut uploads = Vec::new();
        for row in 0..rows {
            for column in 0..cols {
                let cell = terminal.viewport_cell(row, column).unwrap_or(Cell::EMPTY);
                if cell.content == 0 || cell.flags().contains(CellFlags::WIDE_SPACER) {
                    cells[row * cols + column] = GpuCell {
                        glyph: u32::MAX,
                        fg: color(cell.fg, true),
                        bg: color(cell.bg, false),
                        flags: cell.flags,
                    };
                    continue;
                }
                let ch = char::from_u32(cell.content).unwrap_or('\u{fffd}');
                let (pre, post) = if Font::requires_contextual_shaping(ch) {
                    terminal.viewport_shaping_context(row, column)
                } else {
                    (String::new(), String::new())
                };
                let bold = cell.flags().contains(CellFlags::BOLD);
                let italic = cell.flags().contains(CellFlags::ITALIC);
                let (glyph, info, upload) = match terminal.viewport_stored_grapheme(row, column) {
                    Some(grapheme) if pre.is_empty() && post.is_empty() => {
                        font.glyph_text(grapheme, bold, italic)?
                    }
                    Some(grapheme) => {
                        font.glyph_text_with_context(grapheme, &pre, &post, bold, italic)?
                    }
                    None if !pre.is_empty() || !post.is_empty() => {
                        font.glyph_text_with_context(&ch.to_string(), &pre, &post, bold, italic)?
                    }
                    None => font.glyph(ch, bold, italic)?,
                };
                if let Some(upload) = upload {
                    uploads.push(upload);
                }
                let metadata = GpuGlyph {
                    uv: [
                        info.uv_min[0],
                        info.uv_min[1],
                        info.uv_max[0],
                        info.uv_max[1],
                    ],
                    size_bearing: [info.size[0], info.size[1], info.bearing[0], info.bearing[1]],
                    // The padding lane carries the color-atlas selector the
                    // text shader reads; the std430 layout is unchanged.
                    bearing_advance: [
                        info.bearing[0],
                        info.bearing[1],
                        info.advance,
                        u32::from(info.color) as f32,
                    ],
                };
                // Grow the glyph table without truncating entries that earlier
                // cells in this frame already recorded; `resize` would shrink it
                // when a cache hit references a smaller index than the maximum.
                if glyphs.len() <= glyph as usize {
                    glyphs.resize(glyph as usize + 1, GpuGlyph::zeroed());
                }
                glyphs[glyph as usize] = metadata;
                cells[row * cols + column] = GpuCell {
                    glyph,
                    fg: color(cell.fg, true),
                    bg: color(cell.bg, false),
                    flags: cell.flags,
                };
            }
        }

        let frame = &mut self.frames[0];
        unsafe { self.device.wait_for_fences(&[frame.fence], true, u64::MAX) }
            .map_err(vk_error("wait for offscreen frame"))?;
        let cell_bytes = bytemuck::cast_slice(&cells);
        let glyph_bytes = bytemuck::cast_slice(&glyphs);
        if cell_bytes.len() as u64 > frame.cells.size
            || glyph_bytes.len() as u64 > frame.glyphs.size
        {
            return Err("offscreen terminal data exceeds SSBO capacity".into());
        }
        unsafe {
            ptr::copy_nonoverlapping(cell_bytes.as_ptr(), frame.cells.mapped, cell_bytes.len());
            ptr::copy_nonoverlapping(glyph_bytes.as_ptr(), frame.glyphs.mapped, glyph_bytes.len());
        }
        if !self.atlas_uploaded {
            upload_offscreen_atlas(
                AtlasUploadContext {
                    instance: &self.instance,
                    device: &self.device,
                    physical: self.physical,
                    queue: self.queue,
                    queue_family: self.queue_family,
                    atlas: self.atlas,
                },
                font.atlas_pixels(),
                false,
            )?;
            upload_offscreen_atlas(
                AtlasUploadContext {
                    instance: &self.instance,
                    device: &self.device,
                    physical: self.physical,
                    queue: self.queue,
                    queue_family: self.queue_family,
                    atlas: self.color_atlas,
                },
                font.color_atlas_pixels(),
                true,
            )?;
            self.atlas_uploaded = true;
            self.color_uploaded = true;
        } else if !uploads.is_empty() {
            // Monochrome and color uploads target different atlas images.
            let (color_uploads, mask_uploads): (Vec<GlyphUpload>, Vec<GlyphUpload>) =
                uploads.into_iter().partition(|upload| upload.color);
            if !mask_uploads.is_empty() {
                upload_atlas_regions(
                    AtlasUploadContext {
                        instance: &self.instance,
                        device: &self.device,
                        physical: self.physical,
                        queue: self.queue,
                        queue_family: self.queue_family,
                        atlas: self.atlas,
                    },
                    &mask_uploads,
                    true,
                )?;
            }
            if !color_uploads.is_empty() {
                upload_atlas_regions(
                    AtlasUploadContext {
                        instance: &self.instance,
                        device: &self.device,
                        physical: self.physical,
                        queue: self.queue,
                        queue_family: self.queue_family,
                        atlas: self.color_atlas,
                    },
                    &color_uploads,
                    self.color_uploaded,
                )?;
                self.color_uploaded = true;
            }
        }
        let readback = create_buffer(
            &self.instance,
            &self.device,
            self.physical,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            u64::from(self.extent.width) * u64::from(self.extent.height) * 4,
            vk::BufferUsageFlags::TRANSFER_DST,
        )?;
        let push = PushGrid {
            cols: cols as u32,
            rows: rows as u32,
            cell_width: metrics.width as f32,
            cell_height: metrics.height as f32,
            ascent: metrics.ascent as f32,
            cursor: (terminal.cursor().row * cols + terminal.cursor().column) as u32,
            cursor_visible: u32::from(self.show_cursor && terminal.modes().cursor_visible),
            selection_start: u32::MAX,
            selection_end: u32::MAX,
            cursor_style: push_cursor_style(terminal.modes().cursor_style),
        };
        record_frame(
            &self.device,
            frame,
            &FrameDraw {
                image: self.target,
                view: self.target_view,
                background: self.background_pipeline,
                text: self.text_pipeline,
                layout: self.pipeline_layout,
                push,
                instances: count as u32,
                extent: self.extent,
                final_layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                readback: Some(readback.handle),
            },
        )?;
        unsafe { self.device.reset_fences(&[frame.fence]) }
            .map_err(vk_error("reset offscreen frame fence"))?;
        let commands = [vk::CommandBufferSubmitInfo::default().command_buffer(frame.command)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&commands)];
        unsafe { self.device.queue_submit2(self.queue, &submit, frame.fence) }
            .map_err(vk_error("submit offscreen frame"))?;
        unsafe { self.device.wait_for_fences(&[frame.fence], true, u64::MAX) }
            .map_err(vk_error("wait for offscreen readback"))?;
        let byte_count = (self.extent.width * self.extent.height * 4) as usize;
        let pixels = unsafe { std::slice::from_raw_parts(readback.mapped, byte_count) }.to_vec();
        destroy_buffer(&self.device, readback);
        Ok(pixels)
    }
}

impl Renderer {
    /// Creates the XCB surface and a Vulkan 1.3 swapchain for an existing X11 window.
    pub fn new(
        connection: *mut std::ffi::c_void,
        window: u32,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        // Entry owns the dynamically loaded Vulkan loader for the entire instance lifetime.
        let entry = unsafe { Entry::load() }.map_err(|error| format!("Vulkan loader: {error}"))?;
        let app_name = CString::new("aterm").expect("static string has no nul");
        let app = vk::ApplicationInfo::default()
            .application_name(&app_name)
            .application_version(vk::make_api_version(0, 0, 1, 0))
            .engine_name(&app_name)
            .engine_version(vk::make_api_version(0, 0, 1, 0))
            .api_version(vk::API_VERSION_1_3);
        let extensions = [
            ash::khr::surface::NAME.as_ptr(),
            ash::khr::xcb_surface::NAME.as_ptr(),
        ];
        let instance_info = vk::InstanceCreateInfo::default()
            .application_info(&app)
            .enabled_extension_names(&extensions);
        // Instance extension names are static NUL-terminated Vulkan constants.
        let instance = unsafe { entry.create_instance(&instance_info, None) }
            .map_err(vk_error("Vulkan instance creation"))?;
        let surface_loader = ash::khr::surface::Instance::new(&entry, &instance);
        let xcb_loader = ash::khr::xcb_surface::Instance::new(&entry, &instance);
        let surface_info = vk::XcbSurfaceCreateInfoKHR::default()
            .connection(connection.cast())
            .window(window);
        // The XCB connection and window stay alive in app for longer than Renderer.
        let surface = unsafe { xcb_loader.create_xcb_surface(&surface_info, None) }
            .map_err(vk_error("XCB surface creation"))?;
        let (physical, queue_family) = choose_device(&instance, &surface_loader, surface)?;
        let priority = [1.0f32];
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priority)];
        let device_extensions = [ash::khr::swapchain::NAME.as_ptr()];
        let mut features13 = vk::PhysicalDeviceVulkan13Features::default()
            .dynamic_rendering(true)
            .synchronization2(true);
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_extension_names(&device_extensions)
            .push_next(&mut features13);
        let device = unsafe { instance.create_device(physical, &device_info, None) }
            .map_err(vk_error("Vulkan logical device creation"))?;
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        let swapchain_loader = ash::khr::swapchain::Device::new(&instance, &device);
        let surface_context = SurfaceContext {
            device: &device,
            surface_loader: &surface_loader,
            swapchain_loader: &swapchain_loader,
            physical,
            surface,
            queue_family,
        };
        let swap = create_swapchain(&surface_context, width, height)?;
        let descriptor_layout = create_descriptor_layout(&device)?;
        let push_range = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(size_of::<PushGrid>() as u32)];
        let set_layouts = [descriptor_layout];
        let pipeline_layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&push_range);
        let pipeline_layout = unsafe { device.create_pipeline_layout(&pipeline_layout_info, None) }
            .map_err(vk_error("Vulkan pipeline layout"))?;
        let background_pipeline = create_pipeline(
            &device,
            pipeline_layout,
            swap.format,
            include_bytes!(concat!(env!("OUT_DIR"), "/background.vert.spv")),
            include_bytes!(concat!(env!("OUT_DIR"), "/background.frag.spv")),
            false,
        )?;
        let text_pipeline = create_pipeline(
            &device,
            pipeline_layout,
            swap.format,
            include_bytes!(concat!(env!("OUT_DIR"), "/text.vert.spv")),
            include_bytes!(concat!(env!("OUT_DIR"), "/text.frag.spv")),
            true,
        )?;
        let (atlas, atlas_memory, atlas_view, atlas_sampler) =
            create_atlas(&instance, &device, physical, vk::Format::R8_UNORM)?;
        let (color_atlas, color_atlas_memory, color_atlas_view, color_atlas_sampler) =
            create_atlas(&instance, &device, physical, vk::Format::R8G8B8A8_UNORM)?;
        let descriptor_pool = create_descriptor_pool(&device)?;
        let frames = create_frames(FrameSetup {
            instance: &instance,
            device: &device,
            physical,
            queue_family,
            pool: descriptor_pool,
            layout: descriptor_layout,
            atlas_view,
            sampler: atlas_sampler,
            color_atlas_view,
            color_sampler: color_atlas_sampler,
        })?;
        let present_ready = create_present_semaphores(&device, swap.images.len())?;
        Ok(Self {
            _entry: entry,
            instance,
            surface_loader,
            surface,
            physical,
            device,
            queue,
            queue_family,
            swapchain_loader,
            swapchain: swap.handle,
            swapchain_images: swap.images,
            image_views: swap.views,
            present_ready,
            format: swap.format,
            extent: swap.extent,
            descriptor_layout,
            descriptor_pool,
            pipeline_layout,
            background_pipeline,
            text_pipeline,
            atlas,
            atlas_memory,
            atlas_view,
            atlas_sampler,
            color_atlas,
            color_atlas_memory,
            color_atlas_view,
            color_atlas_sampler,
            frames,
            frame_index: 0,
            gpu_cells: Vec::new(),
            gpu_glyphs: vec![GpuGlyph::zeroed()],
            uploaded_atlas: false,
            uploaded_color_atlas: false,
            pending_glyph_uploads: Vec::new(),
            glyphs_dirty: false,
            grid_size: None,
        })
    }

    /// Uploads changed grid rows and presents one frame.
    pub fn render(
        &mut self,
        terminal: &mut Terminal,
        font: &mut Font,
        cursor_blink_on: bool,
    ) -> Result<(), String> {
        let (width, height) = {
            let grid = terminal.active_grid();
            (grid.width(), grid.height())
        };
        let count = width * height;
        if width > MAX_COLS || height > MAX_ROWS {
            return Err(format!(
                "terminal grid exceeds per-frame SSBO capacity ({MAX_COLS}x{MAX_ROWS})"
            ));
        }
        if self.grid_size != Some((width, height)) {
            for frame in &mut self.frames {
                frame.dirty_rows.fill(true);
            }
            self.grid_size = Some((width, height));
        }
        self.gpu_cells.resize(
            count,
            GpuCell {
                glyph: u32::MAX,
                fg: 0x00d8d8d8,
                bg: 0x00181818,
                flags: 0,
            },
        );
        let dirty = terminal.take_dirty_rows();
        for row in dirty {
            for frame in &mut self.frames {
                frame.dirty_rows[row] = true;
            }
            for column in 0..width {
                let cell = terminal.viewport_cell(row, column).unwrap_or(Cell::EMPTY);
                let grapheme = terminal.viewport_stored_grapheme(row, column);
                let ch = char::from_u32(cell.content).unwrap_or('\u{fffd}');
                let (pre_context, post_context) = if Font::requires_contextual_shaping(ch) {
                    terminal.viewport_shaping_context(row, column)
                } else {
                    (String::new(), String::new())
                };
                self.gpu_cells[row * width + column] =
                    self.convert_cell(cell, grapheme, &pre_context, &post_context, font)?;
            }
        }
        let frame_index = self.frame_index;
        let fence = self.frames[frame_index].fence;
        let image_available = self.frames[frame_index].image_available;
        unsafe { self.device.wait_for_fences(&[fence], true, u64::MAX) }
            .map_err(vk_error("wait frame fence"))?;
        let (image_index, suboptimal) = match unsafe {
            self.swapchain_loader.acquire_next_image(
                self.swapchain,
                u64::MAX,
                image_available,
                vk::Fence::null(),
            )
        } {
            Ok(result) => result,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.resize(self.extent.width, self.extent.height)?;
                return Ok(());
            }
            Err(error) => return Err(format!("acquire swapchain image: {error:?}")),
        };
        if self.glyphs_dirty {
            for frame in &mut self.frames {
                frame.glyphs_dirty = true;
            }
            self.glyphs_dirty = false;
        }
        if !self.uploaded_atlas {
            self.upload_atlas(self.atlas, false, Some(font.atlas_pixels()), &[], false)?;
            self.upload_atlas(
                self.color_atlas,
                false,
                Some(font.color_atlas_pixels()),
                &[],
                true,
            )?;
            self.uploaded_atlas = true;
            self.uploaded_color_atlas = true;
            self.pending_glyph_uploads.clear();
        } else if !self.pending_glyph_uploads.is_empty() {
            let pending = std::mem::take(&mut self.pending_glyph_uploads);
            let (color_uploads, mask_uploads): (Vec<GlyphUpload>, Vec<GlyphUpload>) =
                pending.into_iter().partition(|upload| upload.color);
            if !mask_uploads.is_empty() {
                self.upload_atlas(self.atlas, true, None, &mask_uploads, false)?;
            }
            if !color_uploads.is_empty() {
                self.upload_atlas(
                    self.color_atlas,
                    self.uploaded_color_atlas,
                    None,
                    &color_uploads,
                    true,
                )?;
                self.uploaded_color_atlas = true;
            }
        }
        let frame = &mut self.frames[frame_index];
        unsafe { self.device.reset_fences(&[frame.fence]) }
            .map_err(vk_error("reset frame fence"))?;
        for row in 0..height {
            if frame.dirty_rows[row] {
                let start = row * width;
                let row_bytes = bytemuck::cast_slice(&self.gpu_cells[start..start + width]);
                // A frame fence has retired this mapped SSBO before the CPU copies its changed row.
                let destination = unsafe { frame.cells.mapped.add(start * size_of::<GpuCell>()) };
                unsafe {
                    ptr::copy_nonoverlapping(row_bytes.as_ptr(), destination, row_bytes.len())
                };
                frame.dirty_rows[row] = false;
            }
        }
        if frame.glyphs_dirty {
            let glyph_bytes = bytemuck::cast_slice(&self.gpu_glyphs);
            if glyph_bytes.len() as u64 > frame.glyphs.size {
                return Err("font glyph table exceeds allocated GPU buffer".into());
            }
            // Glyph metadata only changes when the CPU atlas allocates a new glyph.
            unsafe {
                ptr::copy_nonoverlapping(
                    glyph_bytes.as_ptr(),
                    frame.glyphs.mapped,
                    glyph_bytes.len(),
                )
            };
            frame.glyphs_dirty = false;
        }
        let push = PushGrid {
            cols: width as u32,
            rows: height as u32,
            cell_width: font.metrics().width as f32,
            cell_height: font.metrics().height as f32,
            ascent: font.metrics().ascent as f32,
            cursor: (terminal.cursor().row * width + terminal.cursor().column) as u32,
            cursor_visible: u32::from(
                terminal.modes().cursor_visible
                    && (!terminal.modes().cursor_blink || cursor_blink_on)
                    && terminal.viewport_offset() == 0,
            ),
            selection_start: terminal
                .selection_cell_range()
                .map_or(u32::MAX, |range| range.0),
            selection_end: terminal
                .selection_cell_range()
                .map_or(u32::MAX, |range| range.1),
            cursor_style: push_cursor_style(terminal.modes().cursor_style),
        };
        record_frame(
            &self.device,
            frame,
            &FrameDraw {
                image: self.swapchain_images[image_index as usize],
                view: self.image_views[image_index as usize],
                background: self.background_pipeline,
                text: self.text_pipeline,
                layout: self.pipeline_layout,
                push,
                instances: count as u32,
                extent: self.extent,
                final_layout: vk::ImageLayout::PRESENT_SRC_KHR,
                readback: None,
            },
        )?;
        let wait = [vk::SemaphoreSubmitInfo::default()
            .semaphore(frame.image_available)
            .stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)];
        let commands = [vk::CommandBufferSubmitInfo::default().command_buffer(frame.command)];
        let present_semaphore = self.present_ready[image_index as usize];
        let signal = [vk::SemaphoreSubmitInfo::default()
            .semaphore(present_semaphore)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        let submit = [vk::SubmitInfo2::default()
            .wait_semaphore_infos(&wait)
            .command_buffer_infos(&commands)
            .signal_semaphore_infos(&signal)];
        unsafe { self.device.queue_submit2(self.queue, &submit, frame.fence) }
            .map_err(vk_error("submit frame"))?;
        let swaps = [self.swapchain];
        let indices = [image_index];
        let present = vk::PresentInfoKHR::default()
            .wait_semaphores(std::slice::from_ref(&present_semaphore))
            .swapchains(&swaps)
            .image_indices(&indices);
        let present_status = unsafe { self.swapchain_loader.queue_present(self.queue, &present) };
        self.frame_index = (self.frame_index + 1) % FRAMES;
        match present_status {
            Ok(present_suboptimal) if !suboptimal && !present_suboptimal => {}
            Ok(_) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR | vk::Result::SUBOPTIMAL_KHR) => {
                self.resize(self.extent.width, self.extent.height)?;
            }
            Err(error) => return Err(format!("present frame: {error:?}")),
        }
        Ok(())
    }

    /// Replaces swapchain images only after submitted work has released their views.
    pub fn invalidate_atlas(&mut self) {
        self.uploaded_atlas = false;
        self.uploaded_color_atlas = false;
        self.pending_glyph_uploads.clear();
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), String> {
        self.invalidate_atlas();
        unsafe { self.device.device_wait_idle() }
            .map_err(vk_error("wait before swapchain resize"))?;
        let context = SurfaceContext {
            device: &self.device,
            surface_loader: &self.surface_loader,
            swapchain_loader: &self.swapchain_loader,
            physical: self.physical,
            surface: self.surface,
            queue_family: self.queue_family,
        };
        let swap = create_swapchain(&context, width, height)?;
        let present_ready = create_present_semaphores(&self.device, swap.images.len())?;
        let replacement_pipelines = if swap.format != self.format {
            Some((
                create_pipeline(
                    &self.device,
                    self.pipeline_layout,
                    swap.format,
                    include_bytes!(concat!(env!("OUT_DIR"), "/background.vert.spv")),
                    include_bytes!(concat!(env!("OUT_DIR"), "/background.frag.spv")),
                    false,
                )?,
                create_pipeline(
                    &self.device,
                    self.pipeline_layout,
                    swap.format,
                    include_bytes!(concat!(env!("OUT_DIR"), "/text.vert.spv")),
                    include_bytes!(concat!(env!("OUT_DIR"), "/text.frag.spv")),
                    true,
                )?,
            ))
        } else {
            None
        };
        unsafe {
            for semaphore in self.present_ready.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
            }
            for view in self.image_views.drain(..) {
                self.device.destroy_image_view(view, None);
            }
            self.swapchain_loader
                .destroy_swapchain(self.swapchain, None);
            if let Some((background, text)) = replacement_pipelines {
                self.device.destroy_pipeline(self.background_pipeline, None);
                self.device.destroy_pipeline(self.text_pipeline, None);
                self.background_pipeline = background;
                self.text_pipeline = text;
            }
        }
        self.swapchain = swap.handle;
        self.swapchain_images = swap.images;
        self.image_views = swap.views;
        self.present_ready = present_ready;
        self.format = swap.format;
        self.extent = swap.extent;
        Ok(())
    }

    fn convert_cell(
        &mut self,
        cell: Cell,
        grapheme: Option<&str>,
        pre_context: &str,
        post_context: &str,
        font: &mut Font,
    ) -> Result<GpuCell, String> {
        if cell.content == 0 || cell.flags().contains(CellFlags::WIDE_SPACER) {
            return Ok(GpuCell {
                glyph: u32::MAX,
                fg: color(cell.fg, true),
                bg: color(cell.bg, false),
                flags: cell.flags,
            });
        }
        let ch = char::from_u32(cell.content).unwrap_or('\u{fffd}');
        let bold = cell.flags().contains(CellFlags::BOLD);
        let italic = cell.flags().contains(CellFlags::ITALIC);
        let (glyph, info, upload) = if let Some(grapheme) = grapheme {
            if pre_context.is_empty() && post_context.is_empty() {
                font.glyph_text(grapheme, bold, italic)?
            } else {
                font.glyph_text_with_context(grapheme, pre_context, post_context, bold, italic)?
            }
        } else if !pre_context.is_empty() || !post_context.is_empty() {
            let text = ch.to_string();
            font.glyph_text_with_context(&text, pre_context, post_context, bold, italic)?
        } else {
            font.glyph(ch, bold, italic)?
        };
        if let Some(upload) = upload {
            self.pending_glyph_uploads.push(upload);
        }
        let old_length = self.gpu_glyphs.len();
        // Grow the glyph table without truncating entries recorded for other
        // cells; `resize` would shrink it on cache hits with smaller indices.
        if self.gpu_glyphs.len() <= glyph as usize {
            self.gpu_glyphs
                .resize(glyph as usize + 1, GpuGlyph::zeroed());
        }
        let metadata = GpuGlyph {
            uv: [
                info.uv_min[0],
                info.uv_min[1],
                info.uv_max[0],
                info.uv_max[1],
            ],
            size_bearing: [info.size[0], info.size[1], info.bearing[0], info.bearing[1]],
            // The padding lane carries the color-atlas selector the text
            // shader reads; the std430 layout is unchanged.
            bearing_advance: [
                info.bearing[0],
                info.bearing[1],
                info.advance,
                u32::from(info.color) as f32,
            ],
        };
        if glyph as usize >= old_length || self.gpu_glyphs[glyph as usize] != metadata {
            self.glyphs_dirty = true;
        }
        self.gpu_glyphs[glyph as usize] = metadata;
        Ok(GpuCell {
            glyph,
            fg: color(cell.fg, true),
            bg: color(cell.bg, false),
            flags: cell.flags,
        })
    }

    /// Uploads one atlas image: either a full replacement or incremental
    /// glyph regions. `color` selects RGBA8 validation for the color atlas;
    /// monochrome uploads use one byte per pixel.
    fn upload_atlas(
        &self,
        image: vk::Image,
        initialized: bool,
        full_atlas: Option<&[u8]>,
        uploads: &[GlyphUpload],
        color: bool,
    ) -> Result<(), String> {
        let mut staging_data = Vec::new();
        let mut regions = Vec::new();
        if let Some(pixels) = full_atlas {
            staging_data.extend_from_slice(pixels);
            regions.push(
                vk::BufferImageCopy::default()
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .layer_count(1),
                    )
                    .image_extent(vk::Extent3D {
                        width: ATLAS_SIZE,
                        height: ATLAS_SIZE,
                        depth: 1,
                    }),
            );
        } else {
            let bytes_per_pixel = if color { 4 } else { 1 };
            for upload in uploads {
                let expected = (upload.width as usize)
                    .checked_mul(upload.height as usize)
                    .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
                    .ok_or("glyph upload dimensions overflow")?;
                if upload.color != color
                    || upload.pixels.len() != expected
                    || upload
                        .x
                        .checked_add(upload.width)
                        .is_none_or(|right| right > ATLAS_SIZE)
                    || upload
                        .y
                        .checked_add(upload.height)
                        .is_none_or(|bottom| bottom > ATLAS_SIZE)
                {
                    return Err("font returned an invalid glyph atlas upload".into());
                }
                let aligned = staging_data
                    .len()
                    .checked_add(3)
                    .ok_or("glyph staging offset overflow")?
                    & !3;
                staging_data.resize(aligned, 0);
                let offset = staging_data.len() as u64;
                staging_data.extend_from_slice(&upload.pixels);
                regions.push(
                    vk::BufferImageCopy::default()
                        .buffer_offset(offset)
                        .image_subresource(
                            vk::ImageSubresourceLayers::default()
                                .aspect_mask(vk::ImageAspectFlags::COLOR)
                                .layer_count(1),
                        )
                        .image_offset(vk::Offset3D {
                            x: upload.x as i32,
                            y: upload.y as i32,
                            z: 0,
                        })
                        .image_extent(vk::Extent3D {
                            width: upload.width,
                            height: upload.height,
                            depth: 1,
                        }),
                );
            }
        }
        let staging = create_buffer(
            &self.instance,
            &self.device,
            self.physical,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            staging_data.len().max(1) as u64,
            vk::BufferUsageFlags::TRANSFER_SRC,
        )?;
        if staging_data.len() as u64 > staging.size {
            return Err("font atlas exceeds Vulkan image size".into());
        }
        unsafe {
            ptr::copy_nonoverlapping(staging_data.as_ptr(), staging.mapped, staging_data.len())
        };
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(self.queue_family)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT);
        let pool = unsafe { self.device.create_command_pool(&pool_info, None) }
            .map_err(vk_error("atlas upload command pool"))?;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command = unsafe { self.device.allocate_command_buffers(&alloc) }
            .map_err(vk_error("atlas upload command buffer"))?[0];
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe { self.device.begin_command_buffer(command, &begin) }
            .map_err(vk_error("begin atlas upload"))?;
        let to_transfer = [vk::ImageMemoryBarrier2::default()
            .src_stage_mask(if initialized {
                vk::PipelineStageFlags2::FRAGMENT_SHADER
            } else {
                vk::PipelineStageFlags2::NONE
            })
            .src_access_mask(if initialized {
                vk::AccessFlags2::SHADER_SAMPLED_READ
            } else {
                vk::AccessFlags2::NONE
            })
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .old_layout(if initialized {
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
            } else {
                vk::ImageLayout::UNDEFINED
            })
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .image(image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            )];
        let dep = vk::DependencyInfo::default().image_memory_barriers(&to_transfer);
        unsafe { self.device.cmd_pipeline_barrier2(command, &dep) };
        unsafe {
            self.device.cmd_copy_buffer_to_image(
                command,
                staging.handle,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &regions,
            )
        };
        let to_sample = [vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
            .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .image(image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            )];
        let dep = vk::DependencyInfo::default().image_memory_barriers(&to_sample);
        unsafe {
            self.device.cmd_pipeline_barrier2(command, &dep);
            self.device.end_command_buffer(command)
        }
        .map_err(vk_error("finish atlas upload"))?;
        let cmds = [vk::CommandBufferSubmitInfo::default().command_buffer(command)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmds)];
        unsafe {
            self.device
                .queue_submit2(self.queue, &submit, vk::Fence::null())
                .and_then(|_| self.device.queue_wait_idle(self.queue))
        }
        .map_err(vk_error("upload glyph atlas"))?;
        unsafe { self.device.destroy_command_pool(pool, None) };
        destroy_buffer(&self.device, staging);
        Ok(())
    }
}

impl Drop for OffscreenRenderer {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            for frame in self.frames.drain(..) {
                destroy_buffer(&self.device, frame.cells);
                destroy_buffer(&self.device, frame.glyphs);
                self.device.destroy_fence(frame.fence, None);
                self.device.destroy_semaphore(frame.image_available, None);
                self.device.destroy_command_pool(frame.command_pool, None);
            }
            self.device
                .destroy_descriptor_pool(self.descriptor_pool, None);
            self.device.destroy_pipeline(self.text_pipeline, None);
            self.device.destroy_pipeline(self.background_pipeline, None);
            self.device
                .destroy_pipeline_layout(self.pipeline_layout, None);
            self.device
                .destroy_descriptor_set_layout(self.descriptor_layout, None);
            self.device.destroy_sampler(self.atlas_sampler, None);
            self.device.destroy_image_view(self.atlas_view, None);
            self.device.destroy_image(self.atlas, None);
            self.device.free_memory(self.atlas_memory, None);
            self.device.destroy_sampler(self.color_atlas_sampler, None);
            self.device.destroy_image_view(self.color_atlas_view, None);
            self.device.destroy_image(self.color_atlas, None);
            self.device.free_memory(self.color_atlas_memory, None);
            self.device.destroy_image_view(self.target_view, None);
            self.device.destroy_image(self.target, None);
            self.device.free_memory(self.target_memory, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

fn choose_offscreen_device(instance: &Instance) -> Result<(vk::PhysicalDevice, u32), String> {
    for physical in unsafe { instance.enumerate_physical_devices() }
        .map_err(vk_error("enumerate offscreen Vulkan devices"))?
    {
        let properties = unsafe { instance.get_physical_device_properties(physical) };
        if properties.api_version < vk::API_VERSION_1_3 {
            continue;
        }
        let mut features13 = vk::PhysicalDeviceVulkan13Features::default();
        let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut features13);
        unsafe { instance.get_physical_device_features2(physical, &mut features) };
        if features13.dynamic_rendering != vk::TRUE || features13.synchronization2 != vk::TRUE {
            continue;
        }
        if let Some((index, _)) =
            unsafe { instance.get_physical_device_queue_family_properties(physical) }
                .iter()
                .enumerate()
                .find(|(_, family)| family.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        {
            return Ok((physical, index as u32));
        }
    }
    Err("no Vulkan 1.3 device has dynamic rendering, synchronization2, and a graphics queue".into())
}

fn create_offscreen_target(
    instance: &Instance,
    device: &Device,
    physical: vk::PhysicalDevice,
    width: u32,
    height: u32,
) -> Result<(vk::Image, vk::DeviceMemory, vk::ImageView), String> {
    let info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let image = unsafe { device.create_image(&info, None) }
        .map_err(vk_error("create offscreen color image"))?;
    let requirements = unsafe { device.get_image_memory_requirements(image) };
    let memory_index = memory_type(
        instance,
        physical,
        requirements.memory_type_bits,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    )?;
    let allocation = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_index);
    let memory = unsafe { device.allocate_memory(&allocation, None) }
        .map_err(vk_error("allocate offscreen color image"))?;
    unsafe { device.bind_image_memory(image, memory, 0) }
        .map_err(vk_error("bind offscreen color image"))?;
    let view_info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .subresource_range(
            vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1),
        );
    let view = unsafe { device.create_image_view(&view_info, None) }
        .map_err(vk_error("create offscreen color view"))?;
    Ok((image, memory, view))
}

fn upload_offscreen_atlas(
    context: AtlasUploadContext<'_>,
    pixels: &[u8],
    color: bool,
) -> Result<(), String> {
    let bytes_per_pixel = if color { 4 } else { 1 };
    let expected = (ATLAS_SIZE * ATLAS_SIZE) as usize * bytes_per_pixel;
    if pixels.len() != expected {
        return Err("font atlas byte count does not match the Vulkan atlas extent".into());
    }
    let upload = GlyphUpload {
        x: 0,
        y: 0,
        width: ATLAS_SIZE,
        height: ATLAS_SIZE,
        pixels: pixels.to_vec(),
        color,
    };
    upload_atlas_regions(context, &[upload], false)
}

fn upload_atlas_regions(
    context: AtlasUploadContext<'_>,
    uploads: &[GlyphUpload],
    initialized: bool,
) -> Result<(), String> {
    let AtlasUploadContext {
        instance,
        device,
        physical,
        queue,
        queue_family,
        atlas,
    } = context;
    let mut data = Vec::new();
    let mut regions = Vec::with_capacity(uploads.len());
    for upload in uploads {
        let bytes_per_pixel = if upload.color { 4 } else { 1 };
        let expected = (upload.width as usize)
            .checked_mul(upload.height as usize)
            .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
            .ok_or("atlas upload dimensions overflow")?;
        if upload.width == 0
            || upload.height == 0
            || upload.pixels.len() != expected
            || upload
                .x
                .checked_add(upload.width)
                .is_none_or(|right| right > ATLAS_SIZE)
            || upload
                .y
                .checked_add(upload.height)
                .is_none_or(|bottom| bottom > ATLAS_SIZE)
        {
            return Err("glyph upload is outside the glyph atlas".into());
        }
        let offset = data.len().checked_add(3).ok_or("atlas offset overflow")? & !3;
        data.resize(offset, 0);
        data.extend_from_slice(&upload.pixels);
        regions.push(
            vk::BufferImageCopy::default()
                .buffer_offset(offset as u64)
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .layer_count(1),
                )
                .image_offset(vk::Offset3D {
                    x: upload.x as i32,
                    y: upload.y as i32,
                    z: 0,
                })
                .image_extent(vk::Extent3D {
                    width: upload.width,
                    height: upload.height,
                    depth: 1,
                }),
        );
    }
    let staging = create_buffer(
        instance,
        device,
        physical,
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        data.len().max(1) as u64,
        vk::BufferUsageFlags::TRANSFER_SRC,
    )?;
    unsafe { ptr::copy_nonoverlapping(data.as_ptr(), staging.mapped, data.len()) };
    let pool = unsafe {
        device.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(queue_family)
                .flags(vk::CommandPoolCreateFlags::TRANSIENT),
            None,
        )
    }
    .map_err(vk_error("create atlas upload command pool"))?;
    let command = unsafe {
        device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )
    }
    .map_err(vk_error("allocate atlas upload command buffer"))?[0];
    unsafe {
        device.begin_command_buffer(
            command,
            &vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )
    }
    .map_err(vk_error("begin atlas upload command buffer"))?;
    let to_copy = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(if initialized {
            vk::PipelineStageFlags2::FRAGMENT_SHADER
        } else {
            vk::PipelineStageFlags2::NONE
        })
        .src_access_mask(if initialized {
            vk::AccessFlags2::SHADER_SAMPLED_READ
        } else {
            vk::AccessFlags2::NONE
        })
        .dst_stage_mask(vk::PipelineStageFlags2::COPY)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .old_layout(if initialized {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        } else {
            vk::ImageLayout::UNDEFINED
        })
        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .image(atlas)
        .subresource_range(
            vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1),
        )];
    unsafe {
        device.cmd_pipeline_barrier2(
            command,
            &vk::DependencyInfo::default().image_memory_barriers(&to_copy),
        );
        device.cmd_copy_buffer_to_image(
            command,
            staging.handle,
            atlas,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &regions,
        );
    }
    let to_sample = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COPY)
        .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
        .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .image(atlas)
        .subresource_range(
            vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1),
        )];
    unsafe {
        device.cmd_pipeline_barrier2(
            command,
            &vk::DependencyInfo::default().image_memory_barriers(&to_sample),
        );
        device.end_command_buffer(command)
    }
    .map_err(vk_error("finish atlas upload command buffer"))?;
    let commands = [vk::CommandBufferSubmitInfo::default().command_buffer(command)];
    let submits = [vk::SubmitInfo2::default().command_buffer_infos(&commands)];
    unsafe {
        device
            .queue_submit2(queue, &submits, vk::Fence::null())
            .and_then(|_| device.queue_wait_idle(queue))
    }
    .map_err(vk_error("submit atlas upload"))?;
    unsafe { device.destroy_command_pool(pool, None) };
    destroy_buffer(device, staging);
    Ok(())
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            for frame in self.frames.drain(..) {
                destroy_buffer(&self.device, frame.cells);
                destroy_buffer(&self.device, frame.glyphs);
                self.device.destroy_fence(frame.fence, None);
                self.device.destroy_semaphore(frame.image_available, None);
                self.device.destroy_command_pool(frame.command_pool, None);
            }
            for semaphore in self.present_ready.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
            }
            self.device
                .destroy_descriptor_pool(self.descriptor_pool, None);
            self.device.destroy_pipeline(self.text_pipeline, None);
            self.device.destroy_pipeline(self.background_pipeline, None);
            self.device
                .destroy_pipeline_layout(self.pipeline_layout, None);
            self.device
                .destroy_descriptor_set_layout(self.descriptor_layout, None);
            self.device.destroy_sampler(self.atlas_sampler, None);
            self.device.destroy_image_view(self.atlas_view, None);
            self.device.destroy_image(self.atlas, None);
            self.device.free_memory(self.atlas_memory, None);
            self.device.destroy_sampler(self.color_atlas_sampler, None);
            self.device.destroy_image_view(self.color_atlas_view, None);
            self.device.destroy_image(self.color_atlas, None);
            self.device.free_memory(self.color_atlas_memory, None);
            for view in self.image_views.drain(..) {
                self.device.destroy_image_view(view, None);
            }
            self.swapchain_loader
                .destroy_swapchain(self.swapchain, None);
            self.device.destroy_device(None);
            self.surface_loader.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}

fn choose_device(
    instance: &Instance,
    surface: &ash::khr::surface::Instance,
    handle: vk::SurfaceKHR,
) -> Result<(vk::PhysicalDevice, u32), String> {
    for physical in unsafe { instance.enumerate_physical_devices() }
        .map_err(vk_error("enumerate Vulkan devices"))?
    {
        let props = unsafe { instance.get_physical_device_properties(physical) };
        if props.api_version < vk::API_VERSION_1_3 {
            continue;
        }
        let mut features13 = vk::PhysicalDeviceVulkan13Features::default();
        let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut features13);
        unsafe { instance.get_physical_device_features2(physical, &mut features) };
        if features13.dynamic_rendering != vk::TRUE || features13.synchronization2 != vk::TRUE {
            continue;
        }
        let extensions = unsafe { instance.enumerate_device_extension_properties(physical) }
            .map_err(vk_error("enumerate device extensions"))?;
        if !extensions.iter().any(|ext| unsafe { CStr::from_ptr(ext.extension_name.as_ptr()) } == ash::khr::swapchain::NAME) { continue; }
        for (index, family) in
            unsafe { instance.get_physical_device_queue_family_properties(physical) }
                .iter()
                .enumerate()
        {
            if !family.queue_flags.contains(vk::QueueFlags::GRAPHICS) {
                continue;
            }
            let present = unsafe {
                surface.get_physical_device_surface_support(physical, index as u32, handle)
            }
            .map_err(vk_error("query presentation support"))?;
            if present {
                return Ok((physical, index as u32));
            }
        }
    }
    Err("no Vulkan 1.3 physical device supports graphics and XCB presentation".into())
}

fn create_swapchain(
    context: &SurfaceContext<'_>,
    width: u32,
    height: u32,
) -> Result<SwapchainState, String> {
    let SurfaceContext {
        device,
        surface_loader,
        swapchain_loader: loader,
        physical,
        surface,
        queue_family: family,
    } = context;
    let caps =
        unsafe { surface_loader.get_physical_device_surface_capabilities(*physical, *surface) }
            .map_err(vk_error("query surface capabilities"))?;
    let formats =
        unsafe { surface_loader.get_physical_device_surface_formats(*physical, *surface) }
            .map_err(vk_error("query surface formats"))?;
    let selected_format = formats
        .iter()
        .find(|item| item.format == vk::Format::B8G8R8A8_UNORM)
        .or(formats.first())
        .ok_or("XCB surface has no supported image format")?;
    let format = selected_format.format;
    let extent = if caps.current_extent.width != u32::MAX {
        caps.current_extent
    } else {
        vk::Extent2D {
            width: width.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
            height: height.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
        }
    };
    let count = (caps.min_image_count + 1).min(if caps.max_image_count == 0 {
        u32::MAX
    } else {
        caps.max_image_count
    });
    let families = [*family];
    let info = vk::SwapchainCreateInfoKHR::default()
        .surface(*surface)
        .min_image_count(count)
        .image_format(format)
        .image_color_space(selected_format.color_space)
        .image_extent(extent)
        .image_array_layers(1)
        .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
        .queue_family_indices(&families)
        .pre_transform(caps.current_transform)
        .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
        .present_mode(vk::PresentModeKHR::FIFO)
        .clipped(true);
    let swapchain =
        unsafe { loader.create_swapchain(&info, None) }.map_err(vk_error("create swapchain"))?;
    let images = unsafe { loader.get_swapchain_images(swapchain) }
        .map_err(vk_error("get swapchain images"))?;
    let mut views = Vec::new();
    for image in &images {
        let view_info = vk::ImageViewCreateInfo::default()
            .image(*image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            );
        views.push(
            unsafe { device.create_image_view(&view_info, None) }
                .map_err(vk_error("create swapchain image view"))?,
        );
    }
    Ok(SwapchainState {
        handle: swapchain,
        images,
        views,
        format,
        extent,
    })
}

fn create_descriptor_layout(device: &Device) -> Result<vk::DescriptorSetLayout, String> {
    let bindings = [
        vk::DescriptorSetLayoutBinding::default()
            .binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT),
        vk::DescriptorSetLayoutBinding::default()
            .binding(2)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX),
        vk::DescriptorSetLayoutBinding::default()
            .binding(3)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT),
        vk::DescriptorSetLayoutBinding::default()
            .binding(4)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    unsafe { device.create_descriptor_set_layout(&info, None) }
        .map_err(vk_error("create descriptor layout"))
}

fn create_pipeline(
    device: &Device,
    layout: vk::PipelineLayout,
    format: vk::Format,
    vert_bytes: &[u8],
    frag_bytes: &[u8],
    blend: bool,
) -> Result<vk::Pipeline, String> {
    let vert_words = ash::util::read_spv(&mut std::io::Cursor::new(vert_bytes))
        .map_err(|error| format!("read vertex shader: {error}"))?;
    let frag_words = ash::util::read_spv(&mut std::io::Cursor::new(frag_bytes))
        .map_err(|error| format!("read fragment shader: {error}"))?;
    let vert = unsafe {
        device.create_shader_module(
            &vk::ShaderModuleCreateInfo::default().code(&vert_words),
            None,
        )
    }
    .map_err(vk_error("create vertex shader"))?;
    let frag = unsafe {
        device.create_shader_module(
            &vk::ShaderModuleCreateInfo::default().code(&frag_words),
            None,
        )
    }
    .map_err(vk_error("create fragment shader"))?;
    let entry = c"main";
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vert)
            .name(entry),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(frag)
            .name(entry),
    ];
    let vertex = vk::PipelineVertexInputStateCreateInfo::default();
    let assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(if blend {
        vk::PrimitiveTopology::TRIANGLE_STRIP
    } else {
        vk::PrimitiveTopology::TRIANGLE_LIST
    });
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);
    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    // Palette channels stay untransformed; the selected swapchain format owns output conversion.
    let blend_attachment = [vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(blend)
        .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
        .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ONE)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .alpha_blend_op(vk::BlendOp::ADD)
        .color_write_mask(vk::ColorComponentFlags::RGBA)];
    let blend_state =
        vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachment);
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
    let rendering = [vk::Format::from_raw(format.as_raw())];
    let mut render_info =
        vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&rendering);
    let info = [vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex)
        .input_assembly_state(&assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .color_blend_state(&blend_state)
        .dynamic_state(&dynamic)
        .layout(layout)
        .push_next(&mut render_info)];
    let result =
        unsafe { device.create_graphics_pipelines(vk::PipelineCache::null(), &info, None) }
            .map(|pipelines| pipelines[0])
            .map_err(|(_, error)| format!("create graphics pipeline: {error:?}"));
    unsafe {
        device.destroy_shader_module(vert, None);
        device.destroy_shader_module(frag, None);
    }
    result
}

fn create_descriptor_pool(device: &Device) -> Result<vk::DescriptorPool, String> {
    let sizes = [
        vk::DescriptorPoolSize {
            ty: vk::DescriptorType::STORAGE_BUFFER,
            descriptor_count: (FRAMES * 2) as u32,
        },
        vk::DescriptorPoolSize {
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: (FRAMES * 2) as u32,
        },
    ];
    let info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(FRAMES as u32)
        .pool_sizes(&sizes);
    unsafe { device.create_descriptor_pool(&info, None) }
        .map_err(vk_error("create descriptor pool"))
}

fn create_present_semaphores(
    device: &Device,
    image_count: usize,
) -> Result<Vec<vk::Semaphore>, String> {
    let info = vk::SemaphoreCreateInfo::default();
    (0..image_count)
        .map(|_| {
            unsafe { device.create_semaphore(&info, None) }
                .map_err(vk_error("create swapchain presentation semaphore"))
        })
        .collect()
}

fn create_frames(setup: FrameSetup<'_>) -> Result<Vec<Frame>, String> {
    let FrameSetup {
        instance,
        device,
        physical,
        queue_family,
        pool,
        layout,
        atlas_view,
        sampler,
        color_atlas_view,
        color_sampler,
    } = setup;
    let layouts = [layout; FRAMES];
    let allocation = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&layouts);
    let sets = unsafe { device.allocate_descriptor_sets(&allocation) }
        .map_err(vk_error("allocate frame descriptors"))?;
    let mut frames = Vec::new();
    for set in sets {
        let command_pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(queue_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let command_pool = unsafe { device.create_command_pool(&command_pool_info, None) }
            .map_err(vk_error("create frame command pool"))?;
        let command_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command = unsafe { device.allocate_command_buffers(&command_info) }
            .map_err(vk_error("allocate frame command buffer"))?[0];
        let semaphore_info = vk::SemaphoreCreateInfo::default();
        let image_available = unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(vk_error("create acquire semaphore"))?;
        let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
        let fence = unsafe { device.create_fence(&fence_info, None) }
            .map_err(vk_error("create frame fence"))?;
        let cells = create_buffer(
            instance,
            device,
            physical,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            (MAX_COLS * MAX_ROWS * size_of::<GpuCell>()) as u64,
            vk::BufferUsageFlags::STORAGE_BUFFER,
        )?;
        let glyphs = create_buffer(
            instance,
            device,
            physical,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            65536 * size_of::<GpuGlyph>() as u64,
            vk::BufferUsageFlags::STORAGE_BUFFER,
        )?;
        let cell_info = [vk::DescriptorBufferInfo::default()
            .buffer(cells.handle)
            .offset(0)
            .range(cells.size)];
        let glyph_info = [vk::DescriptorBufferInfo::default()
            .buffer(glyphs.handle)
            .offset(0)
            .range(glyphs.size)];
        let image_info = [vk::DescriptorImageInfo::default()
            .sampler(sampler)
            .image_view(atlas_view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let color_image_info = [vk::DescriptorImageInfo::default()
            .sampler(color_sampler)
            .image_view(color_atlas_view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&cell_info),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&glyph_info),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(3)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&image_info),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(4)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&color_image_info),
        ];
        unsafe { device.update_descriptor_sets(&writes, &[]) };
        frames.push(Frame {
            command_pool,
            command,
            image_available,
            fence,
            cells,
            glyphs,
            descriptors: set,
            dirty_rows: vec![true; MAX_ROWS],
            glyphs_dirty: true,
        });
    }
    Ok(frames)
}

fn create_atlas(
    instance: &Instance,
    device: &Device,
    physical: vk::PhysicalDevice,
    format: vk::Format,
) -> Result<(vk::Image, vk::DeviceMemory, vk::ImageView, vk::Sampler), String> {
    let info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width: ATLAS_SIZE,
            height: ATLAS_SIZE,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let image = unsafe { device.create_image(&info, None) }
        .map_err(vk_error("create glyph atlas image"))?;
    let requirements = unsafe { device.get_image_memory_requirements(image) };
    let index = memory_type(
        instance,
        physical,
        requirements.memory_type_bits,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    )?;
    let allocation = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(index);
    let memory = unsafe { device.allocate_memory(&allocation, None) }
        .map_err(vk_error("allocate glyph atlas memory"))?;
    unsafe { device.bind_image_memory(image, memory, 0) }
        .map_err(vk_error("bind glyph atlas memory"))?;
    let view_info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(format)
        .subresource_range(
            vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1),
        );
    let view = unsafe { device.create_image_view(&view_info, None) }
        .map_err(vk_error("create atlas view"))?;
    let sampler_info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .max_lod(1.0);
    let sampler = unsafe { device.create_sampler(&sampler_info, None) }
        .map_err(vk_error("create atlas sampler"))?;
    Ok((image, memory, view, sampler))
}

fn create_buffer(
    instance: &Instance,
    device: &Device,
    physical: vk::PhysicalDevice,
    memory_flags: vk::MemoryPropertyFlags,
    size: u64,
    usage: vk::BufferUsageFlags,
) -> Result<Buffer, String> {
    let info = vk::BufferCreateInfo::default()
        .size(size)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let handle =
        unsafe { device.create_buffer(&info, None) }.map_err(vk_error("create Vulkan buffer"))?;
    let requirements = unsafe { device.get_buffer_memory_requirements(handle) };
    let index = match memory_type(
        instance,
        physical,
        requirements.memory_type_bits,
        memory_flags,
    ) {
        Ok(index) => index,
        Err(error) => {
            unsafe { device.destroy_buffer(handle, None) };
            return Err(error);
        }
    };
    let allocation = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(index);
    let memory = match unsafe { device.allocate_memory(&allocation, None) } {
        Ok(memory) => memory,
        Err(error) => {
            unsafe { device.destroy_buffer(handle, None) };
            return Err(format!("allocate Vulkan buffer memory: {error:?}"));
        }
    };
    if let Err(error) = unsafe { device.bind_buffer_memory(handle, memory, 0) } {
        unsafe {
            device.free_memory(memory, None);
            device.destroy_buffer(handle, None);
        }
        return Err(format!("bind Vulkan buffer memory: {error:?}"));
    }
    let mapped = match unsafe { device.map_memory(memory, 0, size, vk::MemoryMapFlags::empty()) } {
        Ok(mapped) => mapped.cast(),
        Err(error) => {
            unsafe {
                device.free_memory(memory, None);
                device.destroy_buffer(handle, None);
            }
            return Err(format!("map Vulkan buffer memory: {error:?}"));
        }
    };
    Ok(Buffer {
        handle,
        memory,
        mapped,
        size,
    })
}

fn memory_type(
    instance: &Instance,
    physical: vk::PhysicalDevice,
    bits: u32,
    flags: vk::MemoryPropertyFlags,
) -> Result<u32, String> {
    let properties = unsafe { instance.get_physical_device_memory_properties(physical) };
    (0..properties.memory_type_count)
        .find(|index| {
            bits & (1 << index) != 0
                && properties.memory_types[*index as usize]
                    .property_flags
                    .contains(flags)
        })
        .ok_or_else(|| format!("no Vulkan memory type supports {flags:?}"))
}
fn destroy_buffer(device: &Device, buffer: Buffer) {
    unsafe {
        device.unmap_memory(buffer.memory);
        device.destroy_buffer(buffer.handle, None);
        device.free_memory(buffer.memory, None);
    }
}

fn record_frame(device: &Device, frame: &Frame, draw: &FrameDraw) -> Result<(), String> {
    let FrameDraw {
        image,
        view,
        background,
        text,
        layout,
        push,
        instances,
        extent,
        final_layout,
        readback,
    } = draw;
    unsafe { device.reset_command_buffer(frame.command, vk::CommandBufferResetFlags::empty()) }
        .map_err(vk_error("reset frame command buffer"))?;
    let begin = vk::CommandBufferBeginInfo::default();
    unsafe { device.begin_command_buffer(frame.command, &begin) }
        .map_err(vk_error("begin frame command buffer"))?;
    let range = vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1);
    let to_color = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::NONE)
        .dst_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
        .dst_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
        .image(*image)
        .subresource_range(range)];
    let dep = vk::DependencyInfo::default().image_memory_barriers(&to_color);
    unsafe { device.cmd_pipeline_barrier2(frame.command, &dep) };
    let color = [vk::RenderingAttachmentInfo::default()
        .image_view(*view)
        .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .clear_value(vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.09, 0.09, 0.09, 1.0],
            },
        })];
    let rendering = vk::RenderingInfo::default()
        .render_area(vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: *extent,
        })
        .layer_count(1)
        .color_attachments(&color);
    unsafe {
        device.cmd_begin_rendering(frame.command, &rendering);
        device.cmd_set_viewport(
            frame.command,
            0,
            &[vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: extent.width as f32,
                height: extent.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            }],
        );
        device.cmd_set_scissor(
            frame.command,
            0,
            &[vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: *extent,
            }],
        );
        device.cmd_bind_descriptor_sets(
            frame.command,
            vk::PipelineBindPoint::GRAPHICS,
            *layout,
            0,
            &[frame.descriptors],
            &[],
        );
        device.cmd_push_constants(
            frame.command,
            *layout,
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
            0,
            bytemuck::bytes_of(push),
        );
        device.cmd_bind_pipeline(frame.command, vk::PipelineBindPoint::GRAPHICS, *background);
        device.cmd_draw(frame.command, 3, 1, 0, 0);
        device.cmd_bind_pipeline(frame.command, vk::PipelineBindPoint::GRAPHICS, *text);
        device.cmd_draw(frame.command, 4, *instances, 0, 0);
        device.cmd_end_rendering(frame.command);
        let to_final = [vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
            .src_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
            .dst_stage_mask(if readback.is_some() {
                vk::PipelineStageFlags2::COPY
            } else {
                vk::PipelineStageFlags2::NONE
            })
            .dst_access_mask(if readback.is_some() {
                vk::AccessFlags2::TRANSFER_READ
            } else {
                vk::AccessFlags2::NONE
            })
            .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .new_layout(*final_layout)
            .image(*image)
            .subresource_range(range)];
        let dep = vk::DependencyInfo::default().image_memory_barriers(&to_final);
        device.cmd_pipeline_barrier2(frame.command, &dep);
        if let Some(buffer) = readback {
            device.cmd_copy_image_to_buffer(
                frame.command,
                *image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                *buffer,
                &[vk::BufferImageCopy::default()
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .layer_count(1),
                    )
                    .image_extent(vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    })],
            );
            let host_ready = [vk::BufferMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COPY)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                .dst_access_mask(vk::AccessFlags2::HOST_READ)
                .buffer(*buffer)
                .offset(0)
                .size(vk::WHOLE_SIZE)];
            let dep = vk::DependencyInfo::default().buffer_memory_barriers(&host_ready);
            device.cmd_pipeline_barrier2(frame.command, &dep);
        }
        device
            .end_command_buffer(frame.command)
            .map_err(vk_error("end frame command buffer"))?;
    }
    Ok(())
}

fn color(value: u32, foreground: bool) -> u32 {
    if value == DEFAULT_COLOR {
        return if foreground { 0x00d8d8d8 } else { 0x00181818 };
    }
    if value & 0xff00_0000 == RGB_COLOR_TAG {
        return pack_rgb(value & 0x00ff_ffff);
    }
    let index = if value & 0xff00_0000 == INDEXED_COLOR_TAG {
        (value & 0xff) as u8
    } else {
        value.min(15) as u8
    };
    let rgb = if index < 16 {
        const PALETTE: [u32; 16] = [
            0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5,
            0x7f7f7f, 0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
        ];
        PALETTE[index as usize]
    } else if index < 232 {
        let n = index - 16;
        let cube = |v: u8| if v == 0 { 0 } else { 55 + 40 * v as u32 };
        cube(n / 36) << 16 | cube(n / 6 % 6) << 8 | cube(n % 6)
    } else {
        let grey = 8 + (index as u32 - 232) * 10;
        grey << 16 | grey << 8 | grey
    };
    pack_rgb(rgb)
}

fn pack_rgb(rgb: u32) -> u32 {
    ((rgb >> 16) & 0xff) | (rgb & 0x00ff00) | ((rgb & 0xff) << 16)
}

fn vk_error(context: &'static str) -> impl FnOnce(vk::Result) -> String {
    move |error| format!("{context}: {error:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_cell_matches_four_word_storage_layout() {
        assert_eq!(size_of::<GpuCell>(), 16);
        assert_eq!(std::mem::offset_of!(GpuCell, glyph), 0);
        assert_eq!(std::mem::offset_of!(GpuCell, fg), 4);
        assert_eq!(std::mem::offset_of!(GpuCell, bg), 8);
        assert_eq!(std::mem::offset_of!(GpuCell, flags), 12);
        assert_eq!(size_of::<GpuGlyph>(), 48);
        assert_eq!(std::mem::offset_of!(GpuGlyph, uv), 0);
        assert_eq!(std::mem::offset_of!(GpuGlyph, size_bearing), 16);
        assert_eq!(std::mem::offset_of!(GpuGlyph, bearing_advance), 32);
        assert_eq!(size_of::<PushGrid>(), 40);
        assert_eq!(std::mem::offset_of!(PushGrid, cols), 0);
        assert_eq!(std::mem::offset_of!(PushGrid, rows), 4);
        assert_eq!(std::mem::offset_of!(PushGrid, cell_width), 8);
        assert_eq!(std::mem::offset_of!(PushGrid, cell_height), 12);
        assert_eq!(std::mem::offset_of!(PushGrid, ascent), 16);
        assert_eq!(std::mem::offset_of!(PushGrid, cursor), 20);
        assert_eq!(std::mem::offset_of!(PushGrid, cursor_visible), 24);
        assert_eq!(std::mem::offset_of!(PushGrid, selection_start), 28);
        assert_eq!(std::mem::offset_of!(PushGrid, selection_end), 32);
        assert_eq!(std::mem::offset_of!(PushGrid, cursor_style), 36);
    }

    #[test]
    fn decoration_geometry_matches_shader_formulas() {
        // One decoration row per 16 cell rows, at least one; underline sits on
        // the cell bottom edge inside the descent zone.
        assert_eq!(underline_rows(1), (0, 1));
        assert_eq!(underline_rows(12), (11, 12));
        assert_eq!(underline_rows(20), (19, 20));
        assert_eq!(underline_rows(32), (30, 32));
        // Strikethrough stays centered on the cell.
        assert_eq!(strikethrough_rows(1), (0, 1));
        assert_eq!(strikethrough_rows(12), (6, 7));
        assert_eq!(strikethrough_rows(20), (10, 11));
        assert_eq!(strikethrough_rows(32), (15, 17));
        // Thin cursors are twice the decoration thickness, at least two pixels.
        assert_eq!(cursor_span(1), 2);
        assert_eq!(cursor_span(9), 2);
        assert_eq!(cursor_span(20), 2);
        assert_eq!(cursor_span(48), 6);
        // Cursor styles map to the documented push-constant encoding.
        assert_eq!(push_cursor_style(aterm_core::CursorStyle::Block), 0);
        assert_eq!(push_cursor_style(aterm_core::CursorStyle::Underline), 1);
        assert_eq!(push_cursor_style(aterm_core::CursorStyle::Bar), 2);
    }

    #[test]
    fn ansi_color_words_convert_to_shader_channel_order() {
        assert_eq!(color(RGB_COLOR_TAG | 0x123456, true), 0x563412);
        assert_eq!(color(INDEXED_COLOR_TAG | 1, true), 0x0000cd);
        assert_eq!(color(INDEXED_COLOR_TAG | 196, true), 0x0000ff);
        assert_eq!(color(DEFAULT_COLOR, false), 0x00181818);
    }

    #[test]
    fn atlas_invalidation_resets_upload_flags() {
        // OffscreenRenderer::invalidate_atlas marks the atlas as needing a
        // full re-upload; the next render call must re-upload instead of
        // reusing stale GPU memory.
        if let Ok(mut renderer) = OffscreenRenderer::new(64, 64) {
            renderer.invalidate_atlas();
            assert!(!renderer.atlas_uploaded);
            assert!(!renderer.color_uploaded);
        }
    }
}
