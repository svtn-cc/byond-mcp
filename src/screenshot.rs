//! Captures a client window straight from the window manager, so it works while occluded.

use std::mem::zeroed;
use std::ptr::null_mut;

use anyhow::{bail, Result};
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
	CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
	BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
};
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
	EnumWindows, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, SetProcessDPIAware,
};

/// Asks DWM for the composed window, which is what makes hardware-rendered maps and WebView2 show up.
const PW_RENDERFULLCONTENT: u32 = 2;

pub struct Window {
	handle: HWND,
	pub title: String,
	pub width: i32,
	pub height: i32,
}

struct Search {
	pid: u32,
	found: Vec<Window>,
}

unsafe extern "system" fn collect(handle: HWND, param: LPARAM) -> BOOL {
	let search = &mut *(param as *mut Search);
	let mut owner = 0u32;
	GetWindowThreadProcessId(handle, &mut owner);
	if owner != search.pid || IsWindowVisible(handle) == 0 {
		return 1;
	}
	let mut rect: RECT = zeroed();
	GetWindowRect(handle, &mut rect);
	let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
	if width <= 0 || height <= 0 {
		return 1;
	}
	let mut title = [0u16; 256];
	let length = GetWindowTextW(handle, title.as_mut_ptr(), title.len() as i32).max(0) as usize;
	search.found.push(Window { handle, title: String::from_utf16_lossy(&title[..length]), width, height });
	1
}

/// Visible top-level windows of a process, largest first.
pub fn windows_of(pid: u32) -> Vec<Window> {
	let mut search = Search { pid, found: Vec::new() };
	unsafe {
		// Without this, sizes are reported in scaled units and captures come out cropped.
		SetProcessDPIAware();
		EnumWindows(Some(collect), &mut search as *mut Search as LPARAM);
	}
	search.found.sort_by_key(|window| -(i64::from(window.width) * i64::from(window.height)));
	search.found
}

pub fn capture_png(window: &Window) -> Result<Vec<u8>> {
	let (width, height) = (window.width, window.height);
	let mut pixels = vec![0u8; width as usize * height as usize * 4];
	let copied = unsafe {
		let screen = GetDC(null_mut());
		let memory = CreateCompatibleDC(screen);
		let bitmap = CreateCompatibleBitmap(screen, width, height);
		let previous = SelectObject(memory, bitmap);
		let printed = PrintWindow(window.handle, memory, PW_RENDERFULLCONTENT);
		SelectObject(memory, previous);

		let mut info: BITMAPINFO = zeroed();
		info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
		info.bmiHeader.biWidth = width;
		info.bmiHeader.biHeight = -height; // top-down rows
		info.bmiHeader.biPlanes = 1;
		info.bmiHeader.biBitCount = 32;
		let rows = GetDIBits(screen, bitmap, 0, height as u32, pixels.as_mut_ptr().cast(), &mut info, DIB_RGB_COLORS);

		DeleteObject(bitmap);
		DeleteDC(memory);
		ReleaseDC(null_mut(), screen);
		printed != 0 && rows == height
	};
	if !copied {
		bail!("Windows refused to capture window {:?}", window.title);
	}
	for pixel in pixels.chunks_exact_mut(4) {
		pixel.swap(0, 2); // BGRA -> RGBA
		pixel[3] = 255;
	}

	let mut encoded = Vec::new();
	let mut encoder = png::Encoder::new(&mut encoded, width as u32, height as u32);
	encoder.set_color(png::ColorType::Rgba);
	encoder.set_depth(png::BitDepth::Eight);
	encoder.write_header()?.write_image_data(&pixels)?;
	Ok(encoded)
}
