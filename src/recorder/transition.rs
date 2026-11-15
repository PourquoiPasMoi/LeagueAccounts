//! The move from champ select (League client window) to the game window inside
//! one recording, drawn on the GPU with a small pixel shader: the last champ
//! select picture pushes forward, blurs and dims into the app's dark colour
//! while a soft blue light sweeps across, then the game settles in from a
//! slight zoom and blur. Both pictures are textures the recorder already has;
//! nothing here touches the game.

use windows::core::{s, Result, PCSTR};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::*;

/// Length of the transition.
pub const SECONDS: f64 = 1.4;

const SHADER: &str = r#"
cbuffer C : register(b0) {
    float4 rectA; float4 rectB; float2 outSize; float t; float hasB; float2 pad;
};
Texture2D A : register(t0);
Texture2D B : register(t1);
SamplerState S : register(s0);
struct V { float4 pos : SV_Position; float2 uv : TEXCOORD0; };

V vs(uint id : SV_VertexID) {
    V o;
    float2 uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
    o.uv = uv;
    return o;
}

float ease(float x) { x = saturate(x); return x * x * (3 - 2 * x); }

// A picture placed at `rect` (output uv), zoomed around the centre and
// blurred over `radius` output pixels (golden-angle disc of taps).
float3 picture(Texture2D tex, float4 rect, float2 uv, float zoom, float radius) {
    float2 p = (uv - 0.5) / zoom + 0.5;
    float2 size = max(rect.zw - rect.xy, 1e-4);
    float3 sum = 0;
    [unroll] for (int i = 0; i < 24; i++) {
        float angle = i * 2.39996;
        float d = sqrt((i + 0.5) / 24.0) * radius;
        float2 q = p + float2(cos(angle), sin(angle)) * d / outSize;
        float2 c = (q - rect.xy) / size;
        float inside = (c.x >= 0 && c.x <= 1 && c.y >= 0 && c.y <= 1) ? 1 : 0;
        sum += tex.SampleLevel(S, saturate(c), 0).rgb * inside;
    }
    return sum / 24.0;
}

float4 ps(V i) : SV_Target {
    float leave = ease(t / 0.55);
    float enter = ease((t - 0.42) / 0.58) * hasB;
    float3 base = float3(0.043, 0.043, 0.051);
    float3 col = lerp(picture(A, rectA, i.uv, 1 + 0.10 * leave, 20 * leave), base, leave * 0.92);
    if (hasB > 0.5) {
        float3 b = picture(B, rectB, i.uv, 1.07 - 0.07 * enter, 20 * (1 - enter));
        col = lerp(col, b, enter);
    }
    // Soft blue light sweeping across while the pictures change.
    float glow = sin(saturate(t) * 3.14159);
    glow *= glow;
    float2 d = (i.uv - 0.5) * float2(outSize.x / outSize.y, 1);
    float band = i.uv.x + i.uv.y * 0.35 - lerp(-0.45, 1.8, ease(t));
    float sweep = exp(-band * band * 18);
    col += float3(0.04, 0.52, 1.0) * (sweep * 0.22 + exp(-dot(d, d) * 2.5) * 0.10) * glow;
    col *= 1 - 0.35 * glow * saturate(dot(d, d) * 1.4);
    return float4(saturate(col), 1);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Constants {
    rect_a: [f32; 4],
    rect_b: [f32; 4],
    out_size: [f32; 2],
    t: f32,
    has_b: f32,
    pad: [f32; 4],
}

fn fail() -> windows::core::Error {
    windows::core::Error::from(E_FAIL)
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut code = None;
    unsafe {
        D3DCompile(SHADER.as_ptr().cast(), SHADER.len(), s!("transition"), None, None, entry, target, D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &mut code, None)?;
    }
    code.ok_or_else(fail)
}

fn bytes(blob: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()) }
}

/// A picture to draw: its texture view, its size, and where it sits in the output (pixels).
pub struct Picture<'a> {
    pub view: &'a ID3D11ShaderResourceView,
    pub rect: windows::Win32::Foundation::RECT,
}

pub struct Blender {
    context: ID3D11DeviceContext,
    vertex: ID3D11VertexShader,
    pixel: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    constants: ID3D11Buffer,
}

impl Blender {
    pub fn new(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<Self> {
        let vs = compile(s!("vs"), s!("vs_4_0"))?;
        let ps = compile(s!("ps"), s!("ps_4_0"))?;
        let mut vertex = None;
        let mut pixel = None;
        let mut sampler = None;
        let mut constants = None;
        unsafe {
            device.CreateVertexShader(bytes(&vs), None, Some(&mut vertex))?;
            device.CreatePixelShader(bytes(&ps), None, Some(&mut pixel))?;
            let desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            device.CreateSamplerState(&desc, Some(&mut sampler))?;
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<Constants>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            device.CreateBuffer(&desc, None, Some(&mut constants))?;
        }
        Ok(Self {
            context: context.clone(),
            vertex: vertex.ok_or_else(fail)?,
            pixel: pixel.ok_or_else(fail)?,
            sampler: sampler.ok_or_else(fail)?,
            constants: constants.ok_or_else(fail)?,
        })
    }

    /// Draw the transition at `t` (0..1) into `target` (`size` pixels).
    pub fn draw(&self, target: &ID3D11RenderTargetView, size: (u32, u32), from: &Picture, to: Option<&Picture>, t: f32) {
        let (width, height) = (size.0.max(1) as f32, size.1.max(1) as f32);
        let rect = |rect: &windows::Win32::Foundation::RECT| {
            [rect.left as f32 / width, rect.top as f32 / height, rect.right as f32 / width, rect.bottom as f32 / height]
        };
        let constants = Constants {
            rect_a: rect(&from.rect),
            rect_b: to.map_or([0.0; 4], |to| rect(&to.rect)),
            out_size: [width, height],
            t: t.clamp(0.0, 1.0),
            has_b: if to.is_some() { 1.0 } else { 0.0 },
            pad: [0.0; 4],
        };
        let context = &self.context;
        unsafe {
            context.UpdateSubresource(&self.constants, 0, None, (&constants as *const Constants).cast(), 0, 0);
            context.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
            let viewport = D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: width, Height: height, MinDepth: 0.0, MaxDepth: 1.0 };
            context.RSSetViewports(Some(&[viewport]));
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(&self.vertex, None);
            context.PSSetShader(&self.pixel, None);
            context.PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            context.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            let second = to.map_or(from.view, |to| to.view);
            context.PSSetShaderResources(0, Some(&[Some(from.view.clone()), Some(second.clone())]));
            context.Draw(3, 0);
            // Unbind, so the textures can be written and read by the video processor again.
            context.PSSetShaderResources(0, Some(&[None, None]));
            context.OMSetRenderTargets(None, None);
        }
    }
}

/// A shader view of a whole texture.
pub fn view(device: &ID3D11Device, texture: &ID3D11Texture2D) -> Result<ID3D11ShaderResourceView> {
    let mut view = None;
    unsafe { device.CreateShaderResourceView(texture, None, Some(&mut view))? };
    view.ok_or_else(fail)
}

/// A render target view of a whole texture.
pub fn target(device: &ID3D11Device, texture: &ID3D11Texture2D) -> Result<ID3D11RenderTargetView> {
    let mut view = None;
    unsafe { device.CreateRenderTargetView(texture, None, Some(&mut view))? };
    view.ok_or_else(fail)
}
