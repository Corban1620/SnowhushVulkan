//! Draws the snow with Vulkan, the same passes as the Windows renderer: Halloween's pumpkins, the piles, read from a
//! height texture, the ghosts' signs, then every flake, grain, puff and ghost as an instanced quad, and rain's drops
//! on the glass, added up into a field first and drawn from it as glass.
//!
//! The Wayland connection's `wl_display` and each layer surface's `wl_surface` are handed straight to
//! `VK_KHR_wayland_surface`; there is no EGL or OpenGL anywhere in the drawing path.

use std::ffi::{CStr, c_void};
use std::mem::size_of;
use std::ptr;

use ash::khr::{surface as surface_khr, swapchain, wayland_surface};
use ash::{Device, Entry, Instance, vk};
use wayland_client::Proxy;
use wayland_client::protocol::wl_surface::WlSurface;

use snowhush_core::geometry::V2;
use snowhush_core::halloween::{self, PUMPKIN_ASPECT, PUMPKIN_CANDLE, SIGN_CAPTIONS};
use snowhush_core::simulation::{COLUMN_WIDTH, PumpkinSprite, SnowSimulation, Sprite};

const SPRITE_VERTEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/sprite.vert.spv"));
const SPRITE_FRAGMENT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/sprite.frag.spv"));
const FIELD_FRAGMENT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/field.frag.spv"));
const GLASS_VERTEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/glass.vert.spv"));
const GLASS_FRAGMENT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/glass.frag.spv"));
const PILE_VERTEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/pile.vert.spv"));
const PILE_FRAGMENT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/pile.frag.spv"));
const SIGN_VERTEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/sign.vert.spv"));
const SIGN_FRAGMENT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/sign.frag.spv"));
const PUMPKIN_VERTEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/pumpkin.vert.spv"));
const PUMPKIN_FRAGMENT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/pumpkin.frag.spv"));
const FADE_VERTEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/fade.vert.spv"));
const FADE_FRAGMENT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders/fade.frag.spv"));

/// The ghosts' signs, one picture per caption at two pixels per point, the stick's foot at the middle of the bottom edge.
const SIGNS: [&[u8]; SIGN_CAPTIONS.len()] = [
    include_bytes!("../../windows/assets/signs/sign-00.png"),
    include_bytes!("../../windows/assets/signs/sign-01.png"),
    include_bytes!("../../windows/assets/signs/sign-02.png"),
    include_bytes!("../../windows/assets/signs/sign-03.png"),
    include_bytes!("../../windows/assets/signs/sign-04.png"),
    include_bytes!("../../windows/assets/signs/sign-05.png"),
    include_bytes!("../../windows/assets/signs/sign-06.png"),
    include_bytes!("../../windows/assets/signs/sign-07.png"),
    include_bytes!("../../windows/assets/signs/sign-08.png"),
    include_bytes!("../../windows/assets/signs/sign-09.png"),
    include_bytes!("../../windows/assets/signs/sign-10.png"),
    include_bytes!("../../windows/assets/signs/sign-11.png"),
];
/// The jack-o'-lantern's shell and its carved face, apart so the candle flickers in the face alone, at two pixels per
/// unit of a drawing 100 units wide.
const PUMPKIN: [&[u8]; 2] = [include_bytes!("../../windows/assets/lanterns/shell.png"), include_bytes!("../../windows/assets/lanterns/face.png")];
/// How far the cursor's candle reaches, in points.
pub const LANTERN_REACH: f32 = 130.0;
/// The height texture's width; the pile shader wraps its index at the same width.
const ROW: usize = 4096;
const OCCLUDERS: usize = 16;
const FIELD_FORMAT: vk::Format = vk::Format::R16G16_SFLOAT;
const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";

/// The light the snow is drawn in.
#[derive(Clone, Copy)]
pub struct Ambient {
    pub light: [f32; 3],
    pub shadow_tint: [f32; 4],
    /// How far into Halloween night it is: moonlit snow, a candle's glow along the drifts' edges and one at the cursor.
    pub night: f32,
    /// Where the cursor's candle is, in the simulation's points.
    pub lantern: V2,
}

/// The block every shader reads, laid out exactly as `globals.glsl` describes it.
#[repr(C)]
#[derive(Clone, Copy)]
struct Globals {
    view: [f32; 4],
    light: [f32; 4],
    shadow: [f32; 4],
    rim: [f32; 4],
    lantern: [f32; 4],
    pumpkins: [[f32; 4]; 3],
    shake: [f32; 4],
    panel: [f32; 4],
    modes: [f32; 4],
    extra: [f32; 4],
}

/// The same fields as `pile.vert` and `pile.frag`'s push block.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PilePush {
    origin: [f32; 2],
    pile_top: f32,
    pile_bottom: f32,
    column_count: i32,
    height_offset: i32,
    occluder_count: i32,
    occluder_base: i32,
    drift: f32,
    _pad: [f32; 3],
}

/// The same fields as `sign.vert` and `sign.frag`'s push block.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SignPush {
    anchor: [f32; 2],
    size: [f32; 2],
    angle: f32,
    ground: f32,
    _pad: [f32; 2],
}

/// The same fields as `pumpkin.vert` and `pumpkin.frag`'s push block.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PumpkinPush {
    anchor: [f32; 2],
    size: [f32; 2],
    wick: [f32; 2],
    seed: f32,
    _pad: f32,
}

/// GPU memory and its host mapping. Everything Snowhush uploads is host visible, so there is no separate staging
/// copy for the snow itself.
struct Buffer {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    size: u64,
    mapped: *mut u8,
}

struct Texture {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    extent: vk::Extent2D,
    mip_levels: u32,
}

struct SignTexture {
    texture: Texture,
    descriptor: vk::DescriptorSet,
    size: [f32; 2],
}

struct PumpkinTextures {
    shell: Texture,
    face: Texture,
    descriptor: vk::DescriptorSet,
}

struct Field {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    descriptor: vk::DescriptorSet,
    extent: vk::Extent2D,
}

struct FieldPipelines {
    field: vk::Pipeline,
    glass: vk::Pipeline,
}

struct Pipelines {
    sprite: vk::Pipeline,
    pile: vk::Pipeline,
    sign: vk::Pipeline,
    pumpkin: vk::Pipeline,
    fade: vk::Pipeline,
}

struct Layouts {
    common: vk::PipelineLayout,
    glass: vk::PipelineLayout,
    pile: vk::PipelineLayout,
    sign: vk::PipelineLayout,
    pumpkin: vk::PipelineLayout,
}

/// One display's drawing surface. Each surface owns its swapchain and its frame's command buffer; the device and the
/// pipelines are shared.
pub struct Surface {
    surface: vk::SurfaceKHR,
    swapchain: vk::SwapchainKHR,
    extent: vk::Extent2D,
    format: vk::Format,
    color_space: vk::ColorSpaceKHR,
    render_pass: vk::RenderPass,
    present_mode: vk::PresentModeKHR,
    views: Vec<vk::ImageView>,
    framebuffers: Vec<vk::Framebuffer>,
    command_buffer: vk::CommandBuffer,
    image_available: vk::Semaphore,
    render_finished: Vec<vk::Semaphore>,
    fence: vk::Fence,
    globals: Buffer,
    globals_set: vk::DescriptorSet,
    field: Option<Field>,
    vertex_buffer: Option<Buffer>,
    vertex_capacity: usize,
    pub width: i32,
    pub height: i32,
    needs_swapchain: bool,
}

pub struct Gpu {
    entry: Entry,
    instance: Instance,
    surface_loader: surface_khr::Instance,
    wayland_loader: wayland_surface::Instance,
    swapchain_loader: swapchain::Device,
    physical: vk::PhysicalDevice,
    device: Device,
    queue: vk::Queue,
    queue_family: u32,
    display: *mut c_void,
    command_pool: vk::CommandPool,
    descriptor_pool: vk::DescriptorPool,
    common_set_layout: vk::DescriptorSetLayout,
    glass_set_layout: vk::DescriptorSetLayout,
    pile_set_layout: vk::DescriptorSetLayout,
    sign_set_layout: vk::DescriptorSetLayout,
    pumpkin_set_layout: vk::DescriptorSetLayout,
    layouts: Layouts,
    field_render_pass: vk::RenderPass,
    field_pipelines: Option<FieldPipelines>,
    pipelines: Option<Pipelines>,
    format: Option<vk::Format>,
    nearest_sampler: vk::Sampler,
    linear_sampler: vk::Sampler,
    /// Drops on the glass add up into this format's field; without it, they are drawn as sprites.
    field_supported: bool,
    occluders: Buffer,
    pile_set: Option<vk::DescriptorSet>,
    heights: Option<Buffer>,
    signs: Vec<Option<SignTexture>>,
    pumpkin: Option<PumpkinTextures>,
    sprites: Vec<Sprite>,
    columns: Vec<f32>,
    pub adapter: String,
}

