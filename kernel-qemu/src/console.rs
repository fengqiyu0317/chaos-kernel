use core::fmt::{self, Write};

use crate::sbi;

// AGENT: Minimal SBI-backed console writer for the M9 QEMU boot shell.
struct SbiConsole;

// AGENT: expose the byte-oriented SBI backend to the first userspace fd while
// retaining the kernel console's newline normalization.
pub fn write_bytes(bytes: &[u8]) -> usize {
    for &byte in bytes {
        if byte == b'\n' {
            sbi::console_putchar(b'\r');
        }
        sbi::console_putchar(byte);
    }
    bytes.len()
}

impl Write for SbiConsole {
    // AGENT: Route formatted Rust output to the legacy SBI console.
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_bytes(s.as_bytes());
        Ok(())
    }
}

// AGENT: Shared formatting entry point used by the local print macros.
pub fn _print(args: fmt::Arguments<'_>) {
    let _ = SbiConsole.write_fmt(args);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::console::_print(core::format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    () => {
        $crate::print!("\n")
    };
    ($fmt:expr) => {
        $crate::print!(core::concat!($fmt, "\n"))
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::print!(core::concat!($fmt, "\n"), $($arg)*)
    };
}
