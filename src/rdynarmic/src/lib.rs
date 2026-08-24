pub mod backend;
pub mod common;
pub mod frontend;
pub mod interface;
pub mod ir;
pub mod jit;

pub use interface::a32::a32::Jit as A32Jit;
pub use interface::a64::a64::Jit as A64Jit;
pub use interface::code_page::{CodePage, CODE_PAGE_SIZE};
pub use interface::exclusive_monitor::ExclusiveMonitor;
pub use interface::halt_reason::HaltReason;
pub use interface::optimization_flags::OptimizationFlag;

#[cfg(test)]
mod tests_a32;
#[cfg(test)]
mod tests_a32_fuzz;
