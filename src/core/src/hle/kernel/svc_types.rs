//! Port of zuyu/src/core/hle/kernel/svc_types.h
//! Status: COMPLET
//! Derniere synchro: 2026-03-11
//!
//! SVC types, enums, and structures used by the kernel's supervisor call interface.
//! All constants and enums match upstream values exactly.

use bitflags::bitflags;

/// SVC Handle type.
pub type Handle = u32;

/// Memory state enumeration.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryState {
    Free = 0x00,
    Io = 0x01,
    Static = 0x02,
    Code = 0x03,
    CodeData = 0x04,
    Normal = 0x05,
    Shared = 0x06,
    Alias = 0x07,
    AliasCode = 0x08,
    AliasCodeData = 0x09,
    Ipc = 0x0A,
    Stack = 0x0B,
    ThreadLocal = 0x0C,
    Transferred = 0x0D,
    SharedTransferred = 0x0E,
    SharedCode = 0x0F,
    Inaccessible = 0x10,
    NonSecureIpc = 0x11,
    NonDeviceIpc = 0x12,
    Kernel = 0x13,
    GeneratedCode = 0x14,
    CodeOut = 0x15,
    Coverage = 0x16,
    Insecure = 0x17,
}

bitflags! {
    /// Memory attribute flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MemoryAttribute: u32 {
        const LOCKED = 1 << 0;
        const IPC_LOCKED = 1 << 1;
        const DEVICE_SHARED = 1 << 2;
        const UNCACHED = 1 << 3;
        const PERMISSION_LOCKED = 1 << 4;
    }
}

bitflags! {
    /// Memory permission flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MemoryPermission: u32 {
        const NONE = 0;
        const READ = 1 << 0;
        const WRITE = 1 << 1;
        const EXECUTE = 1 << 2;
        const READ_WRITE = Self::READ.bits() | Self::WRITE.bits();
        const READ_EXECUTE = Self::READ.bits() | Self::EXECUTE.bits();
        const DONT_CARE = 1 << 28;
    }
}

/// Signal type for address arbiter.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalType {
    Signal = 0,
    SignalAndIncrementIfEqual = 1,
    SignalAndModifyByWaitingCountIfEqual = 2,
}

/// Arbitration type for address arbiter.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbitrationType {
    WaitIfLessThan = 0,
    DecrementAndWaitIfLessThan = 1,
    WaitIfEqual = 2,
}

/// Yield type for thread yielding.
#[repr(i64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YieldType {
    WithoutCoreMigration = 0,
    WithCoreMigration = -1,
    ToAnyThread = -2,
}

/// Thread exit reason.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadExitReason {
    ExitThread = 0,
    TerminateThread = 1,
    ExitProcess = 2,
    TerminateProcess = 3,
}

/// Thread activity state.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadActivity {
    Runnable = 0,
    Paused = 1,
}

/// Ideal core constants.
pub const IDEAL_CORE_DONT_CARE: i32 = -1;
pub const IDEAL_CORE_USE_PROCESS_VALUE: i32 = -2;
pub const IDEAL_CORE_NO_UPDATE: i32 = -3;

/// Thread priority bounds.
pub const LOWEST_THREAD_PRIORITY: i32 = 63;
pub const HIGHEST_THREAD_PRIORITY: i32 = 0;

/// System thread priority highest.
pub const SYSTEM_THREAD_PRIORITY_HIGHEST: i32 = 16;

/// Process state.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Created = 0,
    CreatedAttached = 1,
    Running = 2,
    Crashed = 3,
    RunningAttached = 4,
    Terminating = 5,
    Terminated = 6,
    DebugBreak = 7,
}

/// Process exit reason.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessExitReason {
    ExitProcess = 0,
    TerminateProcess = 1,
    Exception = 2,
}

/// Thread Local Region size.
pub const THREAD_LOCAL_REGION_SIZE: usize = 0x200;

/// Page info structure.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PageInfo {
    pub flags: u32,
}

