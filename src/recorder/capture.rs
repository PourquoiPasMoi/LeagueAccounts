//! Window capture the way OBS and Medal do it: Windows Graphics Capture of the
//! game's window, or, as a fallback, DXGI Desktop Duplication of the monitor it
//! is on (cropped to the window). Frames stay on the GPU as Direct3D 11
//! textures. Nothing here opens, reads or draws on the game process: Windows
//! composes the window and hands the picture over.

use windows::core::{Interface, Result, HSTRING, PCWSTR};
use windows::Graphics::Capture::{Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{E_FAIL, HMODULE, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIDevice, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Gdi::{ClientToScreen, MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
pub use windows::Win32::Foundation::HWND as WindowHandle;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetClientRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible,
};

/// The League game window's class (the client uses another one).
pub const GAME_WINDOW_CLASS: &str = "RiotWindowClass";

fn fail() -> windows::core::Error {
    windows::core::Error::from(E_FAIL)
}

/// One Direct3D 11 device shared by capture, colour conversion and the encoder.
pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    adapter: Option<IDXGIAdapter>,
}

impl Gpu {
    /// A device on the GPU that drives the window's monitor (laptops with two
    /// GPUs: no copy between them), or the default GPU.
    pub fn for_window(window: Option<HWND>) -> Result<Self> {
        let adapter = window.and_then(adapter_for_window);
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                adapter.as_ref(),
                if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE },
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let device = device.ok_or_else(fail)?;
        let context = context.ok_or_else(fail)?;
        // Media Foundation's encoders use the device from their own threads.
        if let Ok(multithread) = context.cast::<ID3D11Multithread>() {
            unsafe {
                let _ = multithread.SetMultithreadProtected(true);
            }
        }
        Ok(Self { device, context, adapter })
    }

    pub fn new() -> Result<Self> {
        Self::for_window(None)
    }

    fn winrt_device(&self) -> Result<IDirect3DDevice> {
        let dxgi: IDXGIDevice = self.device.cast()?;
        unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()
    }
}

fn adapter_for_window(window: HWND) -> Option<IDXGIAdapter> {
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    for index in 0..16 {
        let adapter = unsafe { factory.EnumAdapters1(index) }.ok()?;
        for output in 0..16 {
            let Ok(output) = (unsafe { adapter.EnumOutputs(output) }) else { break };
            if unsafe { output.GetDesc() }.is_ok_and(|desc| desc.Monitor == monitor) {
                return adapter.cast().ok();
            }
        }
    }
    None
}

enum Hold {
    Window(#[allow(dead_code)] Direct3D11CaptureFrame),
    Screen(IDXGIOutputDuplication),
}

/// A captured picture: the texture is valid until this value is dropped.
pub struct Frame {
    _hold: Hold,
    pub texture: ID3D11Texture2D,
    /// The game's picture inside the texture.
    pub region: RECT,
}

impl Drop for Frame {
    fn drop(&mut self) {
        if let Hold::Screen(duplication) = &self._hold {
            unsafe {
                let _ = duplication.ReleaseFrame();
            }
        }
    }
}

pub struct WindowCapture {
    hwnd: HWND,
    device: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    size: SizeInt32,
}

const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
const BUFFERS: i32 = 2;

impl WindowCapture {
    pub fn new(gpu: &Gpu, hwnd: HWND) -> Result<Self> {
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd)? };
        let size = item.Size()?;
        let device = gpu.winrt_device()?;
        // Free-threaded: frames are picked up from the recording thread, no message loop needed.
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&device, FORMAT, BUFFERS, size)?;
        let session = pool.CreateCaptureSession(&item)?;
        // Windows 11 lets apps drop the yellow frame and the cursor; Windows 10 refuses (ignored).
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(false);
        session.StartCapture()?;
        Ok(Self { hwnd, device, pool, session, size })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.size.Width.max(0) as u32, self.size.Height.max(0) as u32)
    }

    /// The newest frame since the last call, older ones are dropped.
    /// `None` when the picture did not change.
    pub fn latest(&mut self) -> Result<Option<Frame>> {
        let mut newest = None;
        while let Ok(frame) = self.pool.TryGetNextFrame() {
            newest = Some(frame);
        }
        let Some(frame) = newest else { return Ok(None) };
        let content = frame.ContentSize()?;
        if content.Width != self.size.Width || content.Height != self.size.Height {
            // The window was resized: buffers follow it, this frame is skipped.
            if content.Width > 0 && content.Height > 0 {
                self.size = content;
                self.pool.Recreate(&self.device, FORMAT, BUFFERS, content)?;
            }
            return Ok(None);
        }
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
        Ok(Some(Frame {
            _hold: Hold::Window(frame),
            texture,
            region: RECT { left: 0, top: 0, right: content.Width, bottom: content.Height },
        }))
    }
}

impl Drop for WindowCapture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
        let _ = self.hwnd;
    }
}

/// Desktop Duplication of the game's monitor, cropped to the game window.
/// Works with exclusive fullscreen and shows no capture border.
pub struct ScreenCapture {
    hwnd: HWND,
    device: ID3D11Device,
    output: IDXGIOutput1,
    duplication: IDXGIOutputDuplication,
    origin: POINT,
}

