use std::ptr::{NonNull, null_mut};

use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

use crate::BufferId;

pub struct BufferPool<const BUFFER_SIZE: u32, const RING_SIZE: u16> {
    ptr: *mut u8,
}

impl<const BUFFER_SIZE: u32, const RING_SIZE: u16> BufferPool<BUFFER_SIZE, RING_SIZE> {
    pub fn new() -> std::io::Result<Self> {
        let total_size = (BUFFER_SIZE * RING_SIZE as u32) as usize;
        let ptr = unsafe {
            mmap_anonymous(
                null_mut(),
                total_size,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::PRIVATE | MapFlags::POPULATE,
            )?
        };
        Ok(Self { ptr: ptr.cast() })
    }

    ///gets the pointer to the buffer of index bid (read-only)
    pub fn get(&self, bid: u16) -> Option<NonNull<u8>> {
        if bid >= RING_SIZE {
            return None;
        }
        let ptr = unsafe { self.ptr.add((BUFFER_SIZE * bid as u32) as _) };
        NonNull::new(ptr)
    }

    ///for building purpose
    pub(crate) fn ptr_for_bid(&self, bid: BufferId) -> *mut u8 {
        assert!(bid < RING_SIZE);
        unsafe { self.ptr.add((bid as u32 * BUFFER_SIZE) as usize) }
    }
}

impl<const BUFFER_SIZE: u32, const RING_SIZE: u16> Drop for BufferPool<BUFFER_SIZE, RING_SIZE> {
    fn drop(&mut self) {
        unsafe {
            let _ =
                rustix::mm::munmap(self.ptr.cast(), (BUFFER_SIZE * (RING_SIZE as u32)) as usize);
        }
    }
}
