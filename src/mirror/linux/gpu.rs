//! Draws the captured frames on the mirror's surface with wgpu on Vulkan
//! (docs/research/linux-support.md, sections 1 and 2): a DMA-BUF frame is imported and copied on
//! the graphics card, a frame in memory uploaded; either is kept and drawn aspect-fitted at each
//! of the card display's refreshes (Fifo), so a still desktop keeps its picture.

use super::super::{Ended, Frame, Pixels, fit};
use super::screen::Screen;
use super::{ABGR8888, ARGB8888, XBGR8888, XRGB8888, rows};
use ash::vk;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::OnceLock;
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
    /// With DMA-BUF import: the raw device and its queue family, to take each imported plane
    /// over from the compositor (`VK_QUEUE_FAMILY_FOREIGN_EXT`) and give it back.
    foreign: Option<(ash::Device, u32)>,
    // The surface goes before the device it was made with.
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

/// `DMA_BUF_IOCTL_EXPORT_SYNC_FILE` (linux/dma-buf.h, Linux 6.0): `_IOWR('b', 2, struct
/// dma_buf_export_sync_file { __u32 flags; __s32 fd; })`.
pub const DMA_BUF_IOCTL_EXPORT_SYNC_FILE: u64 = (3 << 30) | (8 << 16) | ((b'b' as u64) << 8) | 2;
/// `DMA_BUF_SYNC_READ`: a sync file of the writes a reader must wait for.
const DMA_BUF_SYNC_READ: u32 = 1;

/// Waits up to a refresh for the writes to the DMA-BUF `fd`: on its exported sync file, or on
/// older kernels on the DMA-BUF itself, whose `POLLIN` waits for its writers (the kernel's
/// dma-buf "Implicit Fence Poll Support"). False if they didn't finish in time.
fn wait_for_writes(fd: std::os::fd::BorrowedFd) -> Result<bool, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    #[repr(C)]
    struct ExportSyncFile {
        flags: u32,
        fd: i32,
    }
    let mut export = ExportSyncFile {
        flags: DMA_BUF_SYNC_READ,
        fd: -1,
    };
    // SAFETY: `fd` is an open DMA-BUF and `export` the struct this ioctl takes; on success the
    // kernel puts a new sync file descriptor in it, owned below.
    let exported = unsafe {
        libc::ioctl(
            fd.as_raw_fd(),
            DMA_BUF_IOCTL_EXPORT_SYNC_FILE as libc::c_ulong,
            &raw mut export,
        )
    } == 0;
    // SAFETY: on success `export.fd` is a sync file this call owns.
    let sync_file = exported.then(|| unsafe { OwnedFd::from_raw_fd(export.fd) });
    let mut waited = libc::pollfd {
        fd: sync_file
            .as_ref()
            .map_or(fd.as_raw_fd(), AsRawFd::as_raw_fd),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd, for a descriptor open for the whole call.
    match unsafe { libc::poll(&raw mut waited, 1, 100) } {
        -1 => Err(std::io::Error::last_os_error().to_string()),
        0 => Ok(false),
        _ => Ok(true),
    }
}

/// The barrier, with its source and destination stages, that takes the image of an imported
/// DMA-BUF over from the foreign queue family to `family` to copy from (`acquire`), or gives it
/// back after the copy, as wlroots does (`render/vulkan/pass.c`). The layout outside is `GENERAL`.
pub fn foreign_barrier(
    image: vk::Image,
    family: u32,
    acquire: bool,
) -> (vk::ImageMemoryBarrier<'static>, [vk::PipelineStageFlags; 2]) {
    let barrier = vk::ImageMemoryBarrier::default()
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1),
        );
    if acquire {
        (
            barrier
                .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                .dst_queue_family_index(family)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ),
            [
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
            ],
        )
    } else {
        (
            barrier
                .src_queue_family_index(family)
                .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_access_mask(vk::AccessFlags::TRANSFER_READ),
            [
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            ],
        )
    }
}

/// The mirror's Vulkan instance, made once and never destroyed (#60): destroying a second
/// `VkInstance` broke the settings window's own Vulkan swapchain on NVIDIA's driver, so its next
/// paint after Stop jumped to address 0 inside `libnvidia-glcore`.
fn instance() -> &'static wgpu::Instance {
    static INSTANCE: OnceLock<wgpu::Instance> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        })
    })
}

/// Whether the adapter's device has `VK_EXT_queue_family_foreign`, which DMA-BUF import needs to
/// take planes over from the compositor; without it cardcast asks for shared memory.
fn has_queue_family_foreign(adapter: &wgpu::Adapter) -> bool {
    // SAFETY: the adapter is Vulkan (the instance has no other backend); only queried here.
    let Some(adapter) = (unsafe { adapter.as_hal::<Vulkan>() }) else {
        return false;
    };
    // SAFETY: a plain query on the adapter's own physical device.
    let Ok(extensions) = (unsafe {
        adapter
            .shared_instance()
            .raw_instance()
            .enumerate_device_extension_properties(adapter.raw_physical_device())
    }) else {
        return false;
    };
    extensions.iter().any(|extension| {
        extension.extension_name_as_c_str() == Ok(ash::ext::queue_family_foreign::NAME)
    })
}