impl Gpu {
    /// `display` is the Wayland connection's `wl_display`.
    pub fn new(display: *mut c_void) -> Result<Gpu, String> {
        unsafe {
            let entry = Entry::load().map_err(|error| format!("loading Vulkan: {error}"))?;
            let layers = entry.enumerate_instance_layer_properties().map_err(|error| error.to_string())?;
            let validation = std::env::var_os("SNOWHUSH_VALIDATION").is_some() && layers.iter().any(|layer| layer_name(layer) == VALIDATION_LAYER);
            let extensions = entry.enumerate_instance_extension_properties(None).map_err(|error| error.to_string())?;
            for required in [surface_khr::NAME, wayland_surface::NAME] {
                if !extensions.iter().any(|extension| CStr::from_ptr(extension.extension_name.as_ptr()) == required) {
                    return Err(format!("the Vulkan driver has no {}", required.to_string_lossy()));
                }
            }
            let application = c"Snowhush";
            let engine = c"Snowhush";
            let api = vk::make_api_version(0, 1, 1, 0);
            let application_info = vk::ApplicationInfo::default().application_name(application).engine_name(engine).api_version(api);
            let layer_names = [VALIDATION_LAYER.as_ptr()];
            let extension_names = [surface_khr::NAME.as_ptr(), wayland_surface::NAME.as_ptr()];
            let mut create_info = vk::InstanceCreateInfo::default().application_info(&application_info).enabled_extension_names(&extension_names);
            if validation {
                create_info = create_info.enabled_layer_names(&layer_names);
            }
            let instance = entry.create_instance(&create_info, None).map_err(|error| format!("Vulkan instance: {error}"))?;
            let surface_loader = surface_khr::Instance::new(&entry, &instance);
            let wayland_loader = wayland_surface::Instance::new(&entry, &instance);

            let physical_devices = instance.enumerate_physical_devices().map_err(|error| error.to_string())?;
            let mut chosen = None;
            for physical in physical_devices {
                let properties = instance.get_physical_device_properties(physical);
                let extensions = instance.enumerate_device_extension_properties(physical).map_err(|error| error.to_string())?;
                if !extensions.iter().any(|extension| CStr::from_ptr(extension.extension_name.as_ptr()) == swapchain::NAME) {
                    continue;
                }
                let families = instance.get_physical_device_queue_family_properties(physical);
                for (index, family) in families.iter().enumerate() {
                    if !family.queue_flags.contains(vk::QueueFlags::GRAPHICS) {
                        continue;
                    }
                    let supported = wayland_loader.get_physical_device_wayland_presentation_support(
                        physical,
                        index as u32,
                        &mut *display.cast::<vk::wl_display>(),
                    );
                    if !supported {
                        continue;
                    }
                    let score = match properties.device_type {
                        vk::PhysicalDeviceType::DISCRETE_GPU => 4,
                        vk::PhysicalDeviceType::INTEGRATED_GPU => 3,
                        vk::PhysicalDeviceType::VIRTUAL_GPU => 2,
                        _ => 1,
                    };
                    if chosen.as_ref().is_none_or(|(best, _, _)| score > *best) {
                        chosen = Some((score, physical, index as u32));
                    }
                }
            }
            let (_, physical, queue_family) = chosen.ok_or("no Vulkan device can draw to this Wayland display")?;
            let properties = instance.get_physical_device_properties(physical);
            let adapter = format!(
                "{} (Vulkan {}.{}.{})",
                device_name(&properties),
                vk::api_version_major(properties.api_version),
                vk::api_version_minor(properties.api_version),
                vk::api_version_patch(properties.api_version)
            );
            let priority = [1.0f32];
            let queues = [vk::DeviceQueueCreateInfo::default().queue_family_index(queue_family).queue_priorities(&priority)];
            let extension_names = [swapchain::NAME.as_ptr()];
            let device_info = vk::DeviceCreateInfo::default().queue_create_infos(&queues).enabled_extension_names(&extension_names);
            let device = instance.create_device(physical, &device_info, None).map_err(|error| format!("Vulkan device: {error}"))?;
            let queue = device.get_device_queue(queue_family, 0);
            let swapchain_loader = swapchain::Device::new(&instance, &device);

            let command_pool = device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default().queue_family_index(queue_family).flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
                .map_err(|error| format!("command pool: {error}"))?;

            let common_set_layout = descriptor_layout(
                &device,
                &[binding(0, vk::DescriptorType::UNIFORM_BUFFER, vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)],
            )?;
            let glass_set_layout =
                descriptor_layout(&device, &[binding(0, vk::DescriptorType::COMBINED_IMAGE_SAMPLER, vk::ShaderStageFlags::FRAGMENT)])?;
            let pile_set_layout = descriptor_layout(
                &device,
                &[
                    binding(0, vk::DescriptorType::STORAGE_BUFFER, vk::ShaderStageFlags::FRAGMENT),
                    binding(1, vk::DescriptorType::STORAGE_BUFFER, vk::ShaderStageFlags::FRAGMENT),
                ],
            )?;
            let sign_set_layout =
                descriptor_layout(&device, &[binding(0, vk::DescriptorType::COMBINED_IMAGE_SAMPLER, vk::ShaderStageFlags::FRAGMENT)])?;
            let pumpkin_set_layout = descriptor_layout(
                &device,
                &[
                    binding(0, vk::DescriptorType::COMBINED_IMAGE_SAMPLER, vk::ShaderStageFlags::FRAGMENT),
                    binding(1, vk::DescriptorType::COMBINED_IMAGE_SAMPLER, vk::ShaderStageFlags::FRAGMENT),
                ],
            )?;

            let common_layout = pipeline_layout(&device, &[common_set_layout], &[])?;
            let glass_layout = pipeline_layout(&device, &[common_set_layout, glass_set_layout], &[])?;
            let pile_layout = pipeline_layout(
                &device,
                &[common_set_layout, pile_set_layout],
                &[vk::PushConstantRange::default()
                    .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
                    .offset(0)
                    .size(size_of::<PilePush>() as u32)],
            )?;
            let sign_layout = pipeline_layout(
                &device,
                &[common_set_layout, sign_set_layout],
                &[vk::PushConstantRange::default()
                    .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
                    .offset(0)
                    .size(size_of::<SignPush>() as u32)],
            )?;
            let pumpkin_layout = pipeline_layout(
                &device,
                &[common_set_layout, pumpkin_set_layout],
                &[vk::PushConstantRange::default()
                    .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
                    .offset(0)
                    .size(size_of::<PumpkinPush>() as u32)],
            )?;
            let layouts = Layouts { common: common_layout, glass: glass_layout, pile: pile_layout, sign: sign_layout, pumpkin: pumpkin_layout };

            let pool_sizes = [
                vk::DescriptorPoolSize::default().ty(vk::DescriptorType::UNIFORM_BUFFER).descriptor_count(512),
                vk::DescriptorPoolSize::default().ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).descriptor_count(512),
                vk::DescriptorPoolSize::default().ty(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(64),
            ];
            let descriptor_pool = device
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                        .max_sets(1024)
                        .pool_sizes(&pool_sizes),
                    None,
                )
                .map_err(|error| format!("descriptor pool: {error}"))?;

            let nearest_sampler = sampler(&device, vk::Filter::NEAREST, vk::Filter::NEAREST, 0.0)?;
            let linear_sampler = sampler(&device, vk::Filter::LINEAR, vk::Filter::LINEAR, vk::LOD_CLAMP_NONE)?;
            let field_supported = instance
                .get_physical_device_format_properties(physical, FIELD_FORMAT)
                .optimal_tiling_features
                .contains(vk::FormatFeatureFlags::COLOR_ATTACHMENT | vk::FormatFeatureFlags::SAMPLED_IMAGE);
            let field_render_pass = create_field_render_pass(&device)?;
            let occluders = create_buffer(&device, &instance, physical, 64 * OCCLUDERS as u64 * 16, vk::BufferUsageFlags::STORAGE_BUFFER)?;

            Ok(Gpu {
                entry,
                instance,
                surface_loader,
                wayland_loader,
                swapchain_loader,
                physical,
                device,
                queue,
                queue_family,
                display,
                command_pool,
                descriptor_pool,
                common_set_layout,
                glass_set_layout,
                pile_set_layout,
                sign_set_layout,
                pumpkin_set_layout,
                layouts,
                field_render_pass,
                field_pipelines: None,
                pipelines: None,
                format: None,
                nearest_sampler,
                linear_sampler,
                field_supported,
                occluders,
                pile_set: None,
                heights: None,
                signs: (0..SIGNS.len()).map(|_| None).collect(),
                pumpkin: None,
                sprites: Vec::new(),
                columns: Vec::new(),
                adapter,
            })
        }
    }

    pub fn surface(&mut self, surface: &WlSurface, width: i32, height: i32) -> Result<Surface, String> {
        unsafe {
            let create_info = vk::WaylandSurfaceCreateInfoKHR::default().display(self.display.cast()).surface(surface.id().as_ptr().cast());
            let wayland = self.wayland_loader.create_wayland_surface(&create_info, None).map_err(|error| format!("Wayland surface: {error}"))?;
            let supported = self
                .surface_loader
                .get_physical_device_surface_support(self.physical, self.queue_family, wayland)
                .map_err(|error| error.to_string())?;
            if !supported {
                self.surface_loader.destroy_surface(wayland, None);
                return Err("the Vulkan device can't present to this display".into());
            }
            let (format, color_space) = self.surface_format(wayland)?;
            let render_pass = create_main_render_pass(&self.device, format)?;
            self.ensure_pipelines(render_pass)?;
            let command_buffer = self
                .device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.command_pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .map_err(|error| format!("command buffer: {error}"))?[0];
            let image_available = self.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).map_err(|error| error.to_string())?;
            let fence = self
                .device
                .create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None)
                .map_err(|error| error.to_string())?;
            let globals =
                create_buffer(&self.device, &self.instance, self.physical, size_of::<Globals>() as u64, vk::BufferUsageFlags::UNIFORM_BUFFER)?;
            let globals_set = self.allocate_set(self.common_set_layout)?;
            write_buffer(&self.device, globals_set, 0, globals.buffer, globals.size);
            let mut result = Surface {
                surface: wayland,
                swapchain: vk::SwapchainKHR::null(),
                extent: vk::Extent2D { width: width.max(1) as u32, height: height.max(1) as u32 },
                format,
                color_space,
                render_pass,
                present_mode: vk::PresentModeKHR::FIFO,
                views: Vec::new(),
                framebuffers: Vec::new(),
                command_buffer,
                image_available,
                render_finished: Vec::new(),
                fence,
                globals,
                globals_set,
                field: None,
                vertex_buffer: None,
                vertex_capacity: 0,
                width: width.max(1),
                height: height.max(1),
                needs_swapchain: true,
            };
            self.recreate_swapchain(&mut result)?;
            crate::log!("Vulkan surface: {:?}, {:?}, {}x{}", result.format, result.present_mode, result.extent.width, result.extent.height);
            Ok(result)
        }
    }

    pub fn resize(&mut self, surface: &mut Surface, width: i32, height: i32) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) != (surface.width, surface.height) {
            surface.width = width;
            surface.height = height;
            surface.needs_swapchain = true;
        }
    }

    pub fn release(&mut self, mut surface: Surface) {
        unsafe {
            let _ = self.device.device_wait_idle();
            if let Some(field) = surface.field.take() {
                self.destroy_field(field);
            }
            for framebuffer in surface.framebuffers.drain(..) {
                self.device.destroy_framebuffer(framebuffer, None);
            }
            for view in surface.views.drain(..) {
                self.device.destroy_image_view(view, None);
            }
            for semaphore in surface.render_finished.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
            }
            self.device.destroy_semaphore(surface.image_available, None);
            self.device.destroy_fence(surface.fence, None);
            self.device.free_command_buffers(self.command_pool, &[surface.command_buffer]);
            if let Some(vertex) = surface.vertex_buffer.take() {
                destroy_buffer(&self.device, vertex);
            }
            let globals = std::mem::replace(
                &mut surface.globals,
                Buffer { buffer: vk::Buffer::null(), memory: vk::DeviceMemory::null(), size: 0, mapped: ptr::null_mut() },
            );
            destroy_buffer(&self.device, globals);
            self.device.free_descriptor_sets(self.descriptor_pool, &[surface.globals_set]).ok();
            if surface.swapchain != vk::SwapchainKHR::null() {
                self.swapchain_loader.destroy_swapchain(surface.swapchain, None);
            }
            self.device.destroy_render_pass(surface.render_pass, None);
            self.surface_loader.destroy_surface(surface.surface, None);
        }
    }

    /// Shows nothing on the surface, as when its display is covered.
    pub fn clear(&mut self, surface: &mut Surface) -> Result<(), String> {
        let Some((index, command_buffer)) = self.begin_frame(surface)? else { return Ok(()) };
        unsafe {
            let clear = [vk::ClearValue { color: vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 0.0] } }];
            let begin = vk::RenderPassBeginInfo::default()
                .render_pass(surface.render_pass)
                .framebuffer(surface.framebuffers[index as usize])
                .render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent: surface.extent })
                .clear_values(&clear);
            self.device.cmd_begin_render_pass(command_buffer, &begin, vk::SubpassContents::INLINE);
            self.device.cmd_end_render_pass(command_buffer);
            self.device.end_command_buffer(command_buffer).map_err(|error| error.to_string())?;
        }
        self.submit_frame(surface, index, command_buffer)
    }

    /// `panel` is a rectangle the panel stands in front of, left, bottom, right, top in the simulation's
    /// coordinates: nothing of the snow shows inside it.
    pub fn draw(
        &mut self,
        surface: &mut Surface,
        simulation: &SnowSimulation,
        ambient: Ambient,
        panel: Option<[f32; 4]>,
        time: f32,
        opacity: f32,
    ) -> Result<(), String> {
        if !self.ensure_swapchain(surface)? {
            return Ok(());
        }
        // The previous frame is done before any of its buffers, textures or descriptor sets are touched.
        self.wait_frame(surface)?;
        let sprite_count = simulation.sprite_count();
        self.sprites.resize(sprite_count, Sprite::default());
        let sprites = simulation.write_sprites(&mut self.sprites);
        let columns = simulation.column_count();
        let rows = columns.div_ceil(ROW).max(1);
        self.columns.resize(rows * ROW, 0.0);
        let piles = simulation.write_layers(&mut self.columns[..columns]);
        let signs = simulation.signs();
        for sign in &signs {
            self.load_sign(sign.caption)?;
        }
        let pumpkins = simulation.pumpkins();
        if !pumpkins.is_empty() {
            self.load_pumpkin()?;
        }

        let (light, shadow_tint) = halloween::moonlit(ambient.light, ambient.shadow_tint, ambient.night);
        let mut globals = Globals {
            view: [simulation.width(), simulation.height(), 1.0, time],
            light: [light[0], light[1], light[2], 1.0],
            shadow: shadow_tint,
            rim: [1.0, 0.56, 0.2, 0.3 * ambient.night],
            lantern: [ambient.lantern.x, ambient.lantern.y, LANTERN_REACH, ambient.night],
            pumpkins: [[0.0; 4]; 3],
            shake: [simulation.shake().x, simulation.shake().y, COLUMN_WIDTH, 0.0],
            panel: panel.unwrap_or([0.0; 4]),
            modes: [
                if simulation.leafy() { 1.0 } else { 0.0 },
                if simulation.blossom() { 1.0 } else { 0.0 },
                if simulation.wilts() { 1.0 } else { 0.0 },
                0.0,
            ],
            extra: [ambient.night, 0.0, 0.0, 0.0],
        };
        // The pumpkins' candles light the snow as the cursor's does, a little even by day; a missing one is dark.
        for (candle, pumpkin) in globals.pumpkins.iter_mut().zip(&pumpkins) {
            let wick = wick(pumpkin);
            *candle = [wick.x, wick.y, pumpkin.width * 1.6, 0.4 + 0.6 * ambient.night];
        }

        let glass = sprites > 0 && self.field_supported && simulation.has_glass_drops();
        globals.modes[3] = if glass { 1.0 } else { 0.0 };
        self.write_globals(surface, &globals);

        self.ensure_vertex_capacity(surface, sprites * size_of::<Sprite>())?;
        if sprites > 0 {
            surface.vertex_buffer.as_ref().expect("sprite buffer").write(bytes(&self.sprites[..sprites]));
        }

        let pile_draws: Vec<(PilePush, usize)> = piles
            .iter()
            .map(|layer| {
                let count = layer.occluders.len().min(OCCLUDERS);
                (
                    PilePush {
                        origin: [layer.origin.x, layer.origin.y],
                        pile_top: layer.top + 2.0,
                        pile_bottom: layer.bottom,
                        column_count: layer.count as i32,
                        height_offset: layer.offset as i32,
                        occluder_count: count as i32,
                        occluder_base: 0,
                        drift: layer.drift,
                        _pad: [0.0; 3],
                    },
                    count,
                )
            })
            .collect();
        let mut occluder_values = Vec::with_capacity(pile_draws.iter().map(|(_, count)| count).sum::<usize>());
        let mut pile_draws = pile_draws;
        for ((draw, count), layer) in pile_draws.iter_mut().zip(&piles) {
            draw.occluder_base = (occluder_values.len() / 4) as i32;
            occluder_values.extend(layer.occluders[..*count].iter().flatten().copied());
        }
        if !occluder_values.is_empty() {
            self.ensure_occluders(occluder_values.len())?;
            self.occluders.write(bytes(&occluder_values));
        }
        if !piles.is_empty() {
            self.ensure_heights(rows * ROW * size_of::<f32>())?;
            self.heights.as_ref().expect("height buffer").write(bytes(&self.columns[..rows * ROW]));
        }

        let sign_draws: Vec<(SignPush, vk::DescriptorSet)> = signs
            .iter()
            .filter_map(|held| {
                let picture = self.signs.get(held.caption)?.as_ref()?;
                Some((
                    SignPush { anchor: [held.anchor.x, held.anchor.y], size: picture.size, angle: held.angle, ground: held.ground, _pad: [0.0; 2] },
                    picture.descriptor,
                ))
            })
            .collect();
        let pumpkin_draws: Vec<PumpkinPush> = pumpkins
            .iter()
            .map(|sitting| {
                let wick = wick(sitting);
                PumpkinPush {
                    anchor: [sitting.anchor.x, sitting.anchor.y],
                    size: [sitting.width, sitting.width * PUMPKIN_ASPECT],
                    wick: [wick.x, wick.y],
                    seed: sitting.seed,
                    _pad: 0.0,
                }
            })
            .collect();
        // Make sure the field exists before recording; like the other textures it is per surface and per size.
        if glass {
            self.ensure_field(surface)?;
        } else if let Some(field) = surface.field.take() {
            self.destroy_field(field);
        }

        let Some((index, command_buffer)) = self.acquire_frame(surface)? else { return Ok(()) };
        unsafe {
            if glass && let Some(field) = &surface.field {
                let clear = [vk::ClearValue { color: vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 0.0] } }];
                let begin = vk::RenderPassBeginInfo::default()
                    .render_pass(self.field_render_pass)
                    .framebuffer(field.framebuffer)
                    .render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent: surface.extent })
                    .clear_values(&clear);
                self.device.cmd_begin_render_pass(command_buffer, &begin, vk::SubpassContents::INLINE);
                self.set_viewport(command_buffer, surface.extent);
                self.device.cmd_bind_pipeline(command_buffer, vk::PipelineBindPoint::GRAPHICS, self.field_pipelines.as_ref().unwrap().field);
                self.device.cmd_bind_descriptor_sets(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.layouts.common,
                    0,
                    &[surface.globals_set],
                    &[],
                );
                if sprites > 0 {
                    self.device.cmd_bind_vertex_buffers(command_buffer, 0, &[surface.vertex_buffer.as_ref().unwrap().buffer], &[0]);
                    self.device.cmd_draw(command_buffer, 4, sprites as u32, 0, 0);
                }
                self.device.cmd_end_render_pass(command_buffer);
                image_barrier(
                    &self.device,
                    command_buffer,
                    field.image,
                    vk::ImageAspectFlags::COLOR,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                    vk::AccessFlags::SHADER_READ,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                );
            }
            let clear = [vk::ClearValue { color: vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 0.0] } }];
            let begin = vk::RenderPassBeginInfo::default()
                .render_pass(surface.render_pass)
                .framebuffer(surface.framebuffers[index as usize])
                .render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent: surface.extent })
                .clear_values(&clear);
            self.device.cmd_begin_render_pass(command_buffer, &begin, vk::SubpassContents::INLINE);
            self.set_viewport(command_buffer, surface.extent);

            // The pumpkins go down before the snow, so the drift they sit in covers their bottoms.
            if !pumpkin_draws.is_empty()
                && let Some(pumpkin) = &self.pumpkin
            {
                self.device.cmd_bind_pipeline(command_buffer, vk::PipelineBindPoint::GRAPHICS, self.pipelines.as_ref().unwrap().pumpkin);
                self.device.cmd_bind_descriptor_sets(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.layouts.pumpkin,
                    0,
                    &[surface.globals_set, pumpkin.descriptor],
                    &[],
                );
                for draw in &pumpkin_draws {
                    push_constants(&self.device, command_buffer, self.layouts.pumpkin, draw);
                    self.device.cmd_draw(command_buffer, 4, 1, 0, 0);
                }
            }
            if !pile_draws.is_empty()
                && let Some(pile_set) = self.pile_set
            {
                self.device.cmd_bind_pipeline(command_buffer, vk::PipelineBindPoint::GRAPHICS, self.pipelines.as_ref().unwrap().pile);
                self.device.cmd_bind_descriptor_sets(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.layouts.pile,
                    0,
                    &[surface.globals_set, pile_set],
                    &[],
                );
                for (draw, _) in &pile_draws {
                    push_constants(&self.device, command_buffer, self.layouts.pile, draw);
                    self.device.cmd_draw(command_buffer, 4, 1, 0, 0);
                }
            }
            for (draw, descriptor) in &sign_draws {
                self.device.cmd_bind_pipeline(command_buffer, vk::PipelineBindPoint::GRAPHICS, self.pipelines.as_ref().unwrap().sign);
                self.device.cmd_bind_descriptor_sets(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.layouts.sign,
                    0,
                    &[surface.globals_set, *descriptor],
                    &[],
                );
                push_constants(&self.device, command_buffer, self.layouts.sign, draw);
                self.device.cmd_draw(command_buffer, 4, 1, 0, 0);
            }
            if sprites > 0 {
                self.device.cmd_bind_pipeline(command_buffer, vk::PipelineBindPoint::GRAPHICS, self.pipelines.as_ref().unwrap().sprite);
                self.device.cmd_bind_descriptor_sets(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.layouts.common,
                    0,
                    &[surface.globals_set],
                    &[],
                );
                self.device.cmd_bind_vertex_buffers(command_buffer, 0, &[surface.vertex_buffer.as_ref().unwrap().buffer], &[0]);
                self.device.cmd_draw(command_buffer, 4, sprites as u32, 0, 0);
                if let Some(field) = &surface.field {
                    self.device.cmd_bind_pipeline(command_buffer, vk::PipelineBindPoint::GRAPHICS, self.field_pipelines.as_ref().unwrap().glass);
                    self.device.cmd_bind_descriptor_sets(
                        command_buffer,
                        vk::PipelineBindPoint::GRAPHICS,
                        self.layouts.glass,
                        0,
                        &[surface.globals_set, field.descriptor],
                        &[],
                    );
                    self.device.cmd_draw(command_buffer, 3, 1, 0, 0);
                }
            }
            // Fading under a full-screen app or back: the snow as drawn, all of it at once, so where flakes cross a
            // drift it fades as one.
            if opacity < 1.0 {
                self.device.cmd_bind_pipeline(command_buffer, vk::PipelineBindPoint::GRAPHICS, self.pipelines.as_ref().unwrap().fade);
                self.device.cmd_set_blend_constants(command_buffer, &[0.0, 0.0, 0.0, opacity]);
                self.device.cmd_draw(command_buffer, 3, 1, 0, 0);
            }
            self.device.cmd_end_render_pass(command_buffer);
            self.device.end_command_buffer(command_buffer).map_err(|error| error.to_string())?;
        }
        self.submit_frame(surface, index, command_buffer)
    }

    fn write_globals(&self, surface: &Surface, globals: &Globals) {
        surface.globals.write(bytes(std::slice::from_ref(globals)));
    }

    fn ensure_vertex_capacity(&mut self, surface: &mut Surface, bytes: usize) -> Result<(), String> {
        if bytes <= surface.vertex_capacity {
            return Ok(());
        }
        if let Some(old) = surface.vertex_buffer.take() {
            unsafe { destroy_buffer(&self.device, old) };
        }
        let capacity = bytes.next_power_of_two().max(4096);
        surface.vertex_buffer =
            Some(unsafe { create_buffer(&self.device, &self.instance, self.physical, capacity as u64, vk::BufferUsageFlags::VERTEX_BUFFER)? });
        surface.vertex_capacity = capacity;
        Ok(())
    }

    fn ensure_swapchain(&mut self, surface: &mut Surface) -> Result<bool, String> {
        if surface.needs_swapchain {
            self.recreate_swapchain(surface)?;
        }
        Ok(surface.extent.width > 0 && surface.extent.height > 0)
    }

    fn recreate_swapchain(&mut self, surface: &mut Surface) -> Result<(), String> {
        unsafe {
            let _ = self.device.device_wait_idle();
            if let Some(field) = surface.field.take() {
                self.destroy_field(field);
            }
            for framebuffer in surface.framebuffers.drain(..) {
                self.device.destroy_framebuffer(framebuffer, None);
            }
            for view in surface.views.drain(..) {
                self.device.destroy_image_view(view, None);
            }
            for semaphore in surface.render_finished.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
            }
            if surface.swapchain != vk::SwapchainKHR::null() {
                self.swapchain_loader.destroy_swapchain(surface.swapchain, None);
                surface.swapchain = vk::SwapchainKHR::null();
            }
            let capabilities =
                self.surface_loader.get_physical_device_surface_capabilities(self.physical, surface.surface).map_err(|error| error.to_string())?;
            let extent = if capabilities.current_extent.width != u32::MAX {
                capabilities.current_extent
            } else {
                vk::Extent2D {
                    width: (surface.width as u32).clamp(capabilities.min_image_extent.width, capabilities.max_image_extent.width),
                    height: (surface.height as u32).clamp(capabilities.min_image_extent.height, capabilities.max_image_extent.height),
                }
            };
            if extent.width == 0 || extent.height == 0 {
                surface.extent = extent;
                surface.needs_swapchain = true;
                return Ok(());
            }
            let present_modes =
                self.surface_loader.get_physical_device_surface_present_modes(self.physical, surface.surface).map_err(|error| error.to_string())?;
            let present_mode = [vk::PresentModeKHR::MAILBOX, vk::PresentModeKHR::IMMEDIATE, vk::PresentModeKHR::FIFO]
                .into_iter()
                .find(|mode| present_modes.contains(mode))
                .unwrap_or(vk::PresentModeKHR::FIFO);
            let composite = if capabilities.supported_composite_alpha.contains(vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED) {
                vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED
            } else if capabilities.supported_composite_alpha.contains(vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED) {
                vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED
            } else {
                vk::CompositeAlphaFlagsKHR::INHERIT
            };
            let mut count = capabilities.min_image_count + 1;
            if capabilities.max_image_count > 0 {
                count = count.min(capabilities.max_image_count);
            }
            let create_info = vk::SwapchainCreateInfoKHR::default()
                .surface(surface.surface)
                .min_image_count(count)
                .image_format(surface.format)
                .image_color_space(surface.color_space)
                .image_extent(extent)
                .image_array_layers(1)
                .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(capabilities.current_transform)
                .composite_alpha(composite)
                .present_mode(present_mode)
                .clipped(true);
            let swapchain = self.swapchain_loader.create_swapchain(&create_info, None).map_err(|error| format!("swapchain: {error}"))?;
            let images = self.swapchain_loader.get_swapchain_images(swapchain).map_err(|error| error.to_string())?;
            let mut views = Vec::with_capacity(images.len());
            for image in &images {
                views.push(create_image_view(&self.device, *image, surface.format, vk::ImageAspectFlags::COLOR, 1)?);
            }
            let mut framebuffers = Vec::with_capacity(views.len());
            for view in &views {
                framebuffers.push(
                    self.device
                        .create_framebuffer(
                            &vk::FramebufferCreateInfo::default()
                                .render_pass(surface.render_pass)
                                .attachments(std::slice::from_ref(view))
                                .width(extent.width)
                                .height(extent.height)
                                .layers(1),
                            None,
                        )
                        .map_err(|error| format!("framebuffer: {error}"))?,
                );
            }
            let mut render_finished = Vec::with_capacity(images.len());
            for _ in &images {
                render_finished.push(self.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).map_err(|error| error.to_string())?);
            }
            surface.swapchain = swapchain;
            surface.extent = extent;
            surface.present_mode = present_mode;
            surface.views = views;
            surface.framebuffers = framebuffers;
            surface.render_finished = render_finished;
            surface.width = extent.width as i32;
            surface.height = extent.height as i32;
            surface.needs_swapchain = false;
            Ok(())
        }
    }

    fn wait_frame(&mut self, surface: &Surface) -> Result<(), String> {
        unsafe { self.device.wait_for_fences(&[surface.fence], true, u64::MAX).map_err(|error| error.to_string()) }
    }

    fn begin_frame(&mut self, surface: &mut Surface) -> Result<Option<(u32, vk::CommandBuffer)>, String> {
        if !self.ensure_swapchain(surface)? {
            return Ok(None);
        }
        self.wait_frame(surface)?;
        self.acquire_frame(surface)
    }

    fn acquire_frame(&mut self, surface: &mut Surface) -> Result<Option<(u32, vk::CommandBuffer)>, String> {
        unsafe {
            let acquired = self.swapchain_loader.acquire_next_image(surface.swapchain, u64::MAX, surface.image_available, vk::Fence::null());
            let index = match acquired {
                Ok((index, suboptimal)) => {
                    if suboptimal {
                        surface.needs_swapchain = true;
                    }
                    index
                }
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                    surface.needs_swapchain = true;
                    self.recreate_swapchain(surface)?;
                    return Ok(None);
                }
                Err(error) => return Err(format!("acquiring a swapchain image: {error}")),
            };
            self.device.reset_fences(&[surface.fence]).map_err(|error| error.to_string())?;
            self.device.reset_command_buffer(surface.command_buffer, vk::CommandBufferResetFlags::empty()).map_err(|error| error.to_string())?;
            let begin = vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            self.device.begin_command_buffer(surface.command_buffer, &begin).map_err(|error| error.to_string())?;
            Ok(Some((index, surface.command_buffer)))
        }
    }

    fn submit_frame(&mut self, surface: &mut Surface, index: u32, command_buffer: vk::CommandBuffer) -> Result<(), String> {
        unsafe {
            let wait = [surface.image_available];
            let stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
            let signal = [surface.render_finished[index as usize]];
            let command_buffers = [command_buffer];
            let submit = vk::SubmitInfo::default()
                .wait_semaphores(&wait)
                .wait_dst_stage_mask(&stages)
                .command_buffers(&command_buffers)
                .signal_semaphores(&signal);
            self.device.queue_submit(self.queue, &[submit], surface.fence).map_err(|error| format!("submitting a frame: {error}"))?;
            let swapchains = [surface.swapchain];
            let indices = [index];
            let present = vk::PresentInfoKHR::default().wait_semaphores(&signal).swapchains(&swapchains).image_indices(&indices);
            match self.swapchain_loader.queue_present(self.queue, &present) {
                Ok(suboptimal) => {
                    if suboptimal {
                        surface.needs_swapchain = true;
                    }
                }
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) | Err(vk::Result::SUBOPTIMAL_KHR) => surface.needs_swapchain = true,
                Err(error) => return Err(format!("presenting a frame: {error}")),
            }
            Ok(())
        }
    }

    fn set_viewport(&self, command_buffer: vk::CommandBuffer, extent: vk::Extent2D) {
        let viewport = vk::Viewport {
            x: 0.0,
            y: extent.height as f32,
            width: extent.width as f32,
            height: -(extent.height as f32),
            min_depth: 0.0,
            max_depth: 1.0,
        };
        let scissor = vk::Rect2D { offset: vk::Offset2D::default(), extent };
        unsafe {
            self.device.cmd_set_viewport(command_buffer, 0, &[viewport]);
            self.device.cmd_set_scissor(command_buffer, 0, &[scissor]);
        }
    }

    fn surface_format(&mut self, surface: vk::SurfaceKHR) -> Result<(vk::Format, vk::ColorSpaceKHR), String> {
        let formats = unsafe { self.surface_loader.get_physical_device_surface_formats(self.physical, surface).map_err(|error| error.to_string())? };
        if let Some(format) = self.format {
            if let Some(candidate) = formats.iter().find(|candidate| candidate.format == format) {
                return Ok((format, candidate.color_space));
            }
            return Err("a display uses a different surface format from the first".into());
        }
        let preferred = [
            vk::Format::B8G8R8A8_UNORM,
            vk::Format::R8G8B8A8_UNORM,
            vk::Format::B8G8R8A8_SRGB,
            vk::Format::R8G8B8A8_SRGB,
            vk::Format::A2B10G10R10_UNORM_PACK32,
        ];
        let chosen = preferred
            .into_iter()
            .find_map(|format| {
                formats.iter().find(|candidate| candidate.format == format && candidate.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR)
            })
            .or_else(|| formats.first())
            .ok_or("the Wayland surface offers no formats")?;
        self.format = Some(chosen.format);
        Ok((chosen.format, chosen.color_space))
    }

    fn ensure_pipelines(&mut self, render_pass: vk::RenderPass) -> Result<(), String> {
        if self.pipelines.is_some() {
            return Ok(());
        }
        unsafe {
            let modules = ShaderModules::new(&self.device)?;
            let field = create_pipeline(
                &self.device,
                self.layouts.common,
                self.field_render_pass,
                vk::PrimitiveTopology::TRIANGLE_STRIP,
                Some(sprite_input()),
                &modules.sprite_vertex,
                &modules.field_fragment,
                Blend::Field,
            )?;
            let glass = create_pipeline(
                &self.device,
                self.layouts.glass,
                render_pass,
                vk::PrimitiveTopology::TRIANGLE_LIST,
                None,
                &modules.glass_vertex,
                &modules.glass_fragment,
                Blend::Glass,
            )?;
            let sprite = create_pipeline(
                &self.device,
                self.layouts.common,
                render_pass,
                vk::PrimitiveTopology::TRIANGLE_STRIP,
                Some(sprite_input()),
                &modules.sprite_vertex,
                &modules.sprite_fragment,
                Blend::Premultiplied,
            )?;
            let pile = create_pipeline(
                &self.device,
                self.layouts.pile,
                render_pass,
                vk::PrimitiveTopology::TRIANGLE_STRIP,
                None,
                &modules.pile_vertex,
                &modules.pile_fragment,
                Blend::Premultiplied,
            )?;
            let sign = create_pipeline(
                &self.device,
                self.layouts.sign,
                render_pass,
                vk::PrimitiveTopology::TRIANGLE_STRIP,
                None,
                &modules.sign_vertex,
                &modules.sign_fragment,
                Blend::Premultiplied,
            )?;
            let pumpkin = create_pipeline(
                &self.device,
                self.layouts.pumpkin,
                render_pass,
                vk::PrimitiveTopology::TRIANGLE_STRIP,
                None,
                &modules.pumpkin_vertex,
                &modules.pumpkin_fragment,
                Blend::Premultiplied,
            )?;
            let fade = create_pipeline(
                &self.device,
                self.layouts.common,
                render_pass,
                vk::PrimitiveTopology::TRIANGLE_LIST,
                None,
                &modules.fade_vertex,
                &modules.fade_fragment,
                Blend::Fade,
            )?;
            self.field_pipelines = Some(FieldPipelines { field, glass });
            self.pipelines = Some(Pipelines { sprite, pile, sign, pumpkin, fade });
        }
        Ok(())
    }

    fn allocate_set(&self, layout: vk::DescriptorSetLayout) -> Result<vk::DescriptorSet, String> {
        unsafe {
            self.device
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.descriptor_pool).set_layouts(std::slice::from_ref(&layout)),
                )
                .map_err(|error| error.to_string())
                .map(|sets| sets[0])
        }
    }

    fn ensure_heights(&mut self, bytes: usize) -> Result<(), String> {
        if self.heights.as_ref().is_some_and(|heights| heights.size >= bytes as u64) {
            if self.pile_set.is_none() {
                self.pile_set = Some(self.allocate_set(self.pile_set_layout)?);
                self.write_pile_descriptors();
            }
            return Ok(());
        }
        if let Some(old) = self.heights.take() {
            unsafe { destroy_buffer(&self.device, old) };
        }
        let capacity = bytes.next_power_of_two().max(4096);
        self.heights =
            Some(unsafe { create_buffer(&self.device, &self.instance, self.physical, capacity as u64, vk::BufferUsageFlags::STORAGE_BUFFER)? });
        if self.pile_set.is_none() {
            self.pile_set = Some(self.allocate_set(self.pile_set_layout)?);
        }
        self.write_pile_descriptors();
        Ok(())
    }

    fn write_pile_descriptors(&self) {
        let (Some(heights), Some(set)) = (&self.heights, self.pile_set) else { return };
        let heights_info = vk::DescriptorBufferInfo::default().buffer(heights.buffer).offset(0).range(heights.size);
        let occluders_info = vk::DescriptorBufferInfo::default().buffer(self.occluders.buffer).offset(0).range(self.occluders.size);
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(std::slice::from_ref(&heights_info)),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(std::slice::from_ref(&occluders_info)),
        ];
        unsafe { self.device.update_descriptor_sets(&writes, &[]) };
    }

    fn ensure_occluders(&mut self, values: usize) -> Result<(), String> {
        let needed = (values * size_of::<f32>()).max(64 * OCCLUDERS * 16) as u64;
        if self.occluders.size >= needed {
            return Ok(());
        }
        unsafe {
            let size = needed.next_power_of_two();
            let old = std::mem::replace(
                &mut self.occluders,
                create_buffer(&self.device, &self.instance, self.physical, size, vk::BufferUsageFlags::STORAGE_BUFFER)?,
            );
            destroy_buffer(&self.device, old);
        }
        self.write_pile_descriptors();
        Ok(())
    }

    fn ensure_field(&mut self, surface: &mut Surface) -> Result<(), String> {
        let extent = surface.extent;
        if surface.field.as_ref().is_some_and(|field| field.extent == extent) {
            return Ok(());
        }
        if let Some(field) = surface.field.take() {
            self.destroy_field(field);
        }
        unsafe {
            let texture = create_texture(
                &self.device,
                &self.instance,
                self.physical,
                extent.width,
                extent.height,
                1,
                FIELD_FORMAT,
                vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
            )?;
            let framebuffer = self
                .device
                .create_framebuffer(
                    &vk::FramebufferCreateInfo::default()
                        .render_pass(self.field_render_pass)
                        .attachments(std::slice::from_ref(&texture.view))
                        .width(extent.width)
                        .height(extent.height)
                        .layers(1),
                    None,
                )
                .map_err(|error| format!("field framebuffer: {error}"))?;
            let descriptor = self.allocate_set(self.glass_set_layout)?;
            let image = vk::DescriptorImageInfo::default()
                .sampler(self.nearest_sampler)
                .image_view(texture.view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
            let write = vk::WriteDescriptorSet::default()
                .dst_set(descriptor)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(std::slice::from_ref(&image));
            self.device.update_descriptor_sets(&[write], &[]);
            surface.field = Some(Field { image: texture.image, memory: texture.memory, view: texture.view, framebuffer, descriptor, extent });
        }
        Ok(())
    }

    fn destroy_field(&self, field: Field) {
        unsafe {
            self.device.destroy_framebuffer(field.framebuffer, None);
            self.device.destroy_image_view(field.view, None);
            self.device.destroy_image(field.image, None);
            self.device.free_memory(field.memory, None);
            self.device.free_descriptor_sets(self.descriptor_pool, &[field.descriptor]).ok();
        }
    }

    fn load_sign(&mut self, caption: usize) -> Result<(), String> {
        if self.signs.get(caption).is_none_or(Option::is_some) {
            return Ok(());
        }
        let (texture, [width, height]) = self.picture(SIGNS[caption], false)?;
        let descriptor = self.allocate_set(self.sign_set_layout)?;
        let image = vk::DescriptorImageInfo::default()
            .sampler(self.linear_sampler)
            .image_view(texture.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        let write = vk::WriteDescriptorSet::default()
            .dst_set(descriptor)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(std::slice::from_ref(&image));
        unsafe { self.device.update_descriptor_sets(&[write], &[]) };
        self.signs[caption] = Some(SignTexture { texture, descriptor, size: [width as f32 / 2.0, height as f32 / 2.0] });
        Ok(())
    }

    fn load_pumpkin(&mut self) -> Result<(), String> {
        if self.pumpkin.is_some() {
            return Ok(());
        }
        let (shell, _) = self.picture(PUMPKIN[0], true)?;
        let (face, _) = self.picture(PUMPKIN[1], true)?;
        let descriptor = self.allocate_set(self.pumpkin_set_layout)?;
        let shell_info = vk::DescriptorImageInfo::default()
            .sampler(self.linear_sampler)
            .image_view(shell.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        let face_info = vk::DescriptorImageInfo::default()
            .sampler(self.linear_sampler)
            .image_view(face.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(std::slice::from_ref(&shell_info)),
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(std::slice::from_ref(&face_info)),
        ];
        unsafe { self.device.update_descriptor_sets(&writes, &[]) };
        self.pumpkin = Some(PumpkinTextures { shell, face, descriptor });
        Ok(())
    }

    /// A PNG as a texture, with its colours premultiplied as the blending wants, and its size in pixels.
    fn picture(&self, png: &[u8], mipmapped: bool) -> Result<(Texture, [u32; 2]), String> {
        let picture = image::load_from_memory(png).map_err(|error| format!("picture: {error}"))?.to_rgba8();
        let (width, height) = picture.dimensions();
        let mut pixels = picture.into_raw();
        for pixel in pixels.chunks_exact_mut(4) {
            let alpha = pixel[3] as u32;
            for channel in &mut pixel[..3] {
                *channel = ((*channel as u32 * alpha + 127) / 255) as u8;
            }
        }
        let mip_levels = if mipmapped { (32 - width.max(height).max(1).leading_zeros()).max(1) } else { 1 };
        let texture = unsafe {
            create_texture(
                &self.device,
                &self.instance,
                self.physical,
                width,
                height,
                mip_levels,
                vk::Format::R8G8B8A8_UNORM,
                vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC,
            )?
        };
        if let Err(error) = self.upload_texture(&texture, &pixels, mipmapped) {
            self.destroy_texture(texture);
            return Err(error);
        }
        Ok((texture, [width, height]))
    }

    fn upload_texture(&self, texture: &Texture, pixels: &[u8], generate_mips: bool) -> Result<(), String> {
        unsafe {
            let staging = create_buffer(&self.device, &self.instance, self.physical, pixels.len().max(1) as u64, vk::BufferUsageFlags::TRANSFER_SRC)?;
            staging.write(pixels);
            let command_buffer = self
                .device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.command_pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .map_err(|error| error.to_string())?[0];
            self.device.begin_command_buffer(command_buffer, &vk::CommandBufferBeginInfo::default()).map_err(|error| error.to_string())?;
            image_barrier(
                &self.device,
                command_buffer,
                texture.image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::empty(),
                vk::AccessFlags::TRANSFER_WRITE,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
            );
            let region = vk::BufferImageCopy::default()
                .image_subresource(
                    vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).mip_level(0).base_array_layer(0).layer_count(1),
                )
                .image_extent(vk::Extent3D { width: texture.extent.width, height: texture.extent.height, depth: 1 });
            self.device.cmd_copy_buffer_to_image(command_buffer, staging.buffer, texture.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[region]);
            if generate_mips && texture.mip_levels > 1 {
                image_barrier(
                    &self.device,
                    command_buffer,
                    texture.image,
                    vk::ImageAspectFlags::COLOR,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::GENERAL,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                );
                let mut mip_extent = vk::Extent3D { width: texture.extent.width, height: texture.extent.height, depth: 1 };
                for level in 1..texture.mip_levels {
                    let previous = level - 1;
                    let next_extent = vk::Extent3D { width: (mip_extent.width / 2).max(1), height: (mip_extent.height / 2).max(1), depth: 1 };
                    let src = vk::Offset3D { x: 0, y: 0, z: 0 };
                    let src_end = vk::Offset3D { x: mip_extent.width as i32, y: mip_extent.height as i32, z: 1 };
                    let dst_end = vk::Offset3D { x: next_extent.width as i32, y: next_extent.height as i32, z: 1 };
                    let blit = vk::ImageBlit::default()
                        .src_subresource(
                            vk::ImageSubresourceLayers::default()
                                .aspect_mask(vk::ImageAspectFlags::COLOR)
                                .mip_level(previous)
                                .base_array_layer(0)
                                .layer_count(1),
                        )
                        .src_offsets([src, src_end])
                        .dst_subresource(
                            vk::ImageSubresourceLayers::default()
                                .aspect_mask(vk::ImageAspectFlags::COLOR)
                                .mip_level(level)
                                .base_array_layer(0)
                                .layer_count(1),
                        )
                        .dst_offsets([src, dst_end]);
                    self.device.cmd_blit_image(
                        command_buffer,
                        texture.image,
                        vk::ImageLayout::GENERAL,
                        texture.image,
                        vk::ImageLayout::GENERAL,
                        &[blit],
                        vk::Filter::LINEAR,
                    );
                    if level + 1 < texture.mip_levels {
                        image_barrier(
                            &self.device,
                            command_buffer,
                            texture.image,
                            vk::ImageAspectFlags::COLOR,
                            vk::ImageLayout::GENERAL,
                            vk::ImageLayout::GENERAL,
                            vk::AccessFlags::TRANSFER_WRITE,
                            vk::AccessFlags::TRANSFER_READ,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::PipelineStageFlags::TRANSFER,
                        );
                    }
                    mip_extent = next_extent;
                }
                image_barrier(
                    &self.device,
                    command_buffer,
                    texture.image,
                    vk::ImageAspectFlags::COLOR,
                    vk::ImageLayout::GENERAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::SHADER_READ,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                );
            } else {
                image_barrier(
                    &self.device,
                    command_buffer,
                    texture.image,
                    vk::ImageAspectFlags::COLOR,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::SHADER_READ,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                );
            }
            self.device.end_command_buffer(command_buffer).map_err(|error| error.to_string())?;
            let fence = self.device.create_fence(&vk::FenceCreateInfo::default(), None).map_err(|error| error.to_string())?;
            let command_buffers = [command_buffer];
            let submit = vk::SubmitInfo::default().command_buffers(&command_buffers);
            self.device.queue_submit(self.queue, &[submit], fence).map_err(|error| error.to_string())?;
            self.device.wait_for_fences(&[fence], true, u64::MAX).map_err(|error| error.to_string())?;
            self.device.destroy_fence(fence, None);
            self.device.free_command_buffers(self.command_pool, &[command_buffer]);
            destroy_buffer(&self.device, staging);
        }
        Ok(())
    }

    fn destroy_texture(&self, texture: Texture) {
        unsafe {
            self.device.destroy_image_view(texture.view, None);
            self.device.destroy_image(texture.image, None);
            self.device.free_memory(texture.memory, None);
        }
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            if let Some(heights) = self.heights.take() {
                destroy_buffer(&self.device, heights);
            }
            if let Some(pumpkin) = self.pumpkin.take() {
                self.destroy_texture(pumpkin.shell);
                self.destroy_texture(pumpkin.face);
            }
            let signs = std::mem::take(&mut self.signs);
            for sign in signs.into_iter().flatten() {
                self.destroy_texture(sign.texture);
            }
            destroy_buffer(
                &self.device,
                std::mem::replace(
                    &mut self.occluders,
                    Buffer { buffer: vk::Buffer::null(), memory: vk::DeviceMemory::null(), size: 0, mapped: ptr::null_mut() },
                ),
            );
            if let Some(field_pipelines) = self.field_pipelines.take() {
                self.device.destroy_pipeline(field_pipelines.field, None);
                self.device.destroy_pipeline(field_pipelines.glass, None);
            }
            if let Some(pipelines) = self.pipelines.take() {
                for pipeline in [pipelines.sprite, pipelines.pile, pipelines.sign, pipelines.pumpkin, pipelines.fade] {
                    self.device.destroy_pipeline(pipeline, None);
                }
            }
            for layout in [self.layouts.common, self.layouts.glass, self.layouts.pile, self.layouts.sign, self.layouts.pumpkin] {
                self.device.destroy_pipeline_layout(layout, None);
            }
            for set_layout in [self.common_set_layout, self.glass_set_layout, self.pile_set_layout, self.sign_set_layout, self.pumpkin_set_layout] {
                self.device.destroy_descriptor_set_layout(set_layout, None);
            }
            self.device.destroy_descriptor_pool(self.descriptor_pool, None);
            self.device.destroy_sampler(self.nearest_sampler, None);
            self.device.destroy_sampler(self.linear_sampler, None);
            self.device.destroy_render_pass(self.field_render_pass, None);
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
        let _ = &self.entry;
    }
}

struct ShaderModules {
    device: Device,
    sprite_vertex: vk::ShaderModule,
    sprite_fragment: vk::ShaderModule,
    field_fragment: vk::ShaderModule,
    glass_vertex: vk::ShaderModule,
    glass_fragment: vk::ShaderModule,
    pile_vertex: vk::ShaderModule,
    pile_fragment: vk::ShaderModule,
    sign_vertex: vk::ShaderModule,
    sign_fragment: vk::ShaderModule,
    pumpkin_vertex: vk::ShaderModule,
    pumpkin_fragment: vk::ShaderModule,
    fade_vertex: vk::ShaderModule,
    fade_fragment: vk::ShaderModule,
}

impl ShaderModules {
    fn new(device: &Device) -> Result<ShaderModules, String> {
        Ok(ShaderModules {
            device: device.clone(),
            sprite_vertex: module(device, SPRITE_VERTEX)?,
            sprite_fragment: module(device, SPRITE_FRAGMENT)?,
            field_fragment: module(device, FIELD_FRAGMENT)?,
            glass_vertex: module(device, GLASS_VERTEX)?,
            glass_fragment: module(device, GLASS_FRAGMENT)?,
            pile_vertex: module(device, PILE_VERTEX)?,
            pile_fragment: module(device, PILE_FRAGMENT)?,
            sign_vertex: module(device, SIGN_VERTEX)?,
            sign_fragment: module(device, SIGN_FRAGMENT)?,
            pumpkin_vertex: module(device, PUMPKIN_VERTEX)?,
            pumpkin_fragment: module(device, PUMPKIN_FRAGMENT)?,
            fade_vertex: module(device, FADE_VERTEX)?,
            fade_fragment: module(device, FADE_FRAGMENT)?,
        })
    }
}

impl Drop for ShaderModules {
    fn drop(&mut self) {
        unsafe {
            for module in [
                self.sprite_vertex,
                self.sprite_fragment,
                self.field_fragment,
                self.glass_vertex,
                self.glass_fragment,
                self.pile_vertex,
                self.pile_fragment,
                self.sign_vertex,
                self.sign_fragment,
                self.pumpkin_vertex,
                self.pumpkin_fragment,
                self.fade_vertex,
                self.fade_fragment,
            ] {
                self.device.destroy_shader_module(module, None);
            }
        }
    }
}

fn module(device: &Device, bytes: &[u8]) -> Result<vk::ShaderModule, String> {
    let mut code = Vec::with_capacity(bytes.len() / 4);
    for word in bytes.chunks_exact(4) {
        code.push(u32::from_le_bytes([word[0], word[1], word[2], word[3]]));
    }
    unsafe {
        device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None).map_err(|error| format!("shader module: {error}"))
    }
}

