//! Captures the source monitor with DXGI Desktop Duplication (ADR 0002) and draws the newest
//! frame, converted to SDR, plus the pointer, through the mirror window's `Picture` seam.
//! Facts relied on are in `docs/win32-mirror-audio.md`.

use super::pointer::{pointer_image, pointer_viewport};
use super::window::Picture;
use super::{Ended, Fit, recreate_capture_in_place, start_failed, white_scale};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::E_POINTER;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP, ID3DBlob};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE, D3D11_BLEND_DESC,
    D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, D3D11_BLEND_SRC_ALPHA,
    D3D11_BLEND_ZERO, D3D11_BUFFER_DESC, D3D11_COLOR_WRITE_ENABLE_ALL,
    D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_RENDER_TARGET_BLEND_DESC, D3D11_SAMPLER_DESC,
    D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_VIEWPORT, ID3D11BlendState, ID3D11Buffer, ID3D11Device, ID3D11DeviceContext1,
    ID3D11PixelShader, ID3D11RenderTargetView, ID3D11SamplerState, ID3D11ShaderResourceView,
    ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_POINTER_SHAPE_INFO, IDXGIOutput,
    IDXGIOutput6, IDXGIOutputDuplication,
};
use windows::core::{Interface, PCSTR, s};

/// How long to wait before re-creating a lost duplication (desktop switch, mode change, prompt).
const RETRY: Duration = Duration::from_millis(250);

/// Full-screen quad from four vertex ids; `copy` passes an 8-bit picture through, `hdr` turns
/// linear scRGB into sRGB SDR exactly as `super::sdr_from_hdr` does.
const SHADERS: &str = r"
Texture2D picture : register(t0);
SamplerState smooth : register(s0);
cbuffer Hdr : register(b0) { float white_scale; };

struct Pixel { float4 position : SV_Position; float2 uv : TEXCOORD0; };

Pixel vertex_main(uint id : SV_VertexID) {
    Pixel pixel;
    pixel.uv = float2(id & 1, id >> 1);
    pixel.position = float4(pixel.uv.x * 2 - 1, 1 - pixel.uv.y * 2, 0, 1);
    return pixel;
}

float4 copy_main(Pixel pixel) : SV_Target {
    return picture.Sample(smooth, pixel.uv);
}

float4 hdr_main(Pixel pixel) : SV_Target {
    float3 value = saturate(picture.Sample(smooth, pixel.uv).rgb * white_scale);
    float3 srgb = value <= 0.0031308 ? value * 12.92 : 1.055 * pow(value, 1.0 / 2.4) - 0.055;
    return float4(srgb, 1);
}
";

/// How `Painter::paint` treats the texture it draws.
#[derive(Debug, Clone, Copy)]
pub enum Paint {
    /// An 8-bit picture, drawn as it is.
    Copy,
    /// A linear scRGB picture, multiplied by this `white_scale` and encoded to sRGB.
    Hdr(f32),
    /// The pointer, blended over the picture by its alpha.
    Blend,
}

/// Draws a texture into a viewport of a render target.
pub struct Painter {
    vertex: ID3D11VertexShader,
    copy: ID3D11PixelShader,
    hdr: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
    white: ID3D11Buffer,
}

/// Shader compiles in this process; the tests check there are only three.
#[cfg(test)]
static COMPILES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The vertex, copy and HDR shaders' bytecode, or the HRESULT compiling them failed with.
type Bytecode = Result<[Vec<u8>; 3], windows::core::HRESULT>;

/// Compiles the shaders on first use only: every mirror start and restart reuses the bytecode,
/// so a restart costs the sender no `D3DCompile`. A failure is kept too, since it would repeat.
fn bytecode() -> windows::core::Result<&'static [Vec<u8>; 3]> {
    static BYTECODE: std::sync::OnceLock<Bytecode> = std::sync::OnceLock::new();
    BYTECODE
        .get_or_init(|| {
            Ok([
                compile(s!("vertex_main"), s!("vs_4_0"))?,
                compile(s!("copy_main"), s!("ps_4_0"))?,
                compile(s!("hdr_main"), s!("ps_4_0"))?,
            ])
        })
        .as_ref()
        .map_err(|&code| code.into())
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, windows::core::HRESULT> {
    #[cfg(test)]
    COMPILES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut code: Option<ID3DBlob> = None;
    // SAFETY: the source pointer and length describe SHADERS; code is a valid out pointer.
    unsafe {
        D3DCompile(
            SHADERS.as_ptr().cast(),
            SHADERS.len(),
            PCSTR::null(),
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut code,
            None,
        )
    }
    .map_err(|error| error.code())?;
    let code = code.ok_or(E_POINTER)?;
    // SAFETY: the blob owns GetBufferSize bytes at GetBufferPointer while `code` lives; they are copied out.
    let bytes = unsafe {
        std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize())
    };
    Ok(bytes.to_vec())
}

