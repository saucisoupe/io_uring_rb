use core::slice;
use std::{marker::PhantomData, ptr::NonNull};

/// this buffer represents an immutable slice over multiple contiguous underlying buffers, recycle it when you are done. Made for Bundled Recv
/// not automatically returned on Drop.
#[derive(Debug)]
pub struct BufferRange {
    pub(crate) first: BufferRangeInner,
    pub(crate) second: Option<BufferRangeInner>,
    pub(crate) _not_send_sync: PhantomData<*const ()>,
}

#[derive(Debug)]
pub struct BufferRangeInner {
    pub(crate) ptr: NonNull<u8>,
    pub(crate) len: usize,
}

impl BufferRange {
    #[inline]
    pub fn as_parts(&self) -> (&[u8], Option<&[u8]>) {
        let r1 = self.first.as_slice();
        let r2 = self.second.as_ref().map(|s| s.as_slice());
        (r1, r2)
    }

    pub fn as_iterator(&self) -> impl Iterator<Item = &u8> {
        self.first
            .as_slice()
            .iter()
            .chain(self.second.iter().flat_map(|s| s.as_slice()))
    }
}

impl BufferRangeInner {
    #[inline]
    fn as_slice(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}