#[derive(Clone, Copy)]
enum Blend {
    Premultiplied,
    Field,
    Glass,
    Fade,
}

struct SpriteInput {
    binding: vk::VertexInputBindingDescription,
    attributes: [vk::VertexInputAttributeDescription; 15],
}

fn sprite_input() -> SpriteInput {
    let attributes = [
        attribute(0, vk::Format::R32G32_SFLOAT, 0),
        attribute(1, vk::Format::R32G32_SFLOAT, 8),
        attribute(2, vk::Format::R32_SFLOAT, 16),
        attribute(3, vk::Format::R32_SFLOAT, 20),
        attribute(4, vk::Format::R32_SFLOAT, 24),
        attribute(5, vk::Format::R32_SFLOAT, 28),
        attribute(6, vk::Format::R32G32B32_SFLOAT, 32),
        attribute(7, vk::Format::R32_SFLOAT, 44),
        attribute(8, vk::Format::R32G32B32A32_SFLOAT, 48),
        attribute(9, vk::Format::R32G32B32A32_SFLOAT, 64),
        attribute(10, vk::Format::R32G32_SFLOAT, 80),
        attribute(11, vk::Format::R32G32B32_SFLOAT, 88),
        attribute(12, vk::Format::R32G32_SFLOAT, 100),
        attribute(13, vk::Format::R32G32_SFLOAT, 108),
        attribute(14, vk::Format::R32G32B32A32_SFLOAT, 116),
    ];
    SpriteInput {
        binding: vk::VertexInputBindingDescription { binding: 0, stride: size_of::<Sprite>() as u32, input_rate: vk::VertexInputRate::INSTANCE },
        attributes,
    }
}