/// Opens the device as wgpu would, plus `VK_EXT_queue_family_foreign`.
fn open_with_queue_family_foreign(
    adapter: &wgpu::Adapter,
    descriptor: &wgpu::DeviceDescriptor,
) -> Result<(wgpu::Device, wgpu::Queue), String> {
    let opened = {
        // SAFETY: the adapter is Vulkan (the instance has no other backend).
        let hal = unsafe { adapter.as_hal::<Vulkan>() }.ok_or("not a Vulkan adapter")?;
        // SAFETY: the callback only adds an extension the device lists (`has_queue_family_foreign`).
        unsafe {
            hal.open_with_callback(
                descriptor.required_features,
                &descriptor.required_limits,
                &descriptor.memory_hints,
                Some(Box::new(|arguments| {
                    arguments
                        .extensions
                        .push(ash::ext::queue_family_foreign::NAME);
                })),
            )
        }
        .map_err(|error| error.to_string())?
    };
    // SAFETY: `opened` came from this adapter, with exactly the descriptor's features.
    unsafe { adapter.create_device_from_hal(opened, descriptor) }.map_err(|error| error.to_string())
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
        let instance = instance();
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
        let dma_buf = adapter
            .features()
            .contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF)
            && has_queue_family_foreign(&adapter);
        let descriptor = wgpu::DeviceDescriptor {
            label: Some("cardcast mirror"),
            required_features: if dma_buf {
                wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF
            } else {
                wgpu::Features::empty()
            },
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        };
        let opened = if dma_buf {
            open_with_queue_family_foreign(&adapter, &descriptor)
        } else {
            ready(adapter.request_device(&descriptor))
                .ok_or_else(|| "the graphics card did not answer".to_owned())
                .and_then(|opened| opened.map_err(|error| error.to_string()))
        };
        let (device, queue) = opened.map_err(|error| {
            Ended::Restart(format!(
                "Could not start the mirror on the graphics card. ({error})"
            ))
        })?;
        let foreign = dma_buf
            .then(|| {
                // SAFETY: the device is Vulkan (the instance has no other backend); its ash handle
                // is cloned and used only while `device` lives (it is dropped with this Gpu).
                let hal = unsafe { device.as_hal::<Vulkan>() }?;
                Some((hal.raw_device().clone(), hal.queue_family_index()))
            })
            .flatten();
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
            foreign,
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
        })
    }

    /// The DMA-BUF modifiers this device imports in all four formats; none without the extensions.
    pub fn modifiers(&self) -> Vec<u64> {
        if self.foreign.is_none() {
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
                Err(taken) => taken,
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
    /// `texture`, as wlroots reads a client's DMA-BUF (`render/vulkan/pass.c`): wait for the
    /// compositor's writes (its sync file), take the plane over from the foreign queue family in
    /// `GENERAL` layout, copy, give it back, and wait until the copy is done so the capture can
    /// have its buffer back. `NotImported` only if the graphics card can't import it.
    fn import(
        &self,
        fd: OwnedFd,
        (offset, stride, modifier): (u32, u32, u64),
        extent: wgpu::Extent3d,
        format: wgpu::TextureFormat,
        texture: &wgpu::Texture,
    ) -> Result<(), Taken> {
        let Some((raw_device, family)) = &self.foreign else {
            return Err(Taken::NotImported("no DMA-BUF import".into()));
        };
        let failed = |error: String| {
            Taken::Failed(Ended::Restart(format!(
                "The mirror lost the graphics card; restarting it. ({error})"
            )))
        };
        if !wait_for_writes(fd.as_fd()).map_err(failed)? {
            // The compositor is still drawing it after a refresh: keep the last picture.
            return Ok(());
        }
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
            let device = unsafe { self.device.as_hal::<Vulkan>() }
                .ok_or_else(|| Taken::NotImported("not a Vulkan device".into()))?;
            // SAFETY: the device has VULKAN_EXTERNAL_MEMORY_DMA_BUF (`foreign` is set only then),
            // and `fd`, `modifier`, `stride` and `offset` describe the single plane as PipeWire
            // gave it, in the size and format of `hal_descriptor`.
            unsafe {
                device.texture_from_dmabuf_fd(
                    fd,
                    &hal_descriptor,
                    modifier,
                    stride.into(),
                    offset.into(),
                )
            }
            .map_err(|error| Taken::NotImported(error.to_string()))?
        };
        // SAFETY: `imported` was made on this device from `hal_descriptor`, which this matches.
        // wgpu is told it is ready to copy from: the acquire barrier below puts it there first.
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
                wgpu::TextureUses::COPY_SRC,
            )
        };
        // SAFETY: the texture is Vulkan; its image lives as long as `imported`.
        let image = unsafe { imported.as_hal::<Vulkan>() }
            .map(|hal| {
                // SAFETY: only the handle is read, for barriers recorded while `imported` lives.
                unsafe { hal.raw_handle() }
            })
            .ok_or_else(|| Taken::NotImported("not a Vulkan texture".into()))?;
        let acquire = self.barrier(raw_device, foreign_barrier(image, *family, true));
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_texture(imported.as_image_copy(), texture.as_image_copy(), extent);
        let release = self.barrier(raw_device, foreign_barrier(image, *family, false));
        let submitted = self.queue.submit([acquire, encoder.finish(), release]);
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submitted),
                timeout: Some(std::time::Duration::from_secs(1)),
            })
            .map_err(|error| failed(error.to_string()))?;
        Ok(())
    }

    /// A command buffer with only `barrier` in it, recorded on the raw Vulkan command buffer.
    fn barrier(
        &self,
        raw_device: &ash::Device,
        (barrier, stages): (vk::ImageMemoryBarrier<'static>, [vk::PipelineStageFlags; 2]),
    ) -> wgpu::CommandBuffer {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        // SAFETY: the encoder is Vulkan and open; only a pipeline barrier on an image that outlives
        // the submission is recorded, and wgpu is not used on this encoder meanwhile.
        unsafe {
            encoder.as_hal_mut::<Vulkan, _, _>(|hal| {
                if let Some(hal) = hal {
                    raw_device.cmd_pipeline_barrier(
                        hal.raw_handle(),
                        stages[0],
                        stages[1],
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &[barrier],
                    );
                }
            });
        }
        encoder.finish()
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
