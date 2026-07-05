// AGENT: crate root kept outside lib.rs; the implementation lives in
// kernel/, and legacy-compatible pieces live beside their real modules.
pub mod kernel;

// AGENT: keep the former flat public API for non-conflicting simulator items.
pub use kernel::*;

// AGENT: make the real simulator types available even though the root legacy
// API below intentionally uses the historical chaos-tests names.
pub use kernel::{
    GKL as SIM_GKL, Kernel as SimKernel, PgFrame as SimPgFrame, SharedPage as SimSharedPage,
    Task as SimTask, TaskInfo as SimTaskInfo, TaskTable as SimTaskTable,
};

// AGENT: root compatibility names consumed by chaos-tests/src/lib.rs.
pub use kernel::{
    LegacyGkl, LegacyKernel as Kernel, LegacyPgFrame as PgFrame, LegacySharedPage as SharedPage,
    LegacyTask as Task, LegacyTaskInfo as TaskInfo, LegacyTaskTable as TaskTable, GKL,
};
