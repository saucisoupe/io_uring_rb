use std::ptr::{NonNull, null_mut};

use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

pub struct BufferPool<const BUFFER_SIZE: usize, const RING_SIZE: usize> {
    ptr: *mut u8,
}

impl<const BUFFER_SIZE: usize, const RING_SIZE: usize> BufferPool<BUFFER_SIZE, RING_SIZE> {
    pub fn new() -> std::io::Result<Self> {
        let total_size = BUFFER_SIZE * RING_SIZE;
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
        let id = bid as usize;
        if id >= RING_SIZE {
            return None;
        }
        let ptr = unsafe { self.ptr.add(BUFFER_SIZE * id) };
        NonNull::new(ptr)
    }

    ///for building purpose
    pub(crate) fn ptr_for_bid(&self, bid: usize) -> *mut u8 {
        assert!(bid < RING_SIZE);
        unsafe { self.ptr.add(bid * BUFFER_SIZE) }
    }
}

impl<const BUFFER_SIZE: usize, const RING_SIZE: usize> Drop for BufferPool<BUFFER_SIZE, RING_SIZE> {
    fn drop(&mut self) {
        unsafe {
            let _ = rustix::mm::munmap(self.ptr.cast(), BUFFER_SIZE * RING_SIZE);
        }
    }
}