fn attribute(location: u32, format: vk::Format, offset: u32) -> vk::VertexInputAttributeDescription {
    vk::VertexInputAttributeDescription { location, binding: 0, format, offset }
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_pipeline(
    device: &Device,
    layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
    topology: vk::PrimitiveTopology,
    input: Option<SpriteInput>,
    vertex: &vk::ShaderModule,
    fragment: &vk::ShaderModule,
    blend: Blend,
) -> Result<vk::Pipeline, String> {
    unsafe {
        let entry = c"main";
        let stages = [
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(*vertex).name(entry),
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(*fragment).name(entry),
        ];
        let bindings;
        let attributes;
        let vertex_input = if let Some(input) = input {
            bindings = [input.binding];
            attributes = input.attributes;
            vk::PipelineVertexInputStateCreateInfo::default().vertex_binding_descriptions(&bindings).vertex_attribute_descriptions(&attributes)
        } else {
            vk::PipelineVertexInputStateCreateInfo::default()
        };
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(topology);
        let viewport = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let (src_color, dst_color, src_alpha, dst_alpha) = match blend {
            Blend::Premultiplied | Blend::Glass => {
                (vk::BlendFactor::ONE, vk::BlendFactor::ONE_MINUS_SRC_ALPHA, vk::BlendFactor::ONE, vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            }
            Blend::Field => (vk::BlendFactor::ONE, vk::BlendFactor::ONE, vk::BlendFactor::ONE, vk::BlendFactor::ONE),
            Blend::Fade => (vk::BlendFactor::ZERO, vk::BlendFactor::CONSTANT_ALPHA, vk::BlendFactor::ZERO, vk::BlendFactor::CONSTANT_ALPHA),
        };
        let attachment = vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(true)
            .src_color_blend_factor(src_color)
            .dst_color_blend_factor(dst_color)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(src_alpha)
            .dst_alpha_blend_factor(dst_alpha)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA);
        let color_blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(std::slice::from_ref(&attachment));
        let dynamic = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR, vk::DynamicState::BLEND_CONSTANTS];
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
        let pipeline = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic_state)
            .layout(layout)
            .render_pass(render_pass)
            .subpass(0);
        device
            .create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline], None)
            .map_err(|(_, error)| format!("pipeline: {error}"))
            .map(|pipelines| pipelines[0])
    }
}

