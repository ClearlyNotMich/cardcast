//! Draws the captured frames on the mirror's surface with wgpu on Vulkan
//! (docs/research/linux-support.md, sections 1 and 2): a DMA-BUF frame is imported and copied on
//! the graphics card, a frame in memory uploaded; either is kept and drawn aspect-fitted at each
//! of the card display's refreshes (Fifo), so a still desktop keeps its picture.

use super::super::{Ended, Frame, Pixels, fit};
use super::screen::Screen;
use super::{ABGR8888, ARGB8888, XBGR8888, XRGB8888, rows};
use ash::vk;
use std::os::fd::OwnedFd;
use wgpu::hal::api::Vulkan;

/// What became of a frame handed to [`Gpu::take`].
pub enum Taken {
    Yes,
    /// The DMA-BUF frame could not be imported; shared memory still can.
    NotImported(String),
    Failed(Ended),
}

/// The newest picture, kept on the graphics card.
struct Kept {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    size: (u32, u32),
}

pub struct Gpu {
    // The surface goes before the device and instance it was made with.
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    /// The surface's format without sRGB encoding: the captured bytes are sRGB already.
    view_format: wgpu::TextureFormat,
    kept: Option<Kept>,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    queue: wgpu::Queue,
    device: wgpu::Device,
    adapter: wgpu::Adapter,
    _instance: wgpu::Instance,
}

pub const SHADER: &str = r"
@group(0) @binding(0) var picture: texture_2d<f32>;
@group(0) @binding(1) var filtering: sampler;

struct Corner {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One triangle over the whole viewport, which is the fitted rectangle.
@vertex
fn vertex(@builtin(vertex_index) index: u32) -> Corner {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var corner: Corner;
    corner.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    corner.uv = uv;
    return corner;
}

// The capture's alpha may be undefined (XRGB), so the picture is opaque.
@fragment
fn fragment(corner: Corner) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(picture, filtering, corner.uv).rgb, 1.0);
}
";

/// The texture format of a frame's DRM fourcc; the alpha byte, if any, is not shown.
pub fn texture_format(fourcc: u32) -> Option<wgpu::TextureFormat> {
    match fourcc {
        XRGB8888 | ARGB8888 => Some(wgpu::TextureFormat::Bgra8Unorm),
        XBGR8888 | ABGR8888 => Some(wgpu::TextureFormat::Rgba8Unorm),
        _ => None,
    }
}

/// The DMA-BUF layouts cardcast can import and copy from: single-plane, copyable modifiers.
pub fn importable(properties: &[vk::DrmFormatModifierPropertiesEXT]) -> Vec<u64> {
    properties
        .iter()
        .filter(|modifier| {
            modifier.drm_format_modifier_plane_count == 1
                && modifier
                    .drm_format_modifier_tiling_features
                    .contains(vk::FormatFeatureFlags::TRANSFER_SRC)
        })
        .map(|modifier| modifier.drm_format_modifier)
        .collect()
}

/// The output of a future that is ready at once, as wgpu's native ones are; `None` otherwise.
fn ready<T>(future: impl Future<Output = T>) -> Option<T> {
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match std::pin::pin!(future).poll(&mut context) {
        std::task::Poll::Ready(output) => Some(output),
        std::task::Poll::Pending => None,
    }
}

