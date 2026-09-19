use anyhow::{bail, Result};

/// Anonymous private mapping, prefaulted with MAP_POPULATE and (best effort) mlock'ed.
pub struct Buffer {
    ptr: *mut u8,
    len: usize,
    pub locked: bool,
}

impl Buffer {
    pub fn new(len: usize) -> Result<Buffer> {
        // SAFETY: anonymous mapping, no fd; result checked against MAP_FAILED.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_POPULATE,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            bail!("mmap {len} bytes: {}", std::io::Error::last_os_error());
        }
        // SAFETY: ptr/len describe the mapping just created.
        let locked = unsafe { libc::mlock(ptr, len) } == 0;
        let buf = Buffer { ptr: ptr as *mut u8, len, locked };
        // Touch every page so the data is really resident and non-zero-page backed.
        // SAFETY: the mapping is len bytes, writable.
        unsafe {
            let mut p = buf.ptr;
            let end = buf.ptr.add(len);
            while p < end {
                p.write_volatile(1);
                p = p.add(4096);
            }
        }
        Ok(buf)
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.ptr
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: ptr/len are exactly what mmap returned/was given.
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };
    }
}