fn descriptor_layout(device: &Device, bindings: &[vk::DescriptorSetLayoutBinding]) -> Result<vk::DescriptorSetLayout, String> {
    unsafe {
        device
            .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(bindings), None)
            .map_err(|error| format!("descriptor set layout: {error}"))
    }
}

fn binding(binding: u32, ty: vk::DescriptorType, stages: vk::ShaderStageFlags) -> vk::DescriptorSetLayoutBinding<'static> {
    vk::DescriptorSetLayoutBinding::default().binding(binding).descriptor_type(ty).descriptor_count(1).stage_flags(stages)
}

fn pipeline_layout(device: &Device, sets: &[vk::DescriptorSetLayout], push: &[vk::PushConstantRange]) -> Result<vk::PipelineLayout, String> {
    unsafe {
        device
            .create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(sets).push_constant_ranges(push), None)
            .map_err(|error| format!("pipeline layout: {error}"))
    }
}

fn sampler(device: &Device, min: vk::Filter, mag: vk::Filter, max_lod: f32) -> Result<vk::Sampler, String> {
    unsafe {
        device
            .create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(mag)
                    .min_filter(min)
                    .mipmap_mode(if max_lod > 0.0 { vk::SamplerMipmapMode::LINEAR } else { vk::SamplerMipmapMode::NEAREST })
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .max_lod(max_lod),
                None,
            )
            .map_err(|error| format!("sampler: {error}"))
    }
}

