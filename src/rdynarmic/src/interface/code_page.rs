//! Executable guest-code page declaration from upstream `interface/code_page.h`.

/// Smallest valid code page.
pub const CODE_PAGE_SIZE: u64 = 4096;

#[repr(C)]
pub struct CodePage {
    pub inst: [u32; CODE_PAGE_SIZE as usize / std::mem::size_of::<u32>()],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_upstream_code_page() {
        assert_eq!(std::mem::size_of::<CodePage>(), CODE_PAGE_SIZE as usize);
        assert_eq!(
            std::mem::align_of::<CodePage>(),
            std::mem::align_of::<u32>()
        );
    }
}
