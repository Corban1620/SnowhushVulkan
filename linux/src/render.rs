//! Draws the snow with Vulkan (via wgpu).
//! It handles the pile rendering from a storage buffer, and every flake, grain, and puff as an instanced quad.


use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::Proxy;
use wgpu::{
    Instance, InstanceDescriptor, Backends, Device, Queue, TextureFormat,
    RenderPipeline, Buffer, BindGroup,
};

use snowhush_core::simulation::{SnowSimulation, Sprite};

const SPRITE_WGSL: &str = include_str!("shaders/sprite.wgsl");
const PILE_WGSL: &str = include_str!("shaders/pile.wgsl");

/// The light the snow is drawn in.
#[derive(Clone, Copy)]
pub struct Ambient {
    pub light: [f32; 3],
    pub shadow_tint: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct SpriteUniforms {
    view_size: [f32; 2],
    scale: f32,
    time: f32,
    light: [f32; 4],
    panel: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct PileUniforms {
    view_size: [f32; 2],
    scale: f32,
    time: f32,
    column_width: f32,
    column_count: u32,
    occluder_count: u32,
    pile_top: f32,
    pile_bottom: f32,
    origin: [f32; 2],
    height_offset: u32,
    light: [f32; 4],
    shadow_tint: [f32; 4],
    panel: [f32; 4],
    occluders: [[f32; 4]; 16],
}

pub struct Gpu {
    instance: Instance,
    pub adapter: String,
    device: Device,
    display_ptr: *mut std::ffi::c_void,
    queue: Queue,

    sprite_pipeline: RenderPipeline,
    sprite_uniform_buffer: Buffer,
    sprite_bind_group_layout: wgpu::BindGroupLayout,
    sprite_bind_group: BindGroup,

    compute_pipeline: wgpu::ComputePipeline,
    particle_buffer: Buffer,
    physics_bind_group: BindGroup,
    pile_pipeline: RenderPipeline,
    pile_uniform_buffer: Buffer,
    pile_height_buffer: Buffer,
    pile_bind_group_layout: wgpu::BindGroupLayout,

    sprites: Vec<Sprite>,
    columns: Vec<f32>,
    format: TextureFormat,
}

pub struct Surface {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pub width: i32,
    pub height: i32,
}

impl Gpu {
    pub fn new(display: *mut std::ffi::c_void) -> Result<Gpu, String> {
        let instance = Instance::new(InstanceDescriptor {
            backends: Backends::VULKAN,
            ..Default::default()
        });

        // WGPU requires a valid window surface to request the correct adapter if we want guaranteed support
        // But we are dealing with Wayland raw surface creation per `WlSurface`.
        // We can request a headless/primary adapter first.
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok_or("Failed to find wgpu adapter")?;

        let adapter_info = adapter.get_info();
        let adapter_name = adapter_info.name.clone();

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: None,
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
            },
            None,
        ))
        .map_err(|e| format!("WGPU Device request failed: {e}"))?;

        let format = TextureFormat::Bgra8Unorm; // Wayland typical