fn create_main_render_pass(device: &Device, format: vk::Format) -> Result<vk::RenderPass, String> {
    let attachments = [vk::AttachmentDescription::default()
        .format(format)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(vk::ImageLayout::PRESENT_SRC_KHR)];
    let references = [vk::AttachmentReference::default().attachment(0).layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
    let subpasses = [vk::SubpassDescription::default().pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS).color_attachments(&references)];
    let dependencies = [vk::SubpassDependency::default()
        .src_subpass(vk::SUBPASS_EXTERNAL)
        .dst_subpass(0)
        .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)];
    unsafe {
        device
            .create_render_pass(
                &vk::RenderPassCreateInfo::default().attachments(&attachments).subpasses(&subpasses).dependencies(&dependencies),
                None,
            )
            .map_err(|error| format!("render pass: {error}"))
    }
}

fn create_field_render_pass(device: &Device) -> Result<vk::RenderPass, String> {
    let attachments = [vk::AttachmentDescription::default()
        .format(FIELD_FORMAT)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
    let references = [vk::AttachmentReference::default().attachment(0).layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
    let subpasses = [vk::SubpassDescription::default().pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS).color_attachments(&references)];
    let dependencies = [vk::SubpassDependency::default()
        .src_subpass(vk::SUBPASS_EXTERNAL)
        .dst_subpass(0)
        .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)];
    unsafe {
        device
            .create_render_pass(
                &vk::RenderPassCreateInfo::default().attachments(&attachments).subpasses(&subpasses).dependencies(&dependencies),
                None,
            )
            .map_err(|error| format!("field render pass: {error}"))
    }
}

