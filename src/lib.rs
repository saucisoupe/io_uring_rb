use std::{
    cell::UnsafeCell, marker::PhantomData, ops::Range, ptr::NonNull, sync::atomic::Ordering,
};

pub mod buffer;
mod buffer_pool;
pub mod buffers_range;
mod mapped_ring;

use io_uring::{IoUring, types::BufRingEntry};

use crate::{
    buffer::Buffer,
    buffer_pool::BufferPool,
    buffers_range::{BufferRange, BufferRangeInner},
    mapped_ring::MmapedRing,
};

type BufferId = u16;

pub struct RingBuffer<const BUFFER_SIZE: u32, const RING_SIZE: u16> {
    buffer_pool: UnsafeCell<BufferPool<BUFFER_SIZE, RING_SIZE>>,
    mapped_ring: UnsafeCell<MmapedRing>,
    id: u16,
}

impl<const BUFFER_SIZE: u32, const RING_SIZE: u16> RingBuffer<BUFFER_SIZE, RING_SIZE> {
    pub fn group_id(&self) -> u16 {
        self.id
    }

    pub fn new(ring: &IoUring, flags: u16, buffer_group_id: u16) -> std::io::Result<Self> {
        assert!(BUFFER_SIZE.is_power_of_two());
        assert!(RING_SIZE.is_power_of_two());

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
            entry.set_addr(bp.ptr_for_bid(bid as _) as _);
            entry.set_bid(bid as u16);
            entry.set_len(BUFFER_SIZE);
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

    fn get_pool_base(&self) -> NonNull<u8> {
        let ring_ptr = unsafe { &*self.buffer_pool.get() };
        ring_ptr.get(0).unwrap() //always exists
    }

    fn get_range(&self, buff_range: &BufferRangeInner) -> Range<u16> {
        let BufferRangeInner { ptr, len } = buff_range;
        let base = self.get_pool_base();
        get_range_inner::<BUFFER_SIZE, RING_SIZE>(base, *ptr, *len)
    }

    pub fn get_buffer(&self, bid: BufferId, len: usize) -> Option<Buffer> {
        let inner = unsafe { &*self.buffer_pool.get() };
        if len > BUFFER_SIZE as usize {
            return None;
        }
        inner.get(bid).map(|ptr| Buffer {
            bid,
            ptr,
            len,
            _not_send_sync: PhantomData,
        })
    }

    pub fn get_buffers_range(&self, bid_first_buffer: BufferId, len: usize) -> Option<BufferRange> {
        let last_buffer = last_buffer_index::<BUFFER_SIZE, RING_SIZE>(bid_first_buffer, len);
        let inner = unsafe { &*self.buffer_pool.get() };
        if last_buffer >= RING_SIZE {
            let first_len = ((RING_SIZE - bid_first_buffer) as usize) * BUFFER_SIZE as usize;
            let second_len = len - first_len;
            if let Some(first) = inner.get(bid_first_buffer).map(|ptr| BufferRangeInner {
                ptr,
                len: first_len,
            }) {
                let second = inner.get(0).map(|ptr| BufferRangeInner {
                    ptr,
                    len: second_len,
                });
                Some(BufferRange {
                    first,
                    second,
                    _not_send_sync: PhantomData,
                })
            } else {
                None
            }
        } else {
            inner.get(bid_first_buffer).map(|ptr| BufferRange {
                first: BufferRangeInner { ptr, len },
                second: None,
                _not_send_sync: PhantomData,
            })
        }
    }

    ///recycles a buffer in the ring, use this only once on a buffer when you are done
    pub fn recycle_buffer(&self, buffer: &mut Buffer) {
        let ring = unsafe { &*self.mapped_ring.get() };

        unsafe {
            let ring_ptr = ring.inner().as_ptr();
            let tail_ptr = BufRingEntry::tail(ring_ptr) as *const std::sync::atomic::AtomicU16;
            let tail = (*tail_ptr).load(Ordering::Acquire);
            let idx = (tail as usize) & (RING_SIZE - 1) as usize;
            let entry = ring_ptr.add(idx);

            (*entry).set_addr(buffer.ptr.as_ptr() as u64);
            (*entry).set_len(BUFFER_SIZE);
            (*entry).set_bid(buffer.bid);

            (*tail_ptr).store(tail.wrapping_add(1), Ordering::Release);
        }
    }

    /// Recycles a range of buffers back to the ring based on ptr and len.
    /// Updates the atomic tail in one go after setting up all entries.
    pub fn recycle_inner_range(&self, buffer: &BufferRange) {
        let ring = unsafe { &*self.mapped_ring.get() };
        let pool = unsafe { &*self.buffer_pool.get() };
        let ids_to_free = self.get_range(&buffer.first).chain(
            buffer
                .second
                .as_ref()
                .map(|b| self.get_range(b))
                .unwrap_or_default(),
        );
        unsafe {
            let ring_ptr = ring.inner().as_ptr();
            let tail_ptr = BufRingEntry::tail(ring_ptr) as *const std::sync::atomic::AtomicU16;
            let tail = (*tail_ptr).load(Ordering::Acquire);

            let mut len = 0;
            for (i, bid) in ids_to_free.enumerate() {
                let idx = (tail.wrapping_add(i as u16) as usize) & ((RING_SIZE - 1) as usize);
                let entry = ring_ptr.add(idx);

                let ptr = pool.ptr_for_bid(bid);
                (*entry).set_addr(ptr as u64);
                (*entry).set_len(BUFFER_SIZE);
                (*entry).set_bid(bid);
                len += 1;
            }
            let new_tail = tail.wrapping_add(len);
            std::sync::atomic::fence(Ordering::Release);
            (*tail_ptr).store(new_tail, Ordering::Release);
        }
    }
}

///gets the id of the last buffer of a range, starting at bid
#[inline(always)]
fn last_buffer_index<const BUFFER_SIZE: u32, const RING_SIZE: u16>(
    bid: u16,
    total_len: usize,
) -> u16 {
    bid + (total_len as u32).div_ceil(BUFFER_SIZE) as u16 - 1
}

///creates a range of the different buffers holding the data
fn get_range_inner<const BUFFER_SIZE: u32, const RING_SIZE: u16>(
    base: NonNull<u8>,
    ptr: NonNull<u8>,
    len: usize,
) -> Range<u16> {
    let offset = unsafe { ptr.offset_from(base) };
    assert!(offset >= 0);
    let start = (offset as u32 / BUFFER_SIZE) as u16;
    let end = last_buffer_index::<BUFFER_SIZE, RING_SIZE>(start, len) + 1;
    start..end
}

#[cfg(test)]
mod tests {
    use std::ptr::NonNull;

