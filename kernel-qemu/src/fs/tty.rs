// AGENT

// AGENT: represent the first QEMU terminal character device explicitly instead
// of recognizing device behavior from a regular file's path string.
#[derive(Clone, Copy, Default)]
pub struct TtyDevice;

impl TtyDevice {
    // AGENT: keep stdin at the migration design's permitted EOF placeholder
    // until a QEMU console-input backend feeds the kernel TTY queue.
    pub fn read(&self, _buf: &mut [u8]) -> usize {
        0
    }

    // AGENT: route terminal bytes through the existing newline-normalizing SBI
    // console backend without creating a synthetic regular-file offset.
    pub fn write(&self, buf: &[u8]) -> usize {
        crate::console::write_bytes(buf)
    }
}

// AGENT: retain the migrated termios-shaped state for later line-discipline
// work; the first explicit TtyDevice does not claim those semantics yet.
#[derive(Clone, Copy)]
pub struct TrmIO {
    pub iflag: u32,
    pub oflag: u32,
    pub cflag: u32,
    pub lflag: u32,
    pub line: u8,
    pub cc: [u8; 32],
    pub ispeed: u32,
    pub ospeed: u32,
}

// AGENT: keep the terminal attribute defaults identical while documenting this
// block as migrated terminal state rather than an implicit regular file.
impl Default for TrmIO {
    fn default() -> Self {
        TrmIO {
            iflag: 0o66402,
            oflag: 0o5,
            cflag: 0o2277,
            lflag: 0o105073,
            line: 0,
            cc: [
                3, 28, 127, 21, 4, 0, 1, 0, 17, 19, 26, 255, 18, 15, 23, 22, 255, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
            ispeed: 0,
            ospeed: 0,
        }
    }
}

// AGENT: preserve the ABI-shaped terminal window size for future tty ioctls.
#[derive(Clone, Copy, Default)]
pub struct WinSz {
    pub row: u16,
    pub col: u16,
    pub xpx: u16,
    pub ypx: u16,
}
