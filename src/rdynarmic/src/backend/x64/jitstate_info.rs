use crate::backend::x64::a32_jitstate::A32JitState;
use crate::backend::x64::a64_jitstate::A64JitState;

/// Architecture-dependent `JitState` offsets consumed by the shared x64
/// backend.
///
/// Rust cannot reproduce Eden's templated constructor directly, so the two
/// concrete constructors below instantiate the same field inventory for the
/// A32 and A64 state owners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JitStateInfo {
    pub offsetof_guest_mxcsr: usize,
    pub offsetof_asimd_mxcsr: usize,
    pub offsetof_rsb_ptr: usize,
    pub rsb_ptr_mask: usize,
    pub offsetof_rsb_location_descriptors: usize,
    pub offsetof_rsb_codeptrs: usize,
    pub offsetof_cpsr_nzcv: usize,
    pub offsetof_fpsr_exc: usize,
    pub offsetof_fpsr_qc: usize,
    pub offsetof_halt_reason: usize,
}

impl JitStateInfo {
    pub const fn from_a32() -> Self {
        Self {
            offsetof_guest_mxcsr: A32JitState::offset_of_guest_mxcsr(),
            offsetof_asimd_mxcsr: A32JitState::offset_of_asimd_mxcsr(),
            offsetof_rsb_ptr: A32JitState::offset_of_rsb_ptr(),
            rsb_ptr_mask: A32JitState::RSB_PTR_MASK,
            offsetof_rsb_location_descriptors: A32JitState::offset_of_rsb_location_descriptors(),
            offsetof_rsb_codeptrs: A32JitState::offset_of_rsb_codeptrs(),
            offsetof_cpsr_nzcv: A32JitState::offset_of_cpsr_nzcv(),
            offsetof_fpsr_exc: A32JitState::offset_of_fpsr_exc(),
            offsetof_fpsr_qc: A32JitState::offset_of_fpsr_qc(),
            offsetof_halt_reason: A32JitState::offset_of_halt_reason(),
        }
    }

    pub const fn from_a64() -> Self {
        Self {
            offsetof_guest_mxcsr: A64JitState::offset_of_guest_mxcsr(),
            offsetof_asimd_mxcsr: A64JitState::offset_of_asimd_mxcsr(),
            offsetof_rsb_ptr: A64JitState::offset_of_rsb_ptr(),
            rsb_ptr_mask: A64JitState::RSB_PTR_MASK,
            offsetof_rsb_location_descriptors: A64JitState::offset_of_rsb_location_descriptors(),
            offsetof_rsb_codeptrs: A64JitState::offset_of_rsb_codeptrs(),
            offsetof_cpsr_nzcv: A64JitState::offset_of_cpsr_nzcv(),
            offsetof_fpsr_exc: A64JitState::offset_of_fpsr_exc(),
            offsetof_fpsr_qc: A64JitState::offset_of_fpsr_qc(),
            offsetof_halt_reason: A64JitState::offset_of_halt_reason(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a32_info_matches_a32_jit_state() {
        let info = JitStateInfo::from_a32();
        assert_eq!(
            info.offsetof_guest_mxcsr,
            A32JitState::offset_of_guest_mxcsr()
        );
        assert_eq!(
            info.offsetof_asimd_mxcsr,
            A32JitState::offset_of_asimd_mxcsr()
        );
        assert_eq!(info.offsetof_rsb_ptr, A32JitState::offset_of_rsb_ptr());
        assert_eq!(info.rsb_ptr_mask, A32JitState::RSB_PTR_MASK);
        assert_eq!(
            info.offsetof_rsb_location_descriptors,
            A32JitState::offset_of_rsb_location_descriptors()
        );
        assert_eq!(
            info.offsetof_rsb_codeptrs,
            A32JitState::offset_of_rsb_codeptrs()
        );
        assert_eq!(info.offsetof_cpsr_nzcv, A32JitState::offset_of_cpsr_nzcv());
        assert_eq!(info.offsetof_fpsr_exc, A32JitState::offset_of_fpsr_exc());
        assert_eq!(info.offsetof_fpsr_qc, A32JitState::offset_of_fpsr_qc());
        assert_eq!(
            info.offsetof_halt_reason,
            A32JitState::offset_of_halt_reason()
        );
    }

    #[test]
    fn a64_info_matches_a64_jit_state() {
        let info = JitStateInfo::from_a64();
        assert_eq!(
            info.offsetof_guest_mxcsr,
            A64JitState::offset_of_guest_mxcsr()
        );
        assert_eq!(
            info.offsetof_asimd_mxcsr,
            A64JitState::offset_of_asimd_mxcsr()
        );
        assert_eq!(info.offsetof_rsb_ptr, A64JitState::offset_of_rsb_ptr());
        assert_eq!(info.rsb_ptr_mask, A64JitState::RSB_PTR_MASK);
        assert_eq!(
            info.offsetof_rsb_location_descriptors,
            A64JitState::offset_of_rsb_location_descriptors()
        );
        assert_eq!(
            info.offsetof_rsb_codeptrs,
            A64JitState::offset_of_rsb_codeptrs()
        );
        assert_eq!(info.offsetof_cpsr_nzcv, A64JitState::offset_of_cpsr_nzcv());
        assert_eq!(info.offsetof_fpsr_exc, A64JitState::offset_of_fpsr_exc());
        assert_eq!(info.offsetof_fpsr_qc, A64JitState::offset_of_fpsr_qc());
        assert_eq!(
            info.offsetof_halt_reason,
            A64JitState::offset_of_halt_reason()
        );
    }
}