impl ScreenCapture {
    pub fn new(gpu: &Gpu, hwnd: HWND) -> Result<Self> {
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let adapter = match &gpu.adapter {
            Some(adapter) => adapter.clone(),
            None => unsafe { gpu.device.cast::<IDXGIDevice>()?.GetAdapter()? },
        };
        for index in 0..16 {
            let Ok(output) = (unsafe { adapter.EnumOutputs(index) }) else { break };
            let desc = unsafe { output.GetDesc()? };
            if desc.Monitor != monitor {
                continue;
            }
            let output: IDXGIOutput1 = output.cast()?;
            let duplication = unsafe { output.DuplicateOutput(&gpu.device)? };
            let origin = POINT { x: desc.DesktopCoordinates.left, y: desc.DesktopCoordinates.top };
            return Ok(Self { hwnd, device: gpu.device.clone(), output, duplication, origin });
        }
        Err(fail())
    }

    /// The window's client area, in the monitor texture's coordinates.
    fn window_region(&self) -> RECT {
        let mut client = RECT::default();
        let mut corner = POINT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut client);
            let _ = ClientToScreen(self.hwnd, &mut corner);
        }
        let left = corner.x - self.origin.x;
        let top = corner.y - self.origin.y;
        RECT { left, top, right: left + client.right, bottom: top + client.bottom }
    }

    pub fn size(&self) -> (u32, u32) {
        let region = self.window_region();
        ((region.right - region.left).max(2) as u32, (region.bottom - region.top).max(2) as u32)
    }

    pub fn latest(&mut self) -> Result<Option<Frame>> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match unsafe { self.duplication.AcquireNextFrame(0, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(error) if error.code() == DXGI_ERROR_ACCESS_LOST => {
                // Mode change or fullscreen switch: start duplicating again.
                self.duplication = unsafe { self.output.DuplicateOutput(&self.device)? };
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
        // LastPresentTime 0: only the mouse moved, the picture is the same.
        let texture = resource.filter(|_| info.LastPresentTime != 0).and_then(|resource| resource.cast::<ID3D11Texture2D>().ok());
        let Some(texture) = texture else {
            unsafe {
                let _ = self.duplication.ReleaseFrame();
            }
            return Ok(None);
        };
        Ok(Some(Frame { _hold: Hold::Screen(self.duplication.clone()), texture, region: self.window_region() }))
    }
}

/// Either way of capturing the game.
pub enum Capture {
    Window(WindowCapture),
    Screen(ScreenCapture),
}

impl Capture {
    /// Window capture, or screen capture when asked or when window capture fails.
    pub fn open(gpu: &Gpu, hwnd: HWND, prefer_screen: bool) -> Result<Self> {
        if !prefer_screen {
            match WindowCapture::new(gpu, hwnd) {
                Ok(capture) => return Ok(Self::Window(capture)),
                Err(_) => return ScreenCapture::new(gpu, hwnd).map(Self::Screen),
            }
        }
        match ScreenCapture::new(gpu, hwnd) {
            Ok(capture) => Ok(Self::Screen(capture)),
            Err(_) => WindowCapture::new(gpu, hwnd).map(Self::Window),
        }
    }

    pub fn size(&self) -> (u32, u32) {
        match self {
            Self::Window(capture) => capture.size(),
            Self::Screen(capture) => capture.size(),
        }
    }

    pub fn latest(&mut self) -> Result<Option<Frame>> {
        match self {
            Self::Window(capture) => capture.latest(),
            Self::Screen(capture) => capture.latest(),
        }
    }

    pub fn is_screen(&self) -> bool {
        matches!(self, Self::Screen(_))
    }

    pub fn window(&self) -> HWND {
        match self {
            Self::Window(capture) => capture.hwnd,
            Self::Screen(capture) => capture.hwnd,
        }
    }

    /// The window still exists (it closes when the game ends).
    pub fn is_alive(&self) -> bool {
        unsafe { IsWindow(Some(self.window())).as_bool() }
    }

    pub fn is_minimized(&self) -> bool {
        unsafe { IsIconic(self.window()).as_bool() }
    }
}

/// The process id that owns a window (for game-only audio). Only the id is
/// read; the process itself is never opened.
pub fn window_process_id(window: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    pid
}

/// The League game window, when a game is running.
pub fn find_game_window() -> Option<HWND> {
    find_window_by_class(GAME_WINDOW_CLASS)
}

pub fn find_window_by_class(class: &str) -> Option<HWND> {
    let window = unsafe { FindWindowW(&HSTRING::from(class), PCWSTR::null()) }.ok()?;
    (!window.is_invalid()).then_some(window)
}

/// The first visible top-level window whose title contains `needle` (tests).
pub fn find_window_by_title(needle: &str) -> Option<HWND> {
    struct Search {
        needle: String,
        found: Option<HWND>,
    }
    unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> windows::core::BOOL {
        let search = unsafe { &mut *(data.0 as *mut Search) };
        if unsafe { !IsWindowVisible(window).as_bool() } {
            return true.into();
        }
        let mut title = [0u16; 512];
        let length = unsafe { GetWindowTextW(window, &mut title) }.max(0) as usize;
        let title = String::from_utf16_lossy(&title[..length]).to_lowercase();
        if !title.is_empty() && title.contains(&search.needle) {
            search.found = Some(window);
            return false.into();
        }
        true.into()
    }
    let mut search = Search { needle: needle.to_lowercase(), found: None };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
    }
    search.found
}
