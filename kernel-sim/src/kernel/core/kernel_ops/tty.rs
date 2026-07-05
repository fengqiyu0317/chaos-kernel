use super::*;

impl RuntimeKernel {
    // AGENT: normalize terminal input and append it to the simulator TTY buffer.
    pub fn tty_push(&self, c: u8) {
        let byte = if c == b'\r' { b'\n' } else { c };
        let mut buf = self.tty_buf.lock().unwrap();
        if buf.len() < 4096 {
            buf.push_back(byte);
        }
    }

    // AGENT: consume one byte from the simulator TTY buffer.
    pub fn tty_pop(&self) -> Option<u8> {
        let mut buf = self.tty_buf.lock().unwrap();
        buf.pop_front()
    }
}