/// Turns an out-parameter a successful create call left empty into an error.
fn created<T>(value: Option<T>) -> windows::core::Result<T> {
    value.ok_or_else(|| windows::core::Error::from(E_POINTER))
}

impl Painter {
    pub fn new(device: &ID3D11Device) -> windows::core::Result<Self> {
        let [vertex_code, copy_code, hdr_code] = bytecode()?;
        let (mut vertex, mut copy, mut hdr, mut sampler, mut blend, mut white) =
            (None, None, None, None, None, None);
        let pointer_blend = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            SrcBlend: D3D11_BLEND_SRC_ALPHA,
            DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: D3D11_BLEND_ONE,
            DestBlendAlpha: D3D11_BLEND_ZERO,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: u8::try_from(D3D11_COLOR_WRITE_ENABLE_ALL.0).unwrap_or(0x0f),
        };
        // SAFETY: bytecode slices and descriptions outlive each call; out pointers are valid locals.
        unsafe {
            device.CreateVertexShader(vertex_code, None, Some(&mut vertex))?;
            device.CreatePixelShader(copy_code, None, Some(&mut copy))?;
            device.CreatePixelShader(hdr_code, None, Some(&mut hdr))?;
            device.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    MaxLOD: f32::MAX,
                    ..Default::default()
                },
                Some(&mut sampler),
            )?;
            device.CreateBlendState(
                &D3D11_BLEND_DESC {
                    RenderTarget: [pointer_blend; 8],
                    ..Default::default()
                },
                Some(&mut blend),
            )?;
            device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 16,
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0.cast_unsigned(),
                    ..Default::default()
                },
                None,
                Some(&mut white),
            )?;
        }
        Ok(Self {
            vertex: created(vertex)?,
            copy: created(copy)?,
            hdr: created(hdr)?,
            sampler: created(sampler)?,
            blend: created(blend)?,
            white: created(white)?,
        })
    }

    /// Draws `view` stretched over `viewport` (left, top, width, height) of `target`.
    pub fn paint(
        &self,
        context: &ID3D11DeviceContext1,
        target: &ID3D11RenderTargetView,
        view: &ID3D11ShaderResourceView,
        viewport: [f32; 4],
        paint: Paint,
    ) {
        let [left, top, width, height] = viewport;
        // SAFETY: every object bound here belongs to the context's device and lives through the draw.
        unsafe {
            context.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
            context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: left,
                TopLeftY: top,
                Width: width,
                Height: height,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            context.VSSetShader(&self.vertex, None);
            match paint {
                Paint::Hdr(scale) => {
                    let constants = [scale, 0.0, 0.0, 0.0];
                    context.UpdateSubresource(
                        &self.white,
                        0,
                        None,
                        constants.as_ptr().cast(),
                        0,
                        0,
                    );
                    context.PSSetConstantBuffers(0, Some(&[Some(self.white.clone())]));
                    context.PSSetShader(&self.hdr, None);
                }
                Paint::Copy | Paint::Blend => context.PSSetShader(&self.copy, None),
            }
            context.PSSetShaderResources(0, Some(&[Some(view.clone())]));
            context.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            match paint {
                Paint::Blend => context.OMSetBlendState(&self.blend, None, u32::MAX),
                Paint::Copy | Paint::Hdr(_) => context.OMSetBlendState(None, None, u32::MAX),
            }
            context.Draw(4, 0);
            context.OMSetBlendState(None, None, u32::MAX);
        }
    }
}

/// A texture the shaders can read.
pub struct Texture {
    pub texture: ID3D11Texture2D,
    pub view: ID3D11ShaderResourceView,
    pub width: u32,
    pub height: u32,
    pub format: DXGI_FORMAT,
}

impl Texture {
    /// `pixels` is the initial content with its row pitch in bytes.
    pub fn new(
        device: &ID3D11Device,
        (width, height): (u32, u32),
        format: DXGI_FORMAT,
        pixels: Option<(&[u8], u32)>,
    ) -> windows::core::Result<Self> {
        let description = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0.cast_unsigned(),
            ..Default::default()
        };
        let data = pixels.map(|(pixels, pitch)| D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: pitch,
            SysMemSlicePitch: 0,
        });
        let (mut texture, mut view) = (None, None);
        // SAFETY: description and data outlive the calls; data, if any, covers height rows of pitch bytes.
        unsafe {
            device.CreateTexture2D(
                &description,
                data.as_ref().map(std::ptr::from_ref),
                Some(&mut texture),
            )?;
            let texture = created(texture.clone())?;
            device.CreateShaderResourceView(&texture, None, Some(&mut view))?;
        }
        Ok(Self {
            texture: created(texture)?,
            view: created(view)?,
            width,
            height,
            format,
        })
    }
}

