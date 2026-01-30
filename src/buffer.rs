use core::slice;
use std::{marker::PhantomData, ptr::NonNull};

/// this buffer represents an immutable slice in a buffer, recycle it when you are done.
/// not automatically returned on Drop.
#[derive(Debug)]
pub struct Buffer {
    pub(crate) ptr: NonNull<u8>,
    pub(crate) len: usize,
    pub(crate) bid: u16,
    pub(crate) _not_send_sync: PhantomData<*const ()>,
}

impl Buffer {
    pub fn bid(&self) -> u16 {
        self.bid
    }
}

impl AsRef<[u8]> for Buffer {
    fn as_ref(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl AsMut<[u8]> for Buffer {
    fn as_mut(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}
