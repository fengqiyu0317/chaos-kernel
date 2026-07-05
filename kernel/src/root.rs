// AGENT: crate root kept outside lib.rs; the implementation lives in kernel/.
pub mod kernel;

// AGENT: expose the kernel tree directly; compatibility names are now the
// official root names, and Runtime* names are available only when full
// simulator internals are needed.
pub use kernel::*;