impl Gpu {
    /// A Vulkan device that can present to `screen`, which must outlive it.
    pub fn open(screen: &Screen) -> Result<Self, Ended> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let (display, window) = screen.handles();
        // SAFETY: the handles are the screen's live display connection and surface; the mirror
        // thread drops this Gpu, and so the surface, before the screen.
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(display),
                raw_window_handle: window,
            })
        }
        .map_err(|error| {
            Ended::Stop(format!(
                "The mirror needs Vulkan: install your graphics card's Vulkan driver. ({error})"
            ))
        })?;
        let adapter = ready(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .ok_or_else(|| Ended::Stop("The graphics card did not answer.".into()))?
        .map_err(|error| {
            Ended::Stop(format!(
                "The mirror needs Vulkan: install your graphics card's Vulkan driver. ({error})"
            ))
        })?;
        let (device, queue) = ready(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("cardcast mirror"),
            required_features: adapter.features() & wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF,
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .ok_or_else(|| Ended::Restart("The graphics card did not answer.".into()))?
        .map_err(|error| {
            Ended::Restart(format!(
                "Could not start the mirror on the graphics card. ({error})"
            ))
        })?;
        let (width, height) = screen.size();
        let mut config = surface
            .get_default_config(&adapter, width, height)
            .ok_or_else(|| {
                Ended::Stop("The graphics card can't draw on the card display.".into())
            })?;
        let view_format = config.format.remove_srgb_suffix();
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 1;
        if view_format != config.format {
            config.view_formats = vec![view_format];
        }
        surface.configure(&device, &config);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(view_format.into())],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Self {
            surface,
            config,
            view_format,
            kept: None,
            pipeline,
            layout,
            sampler,
            queue,
            device,
            adapter,
            _instance: instance,
        })
    }

    /// The DMA-BUF modifiers this device imports in all four formats; none without the extensions.
    pub fn modifiers(&self) -> Vec<u64> {
        if !self
            .device
            .features()
            .contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF)
        {
            return Vec::new();
        }
        // SAFETY: the adapter is Vulkan (the instance has no other backend); only queried here.
        let Some(adapter) = (unsafe { self.adapter.as_hal::<Vulkan>() }) else {
            return Vec::new();
        };
        let instance = adapter.shared_instance().raw_instance();
        let listed = |format| {
            let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
            // SAFETY: a plain query on the adapter's own physical device; `list` outlives it.
            unsafe {
                instance.get_physical_device_format_properties2(
                    adapter.raw_physical_device(),
                    format,
                    &mut vk::FormatProperties2::default().push_next(&mut list),
                );
            }
            let count = usize::try_from(list.drm_format_modifier_count).unwrap_or(0);
            let mut properties = vec![vk::DrmFormatModifierPropertiesEXT::default(); count];
            let mut list = vk::DrmFormatModifierPropertiesListEXT::default()
                .drm_format_modifier_properties(&mut properties);
            // SAFETY: as above; `properties` has room for the count the first call gave.
            unsafe {
                instance.get_physical_device_format_properties2(
                    adapter.raw_physical_device(),
                    format,
                    &mut vk::FormatProperties2::default().push_next(&mut list),
                );
            }
            let count = usize::try_from(list.drm_format_modifier_count).unwrap_or(0);
            properties.truncate(count);
            importable(&properties)
        };
        let rgba = listed(vk::Format::R8G8B8A8_UNORM);
        listed(vk::Format::B8G8R8A8_UNORM)
            .into_iter()
            .filter(|modifier| rgba.contains(modifier))
            .collect()
    }

    /// Keeps `frame` as the picture to draw. A DMA-BUF frame is copied, and given back to the
    /// capture once the copy is done.
    pub fn take(&mut self, frame: Frame) -> Taken {
        let Some(format) = texture_format(frame.fourcc) else {
            return Taken::Failed(Ended::Stop(format!(
                "The screen capture sends pixels cardcast can't show. (fourcc {:08X})",
                frame.fourcc
            )));
        };
        if frame.width == 0 || frame.height == 0 {
            return Taken::Yes;
        }
        let size = (frame.width, frame.height);
        let texture = self.kept(size, format);
        let extent = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        };
        match frame.pixels {
            Pixels::Memory { bytes, stride } => {
                let Some(bytes) = rows(&bytes, 0, stride, size.1) else {
                    return Taken::Failed(Ended::Restart(
                        "The screen capture sent a damaged picture; restarting it.".into(),
                    ));
                };
                self.queue.write_texture(
                    texture.as_image_copy(),
                    bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(stride),
                        rows_per_image: Some(size.1),
                    },
                    extent,
                );
                Taken::Yes
            }
            Pixels::DmaBuf {
                fd,
                offset,
                stride,
                modifier,
            } => match self.import(fd, (offset, stride, modifier), extent, format, &texture) {
                Ok(()) => Taken::Yes,
                Err(why) => Taken::NotImported(why),
            },
        }
        // `frame.release` is dropped here, after the copy.
    }

    /// The kept texture for a picture of `size` and `format`, made anew when either changes.
    fn kept(&mut self, size: (u32, u32), format: wgpu::TextureFormat) -> wgpu::Texture {
        if let Some(kept) = &self.kept
            && kept.size == size
            && kept.texture.format() == format
        {
            return kept.texture.clone();
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cardcast picture"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.kept = Some(Kept {
            texture: texture.clone(),
            bind_group,
            size,
        });
        texture
    }

    /// Imports the DMA-BUF plane `fd` (`offset`, `stride`, `modifier`) and copies it into
    /// `texture`, waiting until the copy is done so the capture can have its buffer back.
    fn import(
        &self,
        fd: OwnedFd,
        (offset, stride, modifier): (u32, u32, u64),
        extent: wgpu::Extent3d,
        format: wgpu::TextureFormat,
        texture: &wgpu::Texture,
    ) -> Result<(), String> {
        let hal_descriptor = wgpu::hal::TextureDescriptor {
            label: Some("captured DMA-BUF"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUses::COPY_SRC,
            memory_flags: wgpu::hal::MemoryFlags::empty(),
            view_formats: Vec::new(),
        };
        let imported = {
            // SAFETY: the device is Vulkan (the instance has no other backend).
            let device = unsafe { self.device.as_hal::<Vulkan>() }.ok_or("not a Vulkan device")?;
            // SAFETY: the device has VULKAN_EXTERNAL_MEMORY_DMA_BUF (`modifiers` offered none
            // without it), and `fd`, `modifier`, `stride` and `offset` describe the single plane
            // as PipeWire gave it, in the size and format of `hal_descriptor`.
            unsafe {
                device.texture_from_dmabuf_fd(
                    fd,
                    &hal_descriptor,
                    modifier,
                    stride.into(),
                    offset.into(),
                )
            }
            .map_err(|error| error.to_string())?
        };
        // SAFETY: `imported` was made on this device from `hal_descriptor`, which this matches; its
        // memory holds the compositor's picture, in no layout wgpu knows (so UNINITIALIZED).
        let imported = unsafe {
            self.device.create_texture_from_hal::<Vulkan>(
                imported,
                &wgpu::TextureDescriptor {
                    label: Some("captured DMA-BUF"),
                    size: extent,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                },
                wgpu::TextureUses::UNINITIALIZED,
            )
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_texture(imported.as_image_copy(), texture.as_image_copy(), extent);
        let submitted = self.queue.submit([encoder.finish()]);
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submitted),
                timeout: None,
            })
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Draws the kept picture, aspect-fitted on black, and presents it at the card display's next
    /// refresh. False if there was nothing to present yet, or the compositor skipped this one.
    pub fn present(&mut self) -> Result<bool, Ended> {
        let Some(kept) = &self.kept else {
            return Ok(false);
        };
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output)
            | wgpu::CurrentSurfaceTexture::Suboptimal(output) => output,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Validation => {
                return Err(Ended::Restart(
                    "The mirror lost its picture on the card display; restarting it.".into(),
                ));
            }
        };
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.view_format),
            ..Default::default()
        });
        let fitted = fit(kept.size, (self.config.width, self.config.height));
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if fitted.width > 0 && fitted.height > 0 {
                pass.set_viewport(
                    fitted.left as f32,
                    fitted.top as f32,
                    fitted.width as f32,
                    fitted.height as f32,
                    0.0,
                    1.0,
                );
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &kept.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(output);
        Ok(true)
    }
}
