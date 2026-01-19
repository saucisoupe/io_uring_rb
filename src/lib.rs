use core::slice;
use std::{
    cell::UnsafeCell, marker::PhantomData, ops::Range, ptr::NonNull, sync::atomic::Ordering,
};

mod buffer_pool;
mod mapped_ring;

use io_uring::{IoUring, types::BufRingEntry};

use crate::{buffer_pool::BufferPool, mapped_ring::MmapedRing};

pub struct RingBuffer<const BUFFER_SIZE: usize, const RING_SIZE: usize> {
    buffer_pool: UnsafeCell<BufferPool<BUFFER_SIZE, RING_SIZE>>,
    mapped_ring: UnsafeCell<MmapedRing>,
    id: u16,
}

impl<const BUFFER_SIZE: usize, const RING_SIZE: usize> RingBuffer<BUFFER_SIZE, RING_SIZE> {
    pub fn group_id(&self) -> u16 {
        self.id
    }

    pub fn new(ring: &IoUring, flags: u16, buffer_group_id: u16) -> std::io::Result<Self> {
        assert!(BUFFER_SIZE.is_power_of_two());
        assert!(BUFFER_SIZE <= u32::MAX as usize);
        assert!(RING_SIZE.is_power_of_two());
        assert!(RING_SIZE <= u16::MAX as usize);

        let mut mmaped_ring: MmapedRing = MmapedRing::build(RING_SIZE as _)?;
        let slice = mmaped_ring.as_slice();

        unsafe {
            ring.submitter().register_buf_ring_with_flags(
                slice.as_ptr() as _,
                RING_SIZE as _,
                buffer_group_id,
                flags,
            )?
        };

        let bp = BufferPool::<BUFFER_SIZE, RING_SIZE>::new()?;

        for (bid, slot) in slice.iter_mut().enumerate() {
            let entry = slot.write(unsafe { std::mem::zeroed() });
            entry.set_addr(bp.ptr_for_bid(bid) as _);
            entry.set_bid(bid as u16);
            entry.set_len(BUFFER_SIZE as u32);
        }

        unsafe {
            let tail = BufRingEntry::tail(slice.as_ptr() as *const BufRingEntry)
                as *const std::sync::atomic::AtomicU16;
            (*tail).store(RING_SIZE as _, Ordering::Release);
        }

        Ok(RingBuffer {
            buffer_pool: UnsafeCell::new(bp),
            mapped_ring: UnsafeCell::new(mmaped_ring),
            id: buffer_group_id,
        })
    }

    pub fn get_buffer(&self, bid: u16, len: usize) -> Option<Buffer> {
        let inner = unsafe { &*self.buffer_pool.get() };
        if len > BUFFER_SIZE {
            return None;
        }
        inner.get(bid).map(|ptr| Buffer {
            bid,
            ptr,
            len,
            _not_send_sync: PhantomData,
        })
    }

    pub fn get_buffers_range(&self, bid_first_buffer: u16, len: usize) -> Option<BufferRange> {
        let first_buffer = bid_first_buffer as usize;
        let last_buffer = last_buffer::<BUFFER_SIZE, RING_SIZE>(first_buffer, len);
        if last_buffer > RING_SIZE {
            return None;
        }
        let inner = unsafe { &*self.buffer_pool.get() };
        if let Some(ptr) = inner.get(bid_first_buffer) {
            return Some(BufferRange {
                range: first_buffer as _..last_buffer as _,
                ptr,
                len,
                _not_send_sync: PhantomData,
            });
        }
        None
    }

    ///recycles a buffer in the ring, use this only once on a buffer when you are done
    pub fn recycle_buffer(&self, buffer: &mut Buffer) {
        let ring = unsafe { &*self.mapped_ring.get() };

        unsafe {
            let ring_ptr = ring.inner().as_ptr();
            let tail_ptr = BufRingEntry::tail(ring_ptr) as *const std::sync::atomic::AtomicU16;
            let tail = (*tail_ptr).load(Ordering::Acquire);
            let idx = (tail as usize) & (RING_SIZE - 1);
            let entry = ring_ptr.add(idx);

            (*entry).set_addr(buffer.ptr.as_ptr() as u64);
            (*entry).set_len(BUFFER_SIZE as u32);
            (*entry).set_bid(buffer.bid);

            (*tail_ptr).store(tail.wrapping_add(1), Ordering::Release);
        }
    }

    /// recycles all buffers in a range back to the ring.
    pub fn recycle_range_buffer(&self, buffer: &mut BufferRange) {
        let ring = unsafe { &*self.mapped_ring.get() };
        let pool = unsafe { &*self.buffer_pool.get() };

        unsafe {
            let ring_ptr = ring.inner().as_ptr();
            let tail_ptr = BufRingEntry::tail(ring_ptr) as *const std::sync::atomic::AtomicU16;
            let tail = (*tail_ptr).load(Ordering::Acquire);

            let count = buffer.range.end - buffer.range.start;

            for (i, bid) in buffer.range.clone().enumerate() {
                let idx = (tail.wrapping_add(i as u16) as usize) & (RING_SIZE - 1);
                let entry = ring_ptr.add(idx);

                let ptr = pool.ptr_for_bid(bid as usize);
                (*entry).set_addr(ptr as u64);
                (*entry).set_len(BUFFER_SIZE as u32);
                (*entry).set_bid(bid);
            }

            (*tail_ptr).store(tail.wrapping_add(count), Ordering::Release);
        }
    }
}

/// this buffer represents an immutable slice over multiple contiguous underlying buffers, recycle it when you are done. Made for Bundled Recv
/// not automatically returned on Drop.
pub struct BufferRange {
    ptr: NonNull<u8>,
    len: usize,
    range: Range<u16>,
    _not_send_sync: PhantomData<*const ()>,
}

impl BufferRange {
    pub fn range(&self) -> Range<u16> {
        self.range.clone()
    }
}

/// this buffer represents an immutable slice in a buffer, recycle it when you are done.
/// not automatically returned on Drop.
pub struct Buffer {
    ptr: NonNull<u8>,
    len: usize,
    bid: u16,
    _not_send_sync: PhantomData<*const ()>,
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

impl AsRef<[u8]> for BufferRange {
    fn as_ref(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

///gets the id of the last buffer of a range, starting at bid
#[inline(always)]
fn last_buffer<const BUFFER_SIZE: usize, const RING_SIZE: usize>(
    bid: usize,
    total_len: usize,
) -> usize {
    bid + total_len.div_ceil(BUFFER_SIZE)
}