fn create_image_view(
    device: &Device,
    image: vk::Image,
    format: vk::Format,
    aspect: vk::ImageAspectFlags,
    mip_levels: u32,
) -> Result<vk::ImageView, String> {
    unsafe {
        device
            .create_image_view(
                &vk::ImageViewCreateInfo::default().image(image).view_type(vk::ImageViewType::TYPE_2D).format(format).subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(aspect)
                        .base_mip_level(0)
                        .level_count(mip_levels)
                        .base_array_layer(0)
                        .layer_count(1),
                ),
                None,
            )
            .map_err(|error| format!("image view: {error}"))
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_texture(
    device: &Device,
    instance: &Instance,
    physical: vk::PhysicalDevice,
    width: u32,
    height: u32,
    mip_levels: u32,
    format: vk::Format,
    usage: vk::ImageUsageFlags,
) -> Result<Texture, String> {
    unsafe {
        let extent = vk::Extent2D { width: width.max(1), height: height.max(1) };
        let image = device
            .create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format)
                    .extent(vk::Extent3D { width: extent.width, height: extent.height, depth: 1 })
                    .mip_levels(mip_levels)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )
            .map_err(|error| format!("image: {error}"))?;
        let requirements = device.get_image_memory_requirements(image);
        let memory_type = memory_type(instance, physical, requirements.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?;
        let memory = match device
            .allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(requirements.size).memory_type_index(memory_type), None)
        {
            Ok(memory) => memory,
            Err(error) => {
                device.destroy_image(image, None);
                return Err(format!("image memory: {error}"));
            }
        };
        if let Err(error) = device.bind_image_memory(image, memory, 0) {
            device.destroy_image(image, None);
            device.free_memory(memory, None);
            return Err(format!("binding image memory: {error}"));
        }
        let view = create_image_view(device, image, format, vk::ImageAspectFlags::COLOR, mip_levels)?;
        Ok(Texture { image, memory, view, extent, mip_levels })
    }
}

