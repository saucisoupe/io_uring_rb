use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::thread::{self, sleep};
use std::time::Duration;

use io_uring::opcode;
use io_uring::squeue::Flags;
use io_uring::types::Fd;
use io_uring_rb::RingBuffer;

const TEXT: &str = "Sed ut perspiciatis unde omnis iste natus error sit voluptatem accusantium doloremque laudantium, totam rem aperiam, eaque ipsa quae ab illo inventore veritatis et quasi architecto beatae vitae dicta sunt explicabo. Nemo enim ipsam voluptatem quia voluptas sit aspernatur aut odit aut fugit, sed quia consequuntur magni dolores eos qui ratione voluptatem sequi nesciunt. Neque porro quisquam est, qui dolorem ipsum quia dolor sit amet, consectetur, adipisci velit, sed quia non numquam eius modi tempora incidunt ut labore et dolore magnam aliquam quaerat voluptatem. Ut enim ad minima veniam, quis nostrum exercitationem ullam corporis suscipit laboriosam, nisi ut aliquid ex ea commodi consequatur? Quis autem vel eum iure reprehenderit qui in ea voluptate velit esse quam nihil molestiae consequatur, vel illum qui dolorem eum fugiat quo voluptas nulla pariatur?";

#[test]
fn test_recv_with_buffer_ring() {
    const BUFFER_SIZE: usize = 4;
    const SIZE: usize = 4;
    const BGID: u16 = 0;

    let mut ring = io_uring::IoUring::new(64).unwrap();

    let br = RingBuffer::<BUFFER_SIZE, SIZE>::new(&ring, 0, BGID).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = thread::spawn(move || {
        let mut client = TcpStream::connect(addr).unwrap();
        for i in TEXT.as_bytes().chunks(32) {
            client.write_all(i).unwrap();
            client.flush().unwrap();
        }
    });
    let mut received = String::new();
    let (server, _) = listener.accept().unwrap();
    server.set_nonblocking(true).unwrap();
    sleep(Duration::from_millis(200));
    let recv_entry = opcode::RecvMultiBundle::new(Fd(server.as_raw_fd()), 0)
        .build()
        .flags(Flags::BUFFER_SELECT)
        .user_data(0x42);

    'outer: loop {
        unsafe {
            ring.submission().push(&recv_entry).unwrap();
        }
        ring.submit_and_wait(1).unwrap();

        for cqe in ring.completion() {
            let bytes_read = cqe.result();
            match bytes_read {
                n if n > 0 => {
                    let flags = cqe.flags();
                    let buffer_id = (flags >> 16) as u16;
                    let mut buffer = br.get_buffers_range(buffer_id, bytes_read as _).unwrap();

                    let s = unsafe { str::from_utf8_unchecked(buffer.as_ref()) };
                    received.push_str(s);
                    br.recycle_range_buffer(&mut buffer);
                }
                0 => break 'outer,
                e => panic!("{}", e),
            }
        }
    }
    handle.join().unwrap();
    assert_eq!(received, TEXT);
}
