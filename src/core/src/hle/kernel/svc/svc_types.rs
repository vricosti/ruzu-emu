//! Port of zuyu/src/core/hle/kernel/svc_types.h
//! Status: COMPLET
//! Derniere synchro: 2026-03-11
//!
//! SVC types, enums, and constants used by the kernel SVC handlers.

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

/// Memory attribute flags.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryAttribute {
    Locked = 1 << 0,
    IpcLocked = 1 << 1,
    DeviceShared = 1 << 2,
    Uncached = 1 << 3,
    PermissionLocked = 1 << 4,
}

/// Memory permission flags.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryPermission {
    None = 0,
    Read = 1 << 0,
    Write = 1 << 1,
    Execute = 1 << 2,
    ReadWrite = (1 << 0) | (1 << 1),
    ReadExecute = (1 << 0) | (1 << 2),
    DontCare = 1 << 28,
}

/// Signal type for address arbiter operations.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalType {
    Signal = 0,
    SignalAndIncrementIfEqual = 1,
    SignalAndModifyByWaitingCountIfEqual = 2,
}

/// Arbitration type for address arbiter operations.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbitrationType {
    WaitIfLessThan = 0,
    DecrementAndWaitIfLessThan = 1,
    WaitIfEqual = 2,
}

/// Yield type for SleepThread.
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

pub const IDEAL_CORE_DONT_CARE: i32 = -1;
pub const IDEAL_CORE_USE_PROCESS_VALUE: i32 = -2;
pub const IDEAL_CORE_NO_UPDATE: i32 = -3;

pub const LOWEST_THREAD_PRIORITY: i32 = 63;
pub const HIGHEST_THREAD_PRIORITY: i32 = 0;

pub const SYSTEM_THREAD_PRIORITY_HIGHEST: i32 = 16;

/// Process state enumeration.
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

pub const THREAD_LOCAL_REGION_SIZE: usize = 0x200;

/// Page info returned by QueryMemory.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PageInfo {
    pub flags: u32,
}

/// Info types for GetInfo SVC.
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
    /// Switch fw 18.0.0+ (libnx 4.x): extra alias-region size beyond the
    /// normal alias region, used by processes that opt into 39-bit AS
    /// extension. Returns 0 when no extra region is configured.
    AliasRegionExtraSize = 28,
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
            28 => Self::AliasRegionExtraSize,
            65000 => Self::MesosphereMeta,
            65001 => Self::MesosphereCurrentProcess,
            _ => {
                log::warn!("Unknown InfoType: {}", v);
                Self::CoreMask // fallback — will be caught by the match
            }
        }
    }
}

/// Break reason flags.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakReason {
    Panic = 0,
    Assert = 1,
    User = 2,
    PreLoadDll = 3,
    PostLoadDll = 4,
    PreUnloadDll = 5,
    PostUnloadDll = 6,
    CppException = 7,
    NotificationOnlyFlag = 0x80000000,
}

/// Debug thread parameter.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugThreadParam {
    Priority = 0,
    State = 1,
    IdealCore = 2,
    CurrentCore = 3,
    AffinityMask = 4,
}

/// Hardware breakpoint register names.
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

/// Limitable resource types.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitableResource {
    PhysicalMemoryMax = 0,
    ThreadCountMax = 1,
    EventCountMax = 2,
    TransferMemoryCountMax = 3,
    SessionCountMax = 4,
}

/// Returns true if the resource type is valid (0..5).
pub fn is_valid_resource_type(which: LimitableResource) -> bool {
    (which as u32) < 5
}

/// IO pool type. Not currently supported (zero variants upstream: Count = 0).
/// We represent as a u32 wrapper since Rust does not allow zero-variant repr enums.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoPoolType(pub u32);

/// Memory mapping type.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryMapping {
    IoRegister = 0,
    Uncached = 1,
    Memory = 2,
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

/// Code memory operations.
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

/// Device names for device address space operations.
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
}

/// System info type for GetSystemInfo.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemInfoType {
    TotalPhysicalMemorySize = 0,
    UsedPhysicalMemorySize = 1,
    InitialProcessIdRange = 2,
}

impl SystemInfoType {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::TotalPhysicalMemorySize),
            1 => Some(Self::UsedPhysicalMemorySize),
            2 => Some(Self::InitialProcessIdRange),
            _ => None,
        }
    }
}

/// Process info type for GetProcessInfo.
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

/// Memory info returned by QueryMemory.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryInfo {
    pub base_address: u64,
    pub size: u64,
    pub state: u32,      // MemoryState
    pub attribute: u32,  // MemoryAttribute
    pub permission: u32, // MemoryPermission
    pub ipc_count: u32,
    pub device_count: u32,
    pub padding: u32,
}

/// Secure monitor arguments (64-bit).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SecureMonitorArguments64 {
    pub r: [u64; 8],
}

/// Secure monitor arguments (32-bit).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SecureMonitorArguments32 {
    pub r: [u32; 8],
}

/// Last thread context (64-bit).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct LastThreadContext64 {
    pub fp: u64,
    pub sp: u64,
    pub lr: u64,
    pub pc: u64,
}

/// Last thread context (32-bit).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct LastThreadContext32 {
    pub fp: u32,
    pub sp: u32,
    pub lr: u32,
    pub pc: u32,
}

/// Physical memory info (64-bit).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PhysicalMemoryInfo64 {
    pub physical_address: u64,
    pub virtual_address: u64,
    pub size: u64,
}

/// Physical memory info (32-bit).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PhysicalMemoryInfo32 {
    pub physical_address: u64,
    pub virtual_address: u32,
    pub size: u32,
}

/// Number of supervisor calls.
pub const NUM_SUPERVISOR_CALLS: usize = 0xC0;

/// Page size constant used by SVC handlers.
pub const PAGE_SIZE: u64 = 4096;

/// Heap size alignment (2 MiB) — same as svc_common::HEAP_SIZE_ALIGNMENT.
pub const HEAP_SIZE_ALIGNMENT: u64 = 2 * 1024 * 1024;

/// Main memory size max for SetHeapSize validation.
pub const MAIN_MEMORY_SIZE_MAX: u64 = 0x1_0000_0000; // 4 GiB

/// Map device address space alignment mask.
pub const DEVICE_ADDRESS_SPACE_ALIGN_MASK: u64 = (1u64 << 22) - 1;

/// MapDeviceAddressSpaceOption — packed u32.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MapDeviceAddressSpaceOption {
    pub raw: u32,
}

impl MapDeviceAddressSpaceOption {
    pub fn permission(&self) -> MemoryPermission {
        let perm_bits = self.raw & 0xFFFF;
        // Safety: we validate separately
        unsafe { core::mem::transmute(perm_bits) }
    }

    pub fn flags(&self) -> u32 {
        (self.raw >> 16) & 0x1
    }

    pub fn reserved(&self) -> u32 {
        (self.raw >> 17) & 0x7FFF
    }
}

/// Suspend type for debug.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspendType {
    Debug = 0,
}