/// Info types for svcGetInfo.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoType {
    CoreMask = 0,
    PriorityMask = 1,
    AliasRegionAddress = 2,
    AliasRegionSize = 3,
    HeapRegionAddress = 4,
    HeapRegionSize = 5,
    TotalMemorySize = 6,
    UsedMemorySize = 7,
    DebuggerAttached = 8,
    ResourceLimit = 9,
    IdleTickCount = 10,
    RandomEntropy = 11,
    AslrRegionAddress = 12,
    AslrRegionSize = 13,
    StackRegionAddress = 14,
    StackRegionSize = 15,
    SystemResourceSizeTotal = 16,
    SystemResourceSizeUsed = 17,
    ProgramId = 18,
    InitialProcessIdRange = 19,
    UserExceptionContextAddress = 20,
    TotalNonSystemMemorySize = 21,
    UsedNonSystemMemorySize = 22,
    IsApplication = 23,
    FreeThreadCount = 24,
    ThreadTickCount = 25,
    IsSvcPermitted = 26,
    IoRegionHint = 27,
    TransferMemoryHint = 34,
    Unknown37 = 37,
    Unknown38 = 38,
    MesosphereMeta = 65000,
    MesosphereCurrentProcess = 65001,
}

impl InfoType {
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::CoreMask,
            1 => Self::PriorityMask,
            2 => Self::AliasRegionAddress,
            3 => Self::AliasRegionSize,
            4 => Self::HeapRegionAddress,
            5 => Self::HeapRegionSize,
            6 => Self::TotalMemorySize,
            7 => Self::UsedMemorySize,
            8 => Self::DebuggerAttached,
            9 => Self::ResourceLimit,
            10 => Self::IdleTickCount,
            11 => Self::RandomEntropy,
            12 => Self::AslrRegionAddress,
            13 => Self::AslrRegionSize,
            14 => Self::StackRegionAddress,
            15 => Self::StackRegionSize,
            16 => Self::SystemResourceSizeTotal,
            17 => Self::SystemResourceSizeUsed,
            18 => Self::ProgramId,
            19 => Self::InitialProcessIdRange,
            20 => Self::UserExceptionContextAddress,
            21 => Self::TotalNonSystemMemorySize,
            22 => Self::UsedNonSystemMemorySize,
            23 => Self::IsApplication,
            24 => Self::FreeThreadCount,
            25 => Self::ThreadTickCount,
            26 => Self::IsSvcPermitted,
            27 => Self::IoRegionHint,
            34 => Self::TransferMemoryHint,
            37 => Self::Unknown37,
            38 => Self::Unknown38,
            65000 => Self::MesosphereMeta,
            65001 => Self::MesosphereCurrentProcess,
            _ => {
                log::warn!("Unknown InfoType: {}", v);
                Self::CoreMask
            }
        }
    }
}

bitflags! {
    /// Break reason flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BreakReason: u32 {
        const PANIC = 0;
        const ASSERT = 1;
        const USER = 2;
        const PRE_LOAD_DLL = 3;
        const POST_LOAD_DLL = 4;
        const PRE_UNLOAD_DLL = 5;
        const POST_UNLOAD_DLL = 6;
        const CPP_EXCEPTION = 7;
        const NOTIFICATION_ONLY_FLAG = 0x80000000;
    }
}

/// Debug event type.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugEvent {
    CreateProcess = 0,
    CreateThread = 1,
    ExitProcess = 2,
    ExitThread = 3,
    Exception = 4,
}

/// Debug thread parameter type.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugThreadParam {
    Priority = 0,
    State = 1,
    IdealCore = 2,
    CurrentCore = 3,
    AffinityMask = 4,
}

/// Debug exception type.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugException {
    UndefinedInstruction = 0,
    InstructionAbort = 1,
    DataAbort = 2,
    AlignmentFault = 3,
    DebuggerAttached = 4,
    BreakPoint = 5,
    UserBreak = 6,
    DebuggerBreak = 7,
    UndefinedSystemCall = 8,
    MemorySystemError = 9,
}

/// Debug event flags.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugEventFlag {
    Stopped = 1 << 0,
}

/// Breakpoint type.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakPointType {
    HardwareInstruction = 0,
    HardwareData = 1,
}

