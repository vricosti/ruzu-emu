pub mod backend;
pub mod common;
pub mod exclusive_monitor;
pub mod frontend;
pub mod halt_reason;
pub mod interface;
pub mod ir;
pub mod jit;

pub use exclusive_monitor::ExclusiveMonitor;
pub use interface::optimization_flags::OptimizationFlag;
pub use jit::A32Jit;
pub use jit::A64Jit;

#[cfg(test)]
mod tests_a32;
#[cfg(test)]
mod tests_a32_fuzz;
