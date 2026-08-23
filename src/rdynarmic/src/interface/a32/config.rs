use std::sync::Arc;

use super::coprocessor::Coprocessor;

/// The 16 configurable A32 coprocessor slots from `A32::UserConfig`.
///
/// Upstream owner: `interface/A32/config.h::UserConfig::coprocessors`.
pub type Coprocessors = [Option<Arc<dyn Coprocessor>>; 16];

pub fn empty_coprocessors() -> Coprocessors {
    [const { None }; 16]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_has_all_sixteen_upstream_slots() {
        let registry = empty_coprocessors();
        assert_eq!(registry.len(), 16);
        assert!(registry.iter().all(Option::is_none));
    }
}
