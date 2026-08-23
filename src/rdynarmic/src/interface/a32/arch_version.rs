/// Guest A32 architecture version selected by the JIT configuration.
///
/// Upstream owner: `interface/A32/arch_version.h::ArchVersion`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ArchVersion {
    V3,
    V4,
    V4T,
    V5TE,
    V6K,
    V6T2,
    V7,
    #[default]
    V8,
}