unsafe fn create_buffer(
    device: &Device,
    instance: &Instance,
    physical: vk::PhysicalDevice,
    size: u64,
    usage: vk::BufferUsageFlags,
) -> Result<Buffer, String> {
    unsafe {
        let size = size.max(1);
        let buffer = device
            .create_buffer(&vk::BufferCreateInfo::default().size(size).usage(usage).sharing_mode(vk::SharingMode::EXCLUSIVE), None)
            .map_err(|error| format!("buffer: {error}"))?;
        let requirements = device.get_buffer_memory_requirements(buffer);
        let wanted = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let (memory_type, _) =
            memory_type_with(instance, physical, requirements.memory_type_bits, wanted).ok_or("no host-visible coherent memory for a buffer")?;
        let memory = match device
            .allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(requirements.size).memory_type_index(memory_type), None)
        {
            Ok(memory) => memory,
            Err(error) => {
                device.destroy_buffer(buffer, None);
                return Err(format!("buffer memory: {error}"));
            }
        };
        if let Err(error) = device.bind_buffer_memory(buffer, memory, 0) {
            device.destroy_buffer(buffer, None);
            device.free_memory(memory, None);
            return Err(format!("binding buffer memory: {error}"));
        }
        let mapped = device.map_memory(memory, 0, size, vk::MemoryMapFlags::empty()).map_err(|error| format!("mapping buffer: {error}"))? as *mut u8;
        Ok(Buffer { buffer, memory, size, mapped })
    }
}

fn memory_type(instance: &Instance, physical: vk::PhysicalDevice, bits: u32, wanted: vk::MemoryPropertyFlags) -> Result<u32, String> {
    memory_type_with(instance, physical, bits, wanted).map(|(index, _)| index).ok_or_else(|| "no suitable Vulkan memory type".into())
}

fn memory_type_with(
    instance: &Instance,
    physical: vk::PhysicalDevice,
    bits: u32,
    wanted: vk::MemoryPropertyFlags,
) -> Option<(u32, vk::MemoryPropertyFlags)> {
    let properties = unsafe { instance.get_physical_device_memory_properties(physical) };
    for index in 0..properties.memory_type_count {
        let flags = properties.memory_types[index as usize].property_flags;
        if bits & (1 << index) != 0 && flags.contains(wanted) {
            return Some((index, flags));
        }
    }
    None
}

unsafe fn destroy_buffer(device: &Device, buffer: Buffer) {
    unsafe {
        if buffer.buffer != vk::Buffer::null() {
            if !buffer.mapped.is_null() {
                device.unmap_memory(buffer.memory);
            }
            device.destroy_buffer(buffer.buffer, None);
            device.free_memory(buffer.memory, None);
        }
    }
}

fn write_buffer(device: &Device, set: vk::DescriptorSet, binding: u32, buffer: vk::Buffer, size: u64) {
    let info = vk::DescriptorBufferInfo::default().buffer(buffer).offset(0).range(size);
    let write = vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
        .buffer_info(std::slice::from_ref(&info));
    unsafe { device.update_descriptor_sets(&[write], &[]) };
}

#[allow(clippy::too_many_arguments)]
unsafe fn image_barrier(
    device: &Device,
    command_buffer: vk::CommandBuffer,
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    src_access: vk::AccessFlags,
    dst_access: vk::AccessFlags,
    src_stage: vk::PipelineStageFlags,
    dst_stage: vk::PipelineStageFlags,
) {
    unsafe {
        let barrier = vk::ImageMemoryBarrier::default()
            .old_layout(old_layout)
            .new_layout(new_layout)
            .src_access_mask(src_access)
            .dst_access_mask(dst_access)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(aspect)
                    .base_mip_level(0)
                    .level_count(vk::REMAINING_MIP_LEVELS)
                    .base_array_layer(0)
                    .layer_count(1),
            );
        device.cmd_pipeline_barrier(command_buffer, src_stage, dst_stage, vk::DependencyFlags::empty(), &[], &[], &[barrier]);
    }
}

fn push_constants<T: Copy>(device: &Device, command_buffer: vk::CommandBuffer, layout: vk::PipelineLayout, value: &T) {
    let bytes = unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) };
    unsafe {
        device.cmd_push_constants(command_buffer, layout, vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT, 0, bytes);
    }
}

impl Buffer {
    fn write(&self, data: &[u8]) {
        if data.is_empty() || self.mapped.is_null() {
            return;
        }
        let count = data.len().min(self.size as usize);
        unsafe {
            ptr::copy_nonoverlapping(data.as_ptr(), self.mapped, count);
        }
    }
}

/// Where a pumpkin's candle burns, in the simulation's points.
fn wick(pumpkin: &PumpkinSprite) -> V2 {
    let [across, up] = PUMPKIN_CANDLE;
    pumpkin.anchor + V2::new((across - 0.5) * pumpkin.width, up * pumpkin.width * PUMPKIN_ASPECT)
}

/// Plain numbers as bytes.
fn bytes<T: Copy>(values: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values)) }
}

fn layer_name(properties: &vk::LayerProperties) -> &CStr {
    unsafe { CStr::from_ptr(properties.layer_name.as_ptr()) }
}

fn device_name(properties: &vk::PhysicalDeviceProperties) -> String {
    let name = unsafe { CStr::from_ptr(properties.device_name.as_ptr()) };
    name.to_string_lossy().into_owned()
}