/// Hardware breakpoint register name.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareBreakPointRegisterName {
    I0 = 0,
    I1 = 1,
    I2 = 2,
    I3 = 3,
    I4 = 4,
    I5 = 5,
    I6 = 6,
    I7 = 7,
    I8 = 8,
    I9 = 9,
    I10 = 10,
    I11 = 11,
    I12 = 12,
    I13 = 13,
    I14 = 14,
    I15 = 15,
    D0 = 16,
    D1 = 17,
    D2 = 18,
    D3 = 19,
    D4 = 20,
    D5 = 21,
    D6 = 22,
    D7 = 23,
    D8 = 24,
    D9 = 25,
    D10 = 26,
    D11 = 27,
    D12 = 28,
    D13 = 29,
    D14 = 30,
    D15 = 31,
}

/// LP64 (64-bit) structures.
pub mod lp64 {
    use super::*;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct LastThreadContext {
        pub fp: u64,
        pub sp: u64,
        pub lr: u64,
        pub pc: u64,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct PhysicalMemoryInfo {
        pub physical_address: u64,
        pub virtual_address: u64,
        pub size: u64,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct DebugInfoCreateProcess {
        pub program_id: u64,
        pub process_id: u64,
        pub name: [u8; 0xC],
        pub flags: u32,
        pub user_exception_context_address: u64, // 5.0.0+
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct DebugInfoCreateThread {
        pub thread_id: u64,
        pub tls_address: u64,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct DebugInfoExitProcess {
        pub reason: ProcessExitReason,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct DebugInfoExitThread {
        pub reason: ThreadExitReason,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct SecureMonitorArguments {
        pub r: [u64; 8],
    }
    // static_assert: sizeof(SecureMonitorArguments) == 0x40
    const _: () = assert!(std::mem::size_of::<SecureMonitorArguments>() == 0x40);
}

/// ILP32 (32-bit) structures.
pub mod ilp32 {
    use super::*;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct LastThreadContext {
        pub fp: u32,
        pub sp: u32,
        pub lr: u32,
        pub pc: u32,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct PhysicalMemoryInfo {
        pub physical_address: u64,
        pub virtual_address: u32,
        pub size: u32,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct DebugInfoCreateProcess {
        pub program_id: u64,
        pub process_id: u64,
        pub name: [u8; 0xC],
        pub flags: u32,
        pub user_exception_context_address: u32, // 5.0.0+
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct DebugInfoCreateThread {
        pub thread_id: u64,
        pub tls_address: u32,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct DebugInfoExitProcess {
        pub reason: ProcessExitReason,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct DebugInfoExitThread {
        pub reason: ThreadExitReason,
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct SecureMonitorArguments {
        pub r: [u32; 8],
    }
    // static_assert: sizeof(SecureMonitorArguments) == 0x20
    const _: () = assert!(std::mem::size_of::<SecureMonitorArguments>() == 0x20);
}

/// Thread context (AArch64).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ThreadContext {
    pub r: [u64; 29],
    pub fp: u64,
    pub lr: u64,
    pub sp: u64,
    pub pc: u64,
    pub pstate: u32,
    pub padding: u32,
    pub v: [u128; 32],
    pub fpcr: u32,
    pub fpsr: u32,
    pub tpidr: u64,
}
// static_assert: sizeof(ThreadContext) == 0x320
const _: () = assert!(std::mem::size_of::<ThreadContext>() == 0x320);

/// Memory info structure returned by svcQueryMemory.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MemoryInfo {
    pub base_address: u64,
    pub size: u64,
    pub state: MemoryState,
    pub attribute: MemoryAttribute,
    pub permission: MemoryPermission,
    pub ipc_count: u32,
    pub device_count: u32,
    pub padding: u32,
}

/// Limitable resource types.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitableResource {
    PhysicalMemoryMax = 0,
    ThreadCountMax = 1,
    EventCountMax = 2,
    TransferMemoryCountMax = 3,
    SessionCountMax = 4,
    Count = 5,
}

/// IO pool type.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoPoolType {
    // Not supported.
    Count = 0,
}

/// Memory mapping type.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryMapping {
    IoRegister = 0,
    Uncached = 1,
    Memory = 2,
}

bitflags! {
    /// Map device address space flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MapDeviceAddressSpaceFlag: u32 {
        const NONE = 0;
        const NOT_IO_REGISTER = 1 << 0;
    }
}

/// Kernel debug type.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelDebugType {
    Thread = 0,
    ThreadCallStack = 1,
    KernelObject = 2,
    Handle = 3,
    Memory = 4,
    PageTable = 5,
    CpuUtilization = 6,
    Process = 7,
    SuspendProcess = 8,
    ResumeProcess = 9,
    Port = 10,
}

/// Kernel trace state.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelTraceState {
    Disabled = 0,
    Enabled = 1,
}

/// Code memory operation.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeMemoryOperation {
    Map = 0,
    MapToOwner = 1,
    Unmap = 2,
    UnmapFromOwner = 3,
}

/// Interrupt type.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptType {
    Edge = 0,
    Level = 1,
}

/// Device name enumeration.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceName {
    Afi = 0,
    Avpc = 1,
    Dc = 2,
    Dcb = 3,
    Hc = 4,
    Hda = 5,
    Isp2 = 6,
    MsencNvenc = 7,
    Nv = 8,
    Nv2 = 9,
    Ppcs = 10,
    Sata = 11,
    Vi = 12,
    Vic = 13,
    XusbHost = 14,
    XusbDev = 15,
    Tsec = 16,
    Ppcs1 = 17,
    Dc1 = 18,
    Sdmmc1a = 19,
    Sdmmc2a = 20,
    Sdmmc3a = 21,
    Sdmmc4a = 22,
    Isp2b = 23,
    Gpu = 24,
    Gpub = 25,
    Ppcs2 = 26,
    Nvdec = 27,
    Ape = 28,
    Se = 29,
    Nvjpg = 30,
    Hc1 = 31,
    Se1 = 32,
    Axiap = 33,
    Etr = 34,
    Tsecb = 35,
    Tsec1 = 36,
    Tsecb1 = 37,
    Nvdec1 = 38,
    Count = 39,
}

/// System info type.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemInfoType {
    TotalPhysicalMemorySize = 0,
    UsedPhysicalMemorySize = 1,
    InitialProcessIdRange = 2,
}

