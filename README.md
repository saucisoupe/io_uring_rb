Ring buffer manager for io_uring in *core-per-thread architecture context*, works along with the io-uring crate. I needed different features than the existing crates for my async runtime.
# WIP
- wrap-around for RangeBuffer

# conditions
- linux
- kernel version >= 5.19 (my code won't check)
- BUFFER_SIZE and RING_SIZE are compile-time and must be power-of-two

# features
- the ring buffer has constant size
- Buffer represents the slice of data contained in ONE buffer
- RangeBuffer represents a slice of data contained in 1 or more consecutive buffers (consecutive ids <=> contiguous memory) this is designed to handle IORING_RECVSEND_BUNDLE completions

# precautions to take
- Buffer and RangeBuffer are not automatically recycled on drop, feel free to implement your own freeing logic
- don't make anything cross thread boundary, the usecase is one io_uring per thread.
