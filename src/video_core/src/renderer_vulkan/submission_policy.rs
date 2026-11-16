// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Lichii `video_core/renderer_vulkan/submission_policy.h`.
//!
//! Lichii extension (absent from Eden, whose `RasterizerVulkan::FlushWork`
//! dispatches every eighth operation and submits at 4096): a title using the
//! native (NX2) tiling submits every `DEFAULT_NATIVE_BUDGET` draws and
//! dispatches every 32 on desktop hosts. Other titles dispatch every 32 and
//! submit at the legacy budget (4096, 128 on MoltenVK).

/// Lichii `SubmissionPolicy::DefaultNativeBudget`.
pub const DEFAULT_NATIVE_BUDGET: u32 = 128;

/// Lichii `SubmissionPolicy::Action`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Dispatch,
    Flush,
}

/// Lichii `SubmissionPolicy::ParseBudget`: a whole decimal `u32` (as
/// `std::from_chars` takes it: digits only, no sign or space), zero rejected.
pub fn parse_budget(value: &str) -> Option<u32> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    match value.parse::<u32>() {
        Ok(0) | Err(_) => None,
        Ok(budget) => Some(budget),
    }
}

/// Lichii `SubmissionPolicy::Advance`, with its default arguments spelled out
/// by the caller (`native_budget = DEFAULT_NATIVE_BUDGET`,
/// `legacy_budget = 4096`).
pub fn advance(
    counter: &mut u32,
    native: bool,
    android: bool,
    native_budget: u32,
    legacy_budget: u32,
) -> Action {
    *counter = counter.wrapping_add(1);
    if native && !android {
        if *counter >= native_budget {
            *counter = 0;
            return Action::Flush;
        }
        return if *counter % 32 == 0 { Action::Dispatch } else { Action::None };
    }
    let check_mask: u32 = if android { 3 } else { 31 };
    if (*counter & check_mask) != check_mask {
        return Action::None;
    }
    if *counter < (if android { 512 } else { legacy_budget }) {
        return Action::Dispatch;
    }
    *counter = 0;
    Action::Flush
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(native: bool, android: bool, native_budget: u32, legacy_budget: u32, calls: u32) -> Vec<(u32, Action)> {
        let mut counter = 0;
        (1..=calls)
            .filter_map(|call| {
                let action = advance(&mut counter, native, android, native_budget, legacy_budget);
                (action != Action::None).then_some((call, action))
            })
            .collect()
    }

    #[test]
    fn native_desktop_dispatches_every_32_and_flushes_at_the_budget() {
        let actions = run(true, false, DEFAULT_NATIVE_BUDGET, 4096, 256);
        assert_eq!(
            actions,
            vec![
                (32, Action::Dispatch),
                (64, Action::Dispatch),
                (96, Action::Dispatch),
                (128, Action::Flush),
                (160, Action::Dispatch),
                (192, Action::Dispatch),
                (224, Action::Dispatch),
                (256, Action::Flush),
            ]
        );
    }

    #[test]
    fn native_budget_below_32_flushes_without_dispatching() {
        let actions = run(true, false, 10, 4096, 30);
        assert_eq!(actions, vec![(10, Action::Flush), (20, Action::Flush), (30, Action::Flush)]);
    }

    #[test]
    fn native_budget_not_a_multiple_of_32_restarts_the_dispatch_count_after_a_flush() {
        // Lichii resets the counter on Flush, so the dispatch cadence restarts.
        let actions = run(true, false, 40, 4096, 80);
        assert_eq!(
            actions,
            vec![(32, Action::Dispatch), (40, Action::Flush), (72, Action::Dispatch), (80, Action::Flush)]
        );
    }

    #[test]
    fn legacy_desktop_dispatches_every_32_and_flushes_at_the_legacy_budget() {
        let actions = run(false, false, DEFAULT_NATIVE_BUDGET, 4096, 4096 + 32);
        // The counter tests `& 31 == 31`, so a dispatch lands on 31, 63, ...
        assert_eq!(actions.first(), Some(&(31, Action::Dispatch)));
        assert_eq!(actions[1], (63, Action::Dispatch));
        let flushes: Vec<_> = actions.iter().filter(|(_, action)| *action == Action::Flush).collect();
        // 4095 is still below 4096: a dispatch.
        assert!(actions.contains(&(4095, Action::Dispatch)));
        // The counter reaches 4127 (& 31 == 31, >= 4096) only 32 calls later.
        assert_eq!(flushes, vec![&(4127, Action::Flush)]);
        assert_eq!(actions.len(), 4096 / 32 + 1);
    }

    #[test]
    fn legacy_desktop_moltenvk_budget_flushes_at_159() {
        let actions = run(false, false, DEFAULT_NATIVE_BUDGET, 128, 160);
        assert_eq!(
            actions,
            vec![
                (31, Action::Dispatch),
                (63, Action::Dispatch),
                (95, Action::Dispatch),
                (127, Action::Dispatch),
                (159, Action::Flush),
            ]
        );
    }

    #[test]
    fn android_keeps_edens_cadence_even_for_native_titles() {
        for native in [false, true] {
            let actions = run(native, true, DEFAULT_NATIVE_BUDGET, 4096, 520);
            assert_eq!(actions.first(), Some(&(3, Action::Dispatch)));
            assert_eq!(actions[1], (7, Action::Dispatch));
            assert!(actions.contains(&(511, Action::Dispatch)));
            assert!(actions.contains(&(515, Action::Flush)));
        }
    }

    #[test]
    fn parse_budget_matches_from_chars() {
        assert_eq!(parse_budget("128"), Some(128));
        assert_eq!(parse_budget("4096"), Some(4096));
        assert_eq!(parse_budget("4294967295"), Some(u32::MAX));
        assert_eq!(parse_budget(""), None);
        assert_eq!(parse_budget("0"), None);
        assert_eq!(parse_budget("+12"), None);
        assert_eq!(parse_budget("-12"), None);
        assert_eq!(parse_budget(" 12"), None);
        assert_eq!(parse_budget("12 "), None);
        assert_eq!(parse_budget("12a"), None);
        assert_eq!(parse_budget("4294967296"), None);
    }
}