        // Shaders
        let sprite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Sprite Shader"),
            source: wgpu::ShaderSource::Wgsl(SPRITE_WGSL.into()),
        });

        let pile_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Pile Shader"),
            source: wgpu::ShaderSource::Wgsl(PILE_WGSL.into()),
        });

        // Uniforms
        let sprite_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Sprite Uniforms"),
            size: std::mem::size_of::<SpriteUniforms>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sprite_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Sprite Bind Group Layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let sprite_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Sprite Bind Group"),
            layout: &sprite_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: sprite_uniform_buffer.as_entire_binding(),
            }],
        });

        let pile_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Pile Uniforms"),
            size: std::mem::size_of::<PileUniforms>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let pile_height_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Pile Heights Buffer"),
            size: (4096 * 10 * 4) as wgpu::BufferAddress, // Arbitrary starting size
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let pile_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Pile Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let sprite_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Sprite Pipeline Layout"),
            bind_group_layouts: &[&sprite_bind_group_layout],
            push_constant_ranges: &[],
        });

        let sprite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Sprite Pipeline"),
            layout: Some(&sprite_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &sprite_shader,
                entry_point: "vs_main",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Sprite>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,
                        1 => Float32x2,
                        2 => Float32,
                        3 => Float32,
                        4 => Float32,
                        5 => Float32,
                        6 => Float32x3,
                        7 => Float32
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &sprite_shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });


        let compute_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Physics Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/physics.wgsl").into()),
        });

        let physics_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Physics Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let compute_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Compute Pipeline Layout"),
            bind_group_layouts: &[&physics_bind_group_layout],
            push_constant_ranges: &[],
        });

        let compute_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Physics Compute Pipeline"),
            layout: Some(&compute_pipeline_layout),
            module: &compute_shader,
            entry_point: "update_flakes",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });


        let particle_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Particle Storage Buffer"),
            size: (15000 * std::mem::size_of::<Sprite>()) as wgpu::BufferAddress, // Max flakes supported
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Add this to the physics bind group layout
        let physics_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Physics Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let physics_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Physics Bind Group"),
            layout: &physics_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: sprite_uniform_buffer.as_entire_binding(), // share uniforms for now
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: particle_buffer.as_entire_binding(),
                },
            ],
        });

        let pile_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Pile Pipeline Layout"),
            bind_group_layouts: &[&pile_bind_group_layout],
            push_constant_ranges: &[],
        });

        let pile_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Pile Pipeline"),
            layout: Some(&pile_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &pile_shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &pile_shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        Ok(Gpu {
            instance,
            adapter: adapter_name,
            device,
            queue,
            sprite_pipeline,
            sprite_uniform_buffer,
            sprite_bind_group_layout,
            sprite_bind_group,
            compute_pipeline,
            particle_buffer,
            physics_bind_group,
            pile_pipeline,
            pile_uniform_buffer,
            pile_height_buffer,
            pile_bind_group_layout,
            sprites: Vec::new(),
            columns: Vec::new(),
            format,
            display_ptr: display,
        })
    }

    pub fn surface(&self, surface: &WlSurface, width: i32, height: i32) -> Result<Surface, String> {
        use raw_window_handle::{WaylandDisplayHandle, WaylandWindowHandle, RawDisplayHandle, RawWindowHandle};
        use std::ptr::NonNull;

        let display_ptr = self.display_ptr;

        // Use WlEglSurface to extract the raw pointer easily
        let egl_surface = wayland_egl::WlEglSurface::new(surface.id(), width.max(1), height.max(1)).map_err(|e| e.to_string())?;
        let surface_ptr = egl_surface.ptr() as *mut std::ffi::c_void;

        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                NonNull::new(display_ptr).unwrap()
            )),
            raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(
                NonNull::new(surface_ptr).unwrap()
            )),
        };

        let surface_wgpu = unsafe { self.instance.create_surface_unsafe(target) }.map_err(|e| e.to_string())?;

        let width_u32 = width.max(1) as u32;
        let height_u32 = height.max(1) as u32;

        let adapter = pollster::block_on(self.instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface_wgpu),
        })).unwrap();

        let config = surface_wgpu.get_default_config(&adapter, width_u32, height_u32).unwrap_or(
            wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.format,
                width: width_u32,
                height: height_u32,
                present_mode: wgpu::PresentMode::Fifo,
                alpha_mode: wgpu::CompositeAlphaMode::PreMultiplied,
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            }
        );
        surface_wgpu.configure(&self.device, &config);

        Ok(Surface { surface: surface_wgpu, width, height, config })
    }
}

impl Gpu {
    pub fn resize(&self, surface: &mut Surface, width: i32, height: i32) {
        surface.width = width.max(1);
        surface.height = height.max(1);
        surface.config.width = surface.width as u32;
        surface.config.height = surface.height as u32;
        surface.surface.configure(&self.device, &surface.config);
    }

    pub fn release(&self, _surface: Surface) {
        // dropped
    }

    pub fn clear(&self, surface: &Surface) -> Result<(), String> {
        let frame = surface.surface.get_current_texture().map_err(|e| e.to_string())?;
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }

    pub fn draw(&mut self, surface: &Surface, simulation: &SnowSimulation, ambient: Ambient, panel: Option<[f32; 4]>, time: f32) -> Result<(), String> {
        let frame = surface.surface.get_current_texture().map_err(|e| e.to_string())?;
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());

        let sprite_count = simulation.sprite_count();
        self.sprites.resize(sprite_count, Sprite::default());
        let _sprites = simulation.write_sprites(&mut self.sprites);

        // Update sprite uniforms if needed
        let view_size = [simulation.width(), simulation.height()];
        let light = [ambient.light[0], ambient.light[1], ambient.light[2], 1.0];
        let panel_rect = panel.unwrap_or([0.0; 4]);

        let sprite_uniforms = SpriteUniforms {
            view_size,
            scale: 1.0,
            time,
            light,
            panel: panel_rect,
        };

        self.queue.write_buffer(&self.sprite_uniform_buffer, 0, bytemuck::bytes_of(&sprite_uniforms));

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let _render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // TODO: Compute pass integration happens here for Phase 3!

            // For now, render passes
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }
}
