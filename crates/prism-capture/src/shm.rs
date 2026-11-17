//! SHM buffers for capture, and conversion of the captured pixels to cairo ARGB32.

use crate::{Image, State};
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use wayland_client::protocol::{wl_buffer, wl_shm};
use wayland_client::QueueHandle;

pub struct ShmBuffer {
    pub buffer: wl_buffer::WlBuffer,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    ptr: *mut u8,
    len: usize,
    _fd: OwnedFd,
}

impl ShmBuffer {
    pub fn new(shm: &wl_shm::WlShm, qh: &QueueHandle<State>, width: u32, height: u32, stride: u32, format: wl_shm::Format) -> Result<ShmBuffer, String> {
        let len = stride as usize * height as usize;
        if len == 0 {
            return Err("empty capture size".into());
        }
        let raw = unsafe { libc::memfd_create(c"prism-capture".as_ptr(), libc::MFD_CLOEXEC) };
        if raw < 0 {
            return Err("memfd_create failed".into());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        if unsafe { libc::ftruncate(raw, len as libc::off_t) } != 0 {
            return Err("ftruncate failed".into());
        }
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, raw, 0) };
        if ptr == libc::MAP_FAILED {
            return Err("mmap failed".into());
        }
        let pool = shm.create_pool(fd.as_fd(), len as i32, qh, ());
        let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, format, qh, ());
        pool.destroy();
        Ok(ShmBuffer { buffer, width, height, stride, ptr: ptr as *mut u8, len, _fd: fd })
    }

    pub fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// Copy out as cairo ARGB32 (opaque).
    pub fn to_image(&self, format: wl_shm::Format) -> Image {
        to_argb32(self.bytes(), self.width, self.height, self.stride, format)
    }

    pub fn destroy(&mut self) {
        self.buffer.destroy();
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };
    }
}

/// Convert captured pixels to opaque cairo ARGB32 (bytes B, G, R, A on little-endian).
pub fn to_argb32(src: &[u8], width: u32, height: u32, stride: u32, format: wl_shm::Format) -> Image {
    use wl_shm::Format::*;
    let (w, h, s) = (width as usize, height as usize, stride as usize);
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        let row = &src[y * s..y * s + w * 4];
        let dst = &mut out[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            let px = u32::from_le_bytes([row[4 * x], row[4 * x + 1], row[4 * x + 2], row[4 * x + 3]]);
            let (r, g, b) = match format {
                Xrgb8888 | Argb8888 => ((px >> 16) & 0xff, (px >> 8) & 0xff, px & 0xff),
                Xbgr8888 | Abgr8888 => (px & 0xff, (px >> 8) & 0xff, (px >> 16) & 0xff),
                Xrgb2101010 | Argb2101010 => ((px >> 22) & 0xff, (px >> 12) & 0xff, (px >> 2) & 0xff),
                Xbgr2101010 | Abgr2101010 => ((px >> 2) & 0xff, (px >> 12) & 0xff, (px >> 22) & 0xff),
                _ => (0, 0, 0),
            };
            dst[4 * x..4 * x + 4].copy_from_slice(&[b as u8, g as u8, r as u8, 0xff]);
        }
    }
    Image { width, height, stride: width * 4, data: out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_formats() {
        // One pixel: R=0x11 G=0x22 B=0x33.
        let xrgb = 0x00112233u32.to_le_bytes();
        assert_eq!(to_argb32(&xrgb, 1, 1, 4, wl_shm::Format::Xrgb8888).data, [0x33, 0x22, 0x11, 0xff]);
        let xbgr = 0x00332211u32.to_le_bytes();
        assert_eq!(to_argb32(&xbgr, 1, 1, 4, wl_shm::Format::Xbgr8888).data, [0x33, 0x22, 0x11, 0xff]);
        let ten = ((0x11u32 << 22) | (0x22 << 12) | (0x33 << 2)).to_le_bytes();
        assert_eq!(to_argb32(&ten, 1, 1, 4, wl_shm::Format::Xrgb2101010).data, [0x33, 0x22, 0x11, 0xff]);
    }
}
