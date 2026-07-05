pub fn ser(c: u8) -> u8 {
    if c == b'\r' {
        b'\n'
    } else {
        c
    }
}