/// The pointer as the duplication last reported it.
#[derive(Default)]
struct Pointer {
    visible: bool,
    position: (i32, i32),
    image: Option<Texture>,
}

/// The source monitor's picture, duplicated on the card display's device.
pub struct Desktop {
    device: ID3D11Device,
    output: IDXGIOutput6,
    device_path: String,
    painter: Painter,
    duplication: Option<IDXGIOutputDuplication>,
    retry_at: Instant,
    white_scale: f32,
    frame: Option<Texture>,
    pointer: Pointer,
}

impl Desktop {
    /// Starts duplicating `output`, which shows the monitor with `device_path`. `device` must be on
    /// the output's graphics card (`super::source_output` checks that).
    pub fn open(
        device: &ID3D11Device,
        output: &IDXGIOutput,
        device_path: &str,
    ) -> Result<Self, Ended> {
        let failed = |what: &'static str| {
            move |error: windows::core::Error| {
                start_failed(
                    "Could not capture the source monitor.",
                    what,
                    error.code().0,
                )
            }
        };
        let mut desktop = Self {
            device: device.clone(),
            output: output.cast().map_err(|_| {
                Ended::Stop(
                    "Could not capture the source monitor: it needs Windows 10 1803 or later."
                        .into(),
                )
            })?,
            device_path: device_path.to_owned(),
            painter: Painter::new(device).map_err(failed("shaders"))?,
            duplication: None,
            retry_at: Instant::now(),
            white_scale: 1.0,
            frame: None,
            pointer: Pointer::default(),
        };
        desktop.duplicate().map_err(failed("Desktop Duplication"))?;
        Ok(desktop)
    }

    /// (Re)creates the duplication: FP16 when the source monitor is in HDR, else 8-bit.
    fn duplicate(&mut self) -> windows::core::Result<()> {
        // SAFETY: plain description read.
        let hdr = unsafe { self.output.GetDesc1() }.is_ok_and(|description| {
            description.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020
        });
        let formats: &[DXGI_FORMAT] = if hdr {
            &[DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_B8G8R8A8_UNORM]
        } else {
            &[DXGI_FORMAT_B8G8R8A8_UNORM]
        };
        // shortcut: the SDR white level is read when capture starts or is re-created; a slider change while running needs a restart.
        self.white_scale = white_scale(sdr_white_level(&self.device_path).unwrap_or(0));
        // SAFETY: the device is on the output's graphics card; formats outlives the call.
        self.duplication = Some(unsafe { self.output.DuplicateOutput1(&self.device, 0, formats) }?);
        Ok(())
    }

    /// Takes the newest frame, if a new one came, into `self.frame` and the pointer state.
    fn next_frame(&mut self, context: &ID3D11DeviceContext1) -> windows::core::Result<()> {
        let Some(duplication) = self.duplication.clone() else {
            return Ok(());
        };
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        // SAFETY: out pointers are valid locals; timeout 0 never blocks the mirror thread.
        match unsafe { duplication.AcquireNextFrame(0, &mut info, &mut resource) } {
            // No change since the last draw: keep showing the last frame.
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(()),
            result => result?,
        }
        let kept = self.keep(context, &duplication, &info, resource);
        // SAFETY: a frame was acquired above and is released once, after it was copied.
        let released = unsafe { duplication.ReleaseFrame() };
        kept.and(released)
    }

    fn keep(
        &mut self,
        context: &ID3D11DeviceContext1,
        duplication: &IDXGIOutputDuplication,
        info: &DXGI_OUTDUPL_FRAME_INFO,
        resource: Option<windows::Win32::Graphics::Dxgi::IDXGIResource>,
    ) -> windows::core::Result<()> {
        // A frame may carry only a pointer update; LastPresentTime is 0 then.
        if info.LastPresentTime != 0 {
            let image: ID3D11Texture2D = created(resource)?.cast()?;
            let mut description = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: description is a valid out pointer.
            unsafe { image.GetDesc(&mut description) };
            let size = (description.Width, description.Height);
            let reusable = self.frame.as_ref().is_some_and(|frame| {
                (frame.width, frame.height) == size && frame.format == description.Format
            });
            if !reusable {
                self.frame = Some(Texture::new(&self.device, size, description.Format, None)?);
            }
            if let Some(frame) = &self.frame {
                // SAFETY: same size and format, both on this device; the copy is queued before ReleaseFrame.
                unsafe { context.CopyResource(&frame.texture, &image) };
            }
        }
        // PointerPosition is only valid when LastMouseUpdateTime is not 0.
        if info.LastMouseUpdateTime != 0 {
            self.pointer.visible = info.PointerPosition.Visible.as_bool();
            self.pointer.position = (
                info.PointerPosition.Position.x,
                info.PointerPosition.Position.y,
            );
        }
        if info.PointerShapeBufferSize > 0 {
            let mut shape = vec![0u8; info.PointerShapeBufferSize as usize];
            let (mut needed, mut shape_info) = (0, DXGI_OUTDUPL_POINTER_SHAPE_INFO::default());
            // SAFETY: shape holds exactly the buffer size passed; out pointers are valid locals.
            unsafe {
                duplication.GetFramePointerShape(
                    info.PointerShapeBufferSize,
                    shape.as_mut_ptr().cast(),
                    &mut needed,
                    &mut shape_info,
                )
            }?;
            self.pointer.image = match pointer_image(
                shape_info.Type,
                shape_info.Width,
                shape_info.Height,
                shape_info.Pitch,
                &shape,
            ) {
                Some(image) => Some(Texture::new(
                    &self.device,
                    (image.width, image.height),
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    Some((&image.pixels, image.width * 4)),
                )?),
                None => None,
            };
        }
        Ok(())
    }
}

