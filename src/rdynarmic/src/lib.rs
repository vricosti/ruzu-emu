pub mod backend;
pub mod common;
pub mod exclusive_monitor;
pub mod frontend;
pub mod halt_reason;
pub mod ir;
pub mod interface;
pub mod jit;
pub mod jit_config;

pub use exclusive_monitor::ExclusiveMonitor;
pub use jit::A32Jit;
pub use jit::A64Jit;
pub use jit_config::OptimizationFlag;

#[cfg(test)]
mod tests_a32;
#[cfg(test)]
mod tests_a32_fuzz;