/// Process info type.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessInfoType {
    ProcessState = 0,
}

/// Process activity state.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessActivity {
    Runnable = 0,
    Paused = 1,
}

bitflags! {
    /// Create process flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateProcessFlag: u32 {
        const IS_64_BIT = 1 << 0;

        // Address space type (bits 1-3)
        const ADDRESS_SPACE_32_BIT = 0 << 1;
        const ADDRESS_SPACE_64_BIT_DEPRECATED = 1 << 1;
        const ADDRESS_SPACE_32_BIT_WITHOUT_ALIAS = 2 << 1;
        const ADDRESS_SPACE_64_BIT = 3 << 1;

        const ENABLE_DEBUG = 1 << 4;
        const ENABLE_ASLR = 1 << 5;
        const IS_APPLICATION = 1 << 6;

        // 4.x deprecated
        const DEPRECATED_USE_SECURE_MEMORY = 1 << 7;

        // 5.x+ pool partition (bits 7-10)
        const POOL_PARTITION_APPLICATION = 0 << 7;
        const POOL_PARTITION_APPLET = 1 << 7;
        const POOL_PARTITION_SYSTEM = 2 << 7;
        const POOL_PARTITION_SYSTEM_NON_SECURE = 3 << 7;

        // 7.x+
        const OPTIMIZE_MEMORY_ALLOCATION = 1 << 11;

        // 11.x+
        const DISABLE_DEVICE_ADDRESS_SPACE_MERGE = 1 << 12;
    }
}

