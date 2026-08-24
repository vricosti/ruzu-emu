pub mod backend;
pub mod common;
pub mod frontend;
pub mod interface;
pub mod ir;
pub mod jit;

pub use interface::code_page::{CodePage, CODE_PAGE_SIZE};
pub use interface::exclusive_monitor::ExclusiveMonitor;
pub use interface::halt_reason::HaltReason;
pub use interface::optimization_flags::OptimizationFlag;
pub use jit::A32Jit;
pub use jit::A64Jit;

#[cfg(test)]
mod tests_a32;
#[cfg(test)]
mod tests_a32_fuzz;
