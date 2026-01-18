use core::slice;
use std::{cell::UnsafeCell, ops::Range, ptr::NonNull, sync::atomic::Ordering};

mod buffer_pool;
mod mapped_ring;

use io_uring::{IoUring, types::BufRingEntry};

use crate::{buffer_pool::BufferPool, mapped_ring::MmapedRing};

pub struct RingBuffer<const BUFFER_SIZE: usize, const RING_SIZE: usize> {
    buffer_pool: UnsafeCell<BufferPool<BUFFER_SIZE, RING_SIZE>>,
    mapped_ring: UnsafeCell<MmapedRing>,
    id: u16,
}

// No custom Drop needed - kernel cleans up when io_uring fd is closed

impl<const BUFFER_SIZE: usize, const RING_SIZE: usize> RingBuffer<BUFFER_SIZE, RING_SIZE> {
    pub fn group_id(&self) -> u16 {
        self.id
    }

    pub fn new(ring: &IoUring, flags: u16, buffer_group_id: u16) -> std::io::Result<Self> {
        assert!(BUFFER_SIZE.is_power_of_two());
        assert!(BUFFER_SIZE <= u32::MAX as usize);
        assert!(BUFFER_SIZE != 0);
        assert!(RING_SIZE.is_power_of_two());
        assert!(RING_SIZE <= u16::MAX as usize);
        assert!(RING_SIZE != 0);

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
        inner.get(bid).map(|ptr| Buffer { bid, ptr, len })
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
            });
        }
        None
    }

    /// Recycle a single buffer back into the ring
    pub fn recycle(&self, bid: u16) {
        println!("freed");
        let ring = unsafe { &*self.mapped_ring.get() };
        let pool = unsafe { &*self.buffer_pool.get() };

        unsafe {
            let ring_ptr = ring.inner().as_ptr();
            let tail_ptr = BufRingEntry::tail(ring_ptr) as *const std::sync::atomic::AtomicU16;
            let tail = (*tail_ptr).load(Ordering::Acquire);
            let idx = (tail as usize) & (RING_SIZE - 1);
            let entry = ring_ptr.add(idx);

            (*entry).set_addr(pool.ptr_for_bid(bid as usize) as u64);
            (*entry).set_len(BUFFER_SIZE as u32);
            (*entry).set_bid(bid);

            (*tail_ptr).store(tail.wrapping_add(1), Ordering::Release);
        }
    }
}

/// this buffer represents an immutable slice over multiple contiguous underlying buffers, recycle it when you are done. Made for Bundled Recv
/// not automatically returned on Drop.
pub struct BufferRange {
    ptr: NonNull<u8>,
    len: usize,
    range: Range<u16>,
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

#[cfg(test)]
mod test {
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::os::fd::AsRawFd;

    use io_uring::opcode;
    use io_uring::squeue::Flags;
    use io_uring::types::Fd;

    use crate::RingBuffer;

    #[test]
    fn test_recv_with_buffer_ring() {
        const BUFFER_SIZE: usize = 4;
        const SIZE: usize = 4;
        const BGID: u16 = 0;

        // Create io_uring
        let mut ring = io_uring::IoUring::new(64).unwrap();

        // Create buffer ring: 4 buffers of 4 bytes = 16 bytes total
        let br = RingBuffer::<BUFFER_SIZE, SIZE>::new(&ring, 0, BGID).unwrap();

        // Create a TCP listener and connect
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let mut client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        server.set_nonblocking(true).unwrap();

        // Submit recv with provided buffer selection
        let recv_entry = opcode::Recv::new(
            Fd(server.as_raw_fd()),
            std::ptr::null_mut(),
            BUFFER_SIZE as u32,
        )
        .buf_group(BGID)
        .build()
        .flags(Flags::BUFFER_SELECT)
        .user_data(0x42);

        // Send and receive multiple times to test recycling
        // We have only 4 buffers, but we'll do 8 sends to prove recycling works
        for i in 0..8u8 {
            let test_data = [b'A' + i; 4]; // "AAAA", "BBBB", etc.
            client.write_all(&test_data).unwrap();
            client.flush().unwrap();

            unsafe {
                ring.submission().push(&recv_entry).unwrap();
            }
            ring.submit_and_wait(1).unwrap();

            let cqe = ring.completion().next().unwrap();
            assert!(cqe.result() >= 0, "recv {} failed: {}", i, cqe.result());

            let bytes_read = cqe.result() as usize;
            let flags = cqe.flags();
            let buffer_id = (flags >> 16) as u16;

            let buffer = br.get_buffer(buffer_id, bytes_read).unwrap();
            println!(
                "Round {}: Received {} bytes in buffer {}: {:?}",
                i,
                bytes_read,
                buffer_id,
                std::str::from_utf8(buffer.as_ref()).unwrap()
            );

            assert_eq!(buffer.as_ref(), &test_data);
            br.recycle(buffer_id);
        }

        println!("Successfully recycled buffers - 8 recvs with only 4 buffers!");
    }
}