/// Address space shift and mask constants for CreateProcessFlag.
pub const ADDRESS_SPACE_SHIFT: u32 = 1;
pub const ADDRESS_SPACE_MASK: u32 = 7 << ADDRESS_SPACE_SHIFT;
pub const POOL_PARTITION_SHIFT: u32 = 7;
pub const POOL_PARTITION_MASK: u32 = 0xF << POOL_PARTITION_SHIFT;

/// Create process parameter structure.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CreateProcessParameter {
    pub name: [u8; 12],
    pub version: u32,
    pub program_id: u64,
    pub code_address: u64,
    pub code_num_pages: i32,
    pub flags: CreateProcessFlag,
    pub reslimit: Handle,
    pub system_resource_num_pages: i32,
}
// static_assert: sizeof(CreateProcessParameter) == 0x30
const _: () = assert!(std::mem::size_of::<CreateProcessParameter>() == 0x30);

/// Number of supervisor calls.
pub const NUM_SUPERVISOR_CALLS: usize = 0xC0;

/// SVC access flag set (bitset of allowed SVCs).
/// Mirrors `std::bitset<NumSupervisorCalls>`.
#[derive(Debug, Clone)]
pub struct SvcAccessFlagSet {
    bits: [u64; 3], // 192 bits = 3 x u64
}

impl SvcAccessFlagSet {
    pub fn new() -> Self {
        Self { bits: [0; 3] }
    }

    pub fn set(&mut self, index: usize, value: bool) {
        debug_assert!(index < NUM_SUPERVISOR_CALLS);
        let word = index / 64;
        let bit = index % 64;
        if value {
            self.bits[word] |= 1u64 << bit;
        } else {
            self.bits[word] &= !(1u64 << bit);
        }
    }

    pub fn test(&self, index: usize) -> bool {
        debug_assert!(index < NUM_SUPERVISOR_CALLS);
        let word = index / 64;
        let bit = index % 64;
        (self.bits[word] & (1u64 << bit)) != 0
    }
}

impl Default for SvcAccessFlagSet {
    fn default() -> Self {
        Self::new()
    }
}

/// Initial process ID range info.
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitialProcessIdRangeInfo {
    Minimum = 0,
    Maximum = 1,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_state_values() {
        assert_eq!(MemoryState::Free as u32, 0x00);
        assert_eq!(MemoryState::Insecure as u32, 0x17);
    }

    #[test]
    fn test_memory_permission_flags() {
        assert_eq!(MemoryPermission::READ_WRITE.bits(), 0b11);
        assert_eq!(MemoryPermission::READ_EXECUTE.bits(), 0b101);
    }

    #[test]
    fn test_thread_context_size() {
        assert_eq!(std::mem::size_of::<ThreadContext>(), 0x320);
    }

    #[test]
    fn test_secure_monitor_arguments_lp64() {
        assert_eq!(std::mem::size_of::<lp64::SecureMonitorArguments>(), 0x40);
    }

    #[test]
    fn test_secure_monitor_arguments_ilp32() {
        assert_eq!(std::mem::size_of::<ilp32::SecureMonitorArguments>(), 0x20);
    }

    #[test]
    fn test_create_process_parameter_size() {
        assert_eq!(std::mem::size_of::<CreateProcessParameter>(), 0x30);
    }

    #[test]
    fn test_svc_access_flag_set() {
        let mut flags = SvcAccessFlagSet::new();
        assert!(!flags.test(0));
        flags.set(0, true);
        assert!(flags.test(0));
        flags.set(191, true);
        assert!(flags.test(191));
        flags.set(0, false);
        assert!(!flags.test(0));
    }

    #[test]
    fn test_ideal_core_constants() {
        assert_eq!(IDEAL_CORE_DONT_CARE, -1);
        assert_eq!(IDEAL_CORE_USE_PROCESS_VALUE, -2);
        assert_eq!(IDEAL_CORE_NO_UPDATE, -3);
    }

    #[test]
    fn test_thread_priority_bounds() {
        assert_eq!(LOWEST_THREAD_PRIORITY, 63);
        assert_eq!(HIGHEST_THREAD_PRIORITY, 0);
        assert_eq!(SYSTEM_THREAD_PRIORITY_HIGHEST, 16);
    }
}