    use crate::{get_range_inner, last_buffer_index};

    #[test]
    fn last_buffer_tests() {
        let k = last_buffer_index::<64, 64>(63, 64);
        assert_eq!(k, 63);
        let k = last_buffer_index::<64, 64>(0, 64);
        assert_eq!(k, 0);
        let k = last_buffer_index::<64, 64>(0, 25);
        assert_eq!(k, 0);
        let k = last_buffer_index::<64, 64>(0, 130);
        assert_eq!(k, 2);
    }

    #[test]
    fn get_range_inner_test() {
        const LOCAL_BUFFER_SIZE: u32 = 64;
        const LOCAL_RING_SIZE: u16 = 64;
        let mut arr = [0u8; (LOCAL_BUFFER_SIZE * LOCAL_RING_SIZE as u32) as usize];
        let base = unsafe { NonNull::new_unchecked(arr.as_mut_ptr()) };
        assert_eq!(
            3..4,
            get_range_inner::<LOCAL_BUFFER_SIZE, LOCAL_RING_SIZE>(
                base,
                unsafe { base.offset(3 * LOCAL_BUFFER_SIZE as isize) },
                63,
            )
        );
        assert_eq!(
            0..1,
            get_range_inner::<LOCAL_BUFFER_SIZE, LOCAL_RING_SIZE>(
                base,
                unsafe { base.offset(0) },
                63,
            )
        );
        assert_eq!(
            0..1,
            get_range_inner::<LOCAL_BUFFER_SIZE, LOCAL_RING_SIZE>(
                base,
                unsafe { base.offset(0) },
                64,
            )
        );
    }
}