impl Picture for Desktop {
    fn size(&self) -> (u32, u32) {
        self.frame
            .as_ref()
            .map_or((0, 0), |frame| (frame.width, frame.height))
    }

    // shortcut: a rotated source monitor shows un-rotated (the duplication does not rotate); add rotation if anyone mirrors a portrait monitor.
    fn draw(
        &mut self,
        context: &ID3D11DeviceContext1,
        target: &ID3D11RenderTargetView,
        fit: Fit,
    ) -> windows::core::Result<()> {
        if self.duplication.is_none() && Instant::now() >= self.retry_at {
            match self.duplicate() {
                Ok(()) => {}
                Err(error) if recreate_capture_in_place(error.code().0) => {
                    self.retry_at = Instant::now() + RETRY;
                }
                // Ends the mirror; `super::ended` decides whether it restarts.
                Err(error) => return Err(error),
            }
        }
        if self.next_frame(context).is_err() {
            // Access lost (desktop switch, mode change, fullscreen change): re-create on a timer.
            self.duplication = None;
            self.retry_at = Instant::now() + RETRY;
        }
        let Some(frame) = &self.frame else {
            return Ok(());
        };
        let viewport = [
            fit.left as f32,
            fit.top as f32,
            fit.width as f32,
            fit.height as f32,
        ];
        let paint = if frame.format == DXGI_FORMAT_R16G16B16A16_FLOAT {
            Paint::Hdr(self.white_scale)
        } else {
            Paint::Copy
        };
        self.painter
            .paint(context, target, &frame.view, viewport, paint);
        if let (true, Some(image)) = (self.pointer.visible, &self.pointer.image) {
            let viewport = pointer_viewport(
                fit,
                (frame.width, frame.height),
                self.pointer.position,
                (image.width, image.height),
            );
            self.painter
                .paint(context, target, &image.view, viewport, Paint::Blend);
        }
        Ok(())
    }
}

/// The monitor's SDR white level (`DISPLAYCONFIG_SDR_WHITE_LEVEL.SDRWhiteLevel`, 1000 = 80 nits).
fn sdr_white_level(monitor_device_path: &str) -> Option<u32> {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_SDR_WHITE_LEVEL,
        DisplayConfigGetDeviceInfo,
    };
    let (paths, _) = crate::guard::display_config().ok()?;
    let path = paths.iter().find(|path| {
        crate::guard::device_path(path)
            .is_some_and(|(device_path, _)| device_path == monitor_device_path)
    })?;
    let mut level = DISPLAYCONFIG_SDR_WHITE_LEVEL::default();
    level.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL;
    level.header.size = u32::try_from(size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>()).ok()?;
    level.header.adapterId = path.targetInfo.adapterId;
    level.header.id = path.targetInfo.id;
    // SAFETY: header is the first field of a correctly sized DISPLAYCONFIG_SDR_WHITE_LEVEL, as size says.
    (unsafe { DisplayConfigGetDeviceInfo(&mut level.header) } == 0).then_some(level.SDRWhiteLevel)
}

/// Only one Desktop Duplication of an output per process, so tests that duplicate take turns.
#[cfg(test)]
pub fn duplicating() -> std::sync::MutexGuard<'static, ()> {
    static DUPLICATING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    DUPLICATING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
