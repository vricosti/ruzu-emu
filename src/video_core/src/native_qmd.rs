// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of header-only `video_core/native_qmd.h`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub type Word = u32;
pub type Address = u64;
pub type Descriptor = [Word; 64];

// These adapters express upstream's compile-time callback arity/type checks.
pub trait ReleaseCallback {
    const SUPPORTS_REDUCTION: bool;
    fn release(&mut self, address: Address, payload: Word, flags: Word) -> bool;
}

pub struct PlainRelease<F>(pub F);
impl<F: FnMut(Address, Word) -> bool> ReleaseCallback for PlainRelease<F> {
    const SUPPORTS_REDUCTION: bool = false;
    fn release(&mut self, address: Address, payload: Word, _: Word) -> bool {
        (self.0)(address, payload)
    }
}

pub struct FlaggedRelease<F>(pub F);
impl<F: FnMut(Address, Word, Word) -> bool> ReleaseCallback for FlaggedRelease<F> {
    const SUPPORTS_REDUCTION: bool = true;
    fn release(&mut self, address: Address, payload: Word, flags: Word) -> bool {
        (self.0)(address, payload, flags)
    }
}

pub trait ShaderDispatch {
    const REJECT_SHADER_DISPATCH: bool;
    fn dispatch(&mut self, descriptor: &Descriptor) -> bool;
}

pub struct RejectShaderDispatch;
impl ShaderDispatch for RejectShaderDispatch {
    const REJECT_SHADER_DISPATCH: bool = true;
    fn dispatch(&mut self, _: &Descriptor) -> bool {
        false
    }
}

impl<F: FnMut(&Descriptor) -> bool> ShaderDispatch for F {
    const REJECT_SHADER_DISPATCH: bool = false;
    fn dispatch(&mut self, descriptor: &Descriptor) -> bool {
        self(descriptor)
    }
}

struct State {
    descriptor: Descriptor,
    dependencies: i32,
    deferred: i32,
    loaded: bool,
    scheduled: bool,
    completed: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            descriptor: [0; 64],
            dependencies: 0,
            deferred: 0,
            loaded: false,
            scheduled: false,
            completed: false,
        }
    }
}

type Trace = Box<dyn FnMut(&'static str, Address, i32) + Send>;

#[derive(Default)]
pub struct NativeQmdScheduler {
    pub trace: Option<Trace>,
}

impl NativeQmdScheduler {
    // NVN2 also patches the third slot for interqueue synchronization.
    const RELEASE_SLOTS: [usize; 3] = [24, 28, 52];

    fn states() -> &'static Mutex<HashMap<Address, State>> {
        // Upstream's static table is shared by ALL channel schedulers. The GPU
        // serializes Signal calls; the mutex makes that shared ownership explicit
        // in Rust. Callbacks must not recursively call Signal (the current
        // upstream read/release/dispatch callbacks do not do so).
        static STATES: OnceLock<Mutex<HashMap<Address, State>>> = OnceLock::new();
        STATES.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub fn signal<R: ReleaseCallback>(
        &mut self,
        address: Address,
        argument: Word,
        read: impl FnMut(Address, &mut Descriptor) -> bool,
        release: R,
    ) -> &'static str {
        self.signal_with_dispatch(address, argument, read, release, RejectShaderDispatch)
    }

    pub fn signal_with_dispatch<R: ReleaseCallback, D: ShaderDispatch>(
        &mut self,
        address: Address,
        argument: Word,
        mut read: impl FnMut(Address, &mut Descriptor) -> bool,
        mut release: R,
        mut dispatch: D,
    ) -> &'static str {
        #[derive(Clone, Copy, Default)]
        struct Pending {
            address: Address,
            action: Word,
            count: Word,
        }
        let mut error = "";
        let mut pending = [Pending::default(); 256];
        let mut count = 1;
        pending[0] = Pending {
            address,
            action: argument & 15,
            count: ((argument >> 14) & 0x3ff) + 1,
        };
        let mut states = Self::states()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut i = 0;
        while i < count {
            let request = pending[i];
            i += 1;
            if states.len() >= 4096 && !states.contains_key(&request.address) {
                return "QMD cache limit";
            }
            let state = states.entry(request.address).or_default();
            if request.action == 3 || request.action == 2 {
                if request.action == 3 || !state.loaded {
                    if !read(request.address, &mut state.descriptor) {
                        return "unmapped QMD";
                    }
                    let qmd = &state.descriptor;
                    if qmd[18] & 0xff != 0x30 || qmd[4] & 0x100 != 0 {
                        return "unsupported QMD version or queue";
                    }
                    if D::REJECT_SHADER_DISPATCH
                        && qmd[12] != 0
                        && qmd[13] & 0xffff != 0
                        && qmd[14] & 0xffff != 0
                    {
                        return "native shader dispatch required";
                    }
                    state.dependencies = ((qmd[4] >> 14) & 0xffff) as i32 + state.deferred;
                    if let Some(trace) = &mut self.trace {
                        trace("base", request.address, ((qmd[4] >> 14) & 0xffff) as i32);
                    }
                    if let Some(trace) = &mut self.trace {
                        trace("load", request.address, state.dependencies);
                    }
                    state.deferred = 0;
                    state.loaded = true;
                    state.completed = false;
                }
                state.scheduled = true;
            } else if request.action == 7 || request.action == 12 {
                let delta = if request.action == 7 {
                    -(request.count as i32)
                } else {
                    request.count as i32
                };
                if !state.loaded || state.completed {
                    state.deferred += delta;
                    if let Some(trace) = &mut self.trace {
                        trace("defer", request.address, state.deferred);
                    }
                    continue;
                }
                state.dependencies += delta;
                if let Some(trace) = &mut self.trace {
                    trace("count", request.address, state.dependencies);
                }
            } else if request.action == 0 || request.action == 1 {
                continue;
            } else {
                return "unsupported PCAS action";
            }
            if !state.loaded || !state.scheduled || state.completed || state.dependencies > 0 {
                continue;
            }
            if state.dependencies < 0 {
                return "QMD dependency underflow";
            }
            // Payloads may be patched while waiting: fetch again at execution.
            if !read(request.address, &mut state.descriptor) {
                return "unmapped QMD";
            }
            let qmd = state.descriptor;
            if qmd[18] & 0xff != 0x30 || qmd[4] & 0x100 != 0 {
                return "unsupported QMD version or queue";
            }
            for base in Self::RELEASE_SLOTS {
                let flags = qmd[base + 1];
                if flags & (1 << 23) == 0 {
                    continue;
                }
                let add = flags & (1 << 26) != 0;
                if flags & (0x3b << 24) != 0 || flags >> 30 != 1 || (add && flags & (7 << 20) != 0)
                {
                    return "unsupported QMD release format";
                }
                if add && !R::SUPPORTS_REDUCTION {
                    return "unsupported QMD reduction callback";
                }
            }
            // Preserve upstream completion/release even if its backend cannot
            // run a shader; report the failure rather than claiming execution.
            let dispatched = !(qmd[12] != 0 && qmd[13] & 0xffff != 0 && qmd[14] & 0xffff != 0)
                || dispatch.dispatch(&qmd);
            if !dispatched {
                error = "native shader dispatch required";
            }
            if let Some(trace) = &mut self.trace {
                trace("run", request.address, i32::from(dispatched));
            }
            state.completed = true;
            for base in Self::RELEASE_SLOTS {
                let flags = qmd[base + 1];
                if flags & (1 << 23) == 0 {
                    continue;
                }
                let destination = (u64::from(flags & 0xff) << 32) | u64::from(qmd[base]);
                if !release.release(destination, qmd[base + 2], flags) {
                    return "unmapped QMD release";
                }
            }
            if qmd[16] & 1 != 0 {
                let action = (qmd[16] >> 1) & 7;
                if action != 4 && action != 3 && action != 1 {
                    return "unsupported dependent QMD action";
                }
                if count == pending.len() {
                    return "QMD chain limit";
                }
                pending[count] = Pending {
                    address: u64::from(qmd[15]) << 8,
                    action: if action == 4 {
                        7
                    } else if action == 3 {
                        3
                    } else {
                        2
                    },
                    count: 1,
                };
                count += 1;
            }
            if qmd[16] & (1 << 5) != 0 {
                return "second dependent QMD unsupported";
            }
        }
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::{Arc, MutexGuard};

    fn reset() -> MutexGuard<'static, ()> {
        static TEST_LOCK: Mutex<()> = Mutex::new(());
        let guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        NativeQmdScheduler::states().lock().unwrap().clear();
        guard
    }

    fn descriptor(dependencies: u32) -> Descriptor {
        let mut qmd = [0; 64];
        qmd[18] = 0x30;
        qmd[4] = dependencies << 14;
        qmd
    }

    fn set_release(qmd: &mut Descriptor, base: usize, address: u64, payload: u32) {
        qmd[base] = address as u32;
        qmd[base + 1] = (address >> 32) as u32 | (1 << 23) | (1 << 30);
        qmd[base + 2] = payload;
    }

    #[test]
    fn channels_share_early_decrements_and_reload_patched_release_at_execution() {
        let _guard = reset();
        let mut first = NativeQmdScheduler::default();
        let mut second = NativeQmdScheduler::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        for scheduler in [&mut first, &mut second] {
            let events = events.clone();
            scheduler.trace = Some(Box::new(move |event, address, value| {
                events.lock().unwrap().push((event, address, value));
            }));
        }
        assert_eq!(
            first.signal(
                0x1000,
                7,
                |_, _| panic!("premature read"),
                PlainRelease(|_, _| true)
            ),
            ""
        );
        let qmd = RefCell::new(descriptor(2));
        let reads = RefCell::new(0);
        let read = |address, out: &mut Descriptor| {
            assert_eq!(address, 0x1000);
            *reads.borrow_mut() += 1;
            *out = *qmd.borrow();
            true
        };
        assert_eq!(
            second.signal(
                0x1000,
                3,
                read,
                PlainRelease(|_, _| panic!("premature release"))
            ),
            ""
        );
        set_release(&mut qmd.borrow_mut(), 52, 0xab_12345678, 42);
        let mut releases = Vec::new();
        assert_eq!(
            first.signal(
                0x1000,
                7,
                read,
                PlainRelease(|a, p| {
                    releases.push((a, p));
                    true
                })
            ),
            ""
        );
        assert_eq!(*reads.borrow(), 2);
        assert_eq!(releases, [(0xab_12345678, 42)]);
        assert_eq!(
            *events.lock().unwrap(),
            [
                ("defer", 0x1000, -1),
                ("base", 0x1000, 2),
                ("load", 0x1000, 1),
                ("count", 0x1000, 0),
                ("run", 0x1000, 1),
            ]
        );
        // A completed descriptor defers new increments until a forced reload.
        assert_eq!(
            first.signal(0x1000, 12, read, PlainRelease(|_, _| true)),
            ""
        );
        assert_eq!(
            second.signal(
                0x1000,
                2,
                |_, _| panic!("action 2 reload"),
                PlainRelease(|_, _| true)
            ),
            ""
        );
        qmd.borrow_mut()[4] = 0;
        assert_eq!(
            second.signal(
                0x1000,
                3,
                read,
                PlainRelease(|_, _| panic!("deferred increment lost"))
            ),
            ""
        );
        assert_eq!(
            NativeQmdScheduler::states().lock().unwrap()[&0x1000].dependencies,
            1
        );
    }

    #[test]
    fn validate_all_slots_before_dispatch_and_complete_before_release_failure() {
        let _guard = reset();
        let mut scheduler = NativeQmdScheduler::default();
        let mut qmd = descriptor(0);
        for base in [24, 28, 52] {
            set_release(&mut qmd, base, base as u64, base as u32);
        }
        qmd[53] |= 1 << 24;
        assert_eq!(
            scheduler.signal(
                1,
                3,
                |_, out| {
                    *out = qmd;
                    true
                },
                PlainRelease(|_, _| panic!("partial release"))
            ),
            "unsupported QMD release format"
        );
        qmd[53] &= !(1 << 24);
        let mut released = Vec::new();
        assert_eq!(
            scheduler.signal(
                1,
                2,
                |_, out| {
                    *out = qmd;
                    true
                },
                PlainRelease(|address, payload| {
                    released.push((address, payload));
                    address != 28
                })
            ),
            "unmapped QMD release"
        );
        assert_eq!(released, [(24, 24), (28, 28)]);
        assert!(NativeQmdScheduler::states().lock().unwrap()[&1].completed);
    }

    #[test]
    fn dispatch_callback_failure_releases_but_default_rejection_does_not_load() {
        let _guard = reset();
        let mut scheduler = NativeQmdScheduler::default();
        let mut qmd = descriptor(0);
        qmd[12] = 1;
        qmd[13] = 1;
        qmd[14] = 1;
        set_release(&mut qmd, 24, 0x55, 91);
        let read = |_, out: &mut Descriptor| {
            *out = qmd;
            true
        };
        assert_eq!(
            scheduler.signal(1, 3, read, PlainRelease(|_, _| panic!("default dispatch"))),
            "native shader dispatch required"
        );
        assert!(!NativeQmdScheduler::states().lock().unwrap()[&1].loaded);
        let mut calls = Vec::new();
        assert_eq!(
            scheduler.signal_with_dispatch(
                1,
                3,
                read,
                FlaggedRelease(|a, p, f| {
                    calls.push((a, p, f));
                    true
                }),
                |received: &Descriptor| {
                    assert_eq!(*received, qmd);
                    false
                }
            ),
            "native shader dispatch required"
        );
        assert_eq!(calls, [(0x55, 91, qmd[25])]);
        assert!(NativeQmdScheduler::states().lock().unwrap()[&1].completed);
        // A high-only dimension is an empty grid, so no dispatch is required.
        qmd[13] = 0x10000;
        assert_eq!(
            scheduler.signal(
                2,
                3,
                |_, out| {
                    *out = qmd;
                    true
                },
                PlainRelease(|_, _| true)
            ),
            ""
        );
    }

    #[test]
    fn reduction_requires_flagged_callback_and_rejects_unsupported_formats() {
        let _guard = reset();
        let mut scheduler = NativeQmdScheduler::default();
        let mut qmd = descriptor(0);
        set_release(&mut qmd, 24, 0x99, 7);
        qmd[25] |= 1 << 26;
        assert_eq!(
            scheduler.signal(
                1,
                3,
                |_, out| {
                    *out = qmd;
                    true
                },
                PlainRelease(|_, _| true)
            ),
            "unsupported QMD reduction callback"
        );
        let mut calls = 0;
        assert_eq!(
            scheduler.signal(
                1,
                3,
                |_, out| {
                    *out = qmd;
                    true
                },
                FlaggedRelease(|a, p, flags| {
                    assert_eq!((a, p, flags), (0x99, 7, qmd[25]));
                    calls += 1;
                    true
                })
            ),
            ""
        );
        assert_eq!(calls, 1);
        for bit in [20, 21, 22, 24, 25, 27, 28, 29, 31] {
            let mut invalid = qmd;
            invalid[25] |= 1 << bit;
            assert_eq!(
                scheduler.signal(
                    1,
                    3,
                    |_, out| {
                        *out = invalid;
                        true
                    },
                    FlaggedRelease(|_, _, _| panic!("invalid reduction"))
                ),
                "unsupported QMD release format"
            );
        }
    }

    #[test]
    fn dependent_actions_preserve_chain_order_and_limit() {
        let _guard = reset();
        let mut scheduler = NativeQmdScheduler::default();
        for (index, action) in [1, 3, 4].into_iter().enumerate() {
            let parent = 0x1000 + (index as u64) * 0x1000;
            let child = parent + 0x100;
            let mut root = descriptor(0);
            set_release(&mut root, 24, parent, 1);
            root[15] = (child >> 8) as u32;
            root[16] = 1 | (action << 1);
            let mut dependent = descriptor(u32::from(action == 4));
            set_release(&mut dependent, 24, child, 2);
            let read = |a, out: &mut Descriptor| {
                *out = if a == parent { root } else { dependent };
                true
            };
            if action == 4 {
                assert_eq!(
                    scheduler.signal(child, 3, read, PlainRelease(|_, _| panic!("early child"))),
                    ""
                );
            }
            let mut releases = Vec::new();
            assert_eq!(
                scheduler.signal(
                    parent,
                    3,
                    read,
                    PlainRelease(|a, p| {
                        releases.push((a, p));
                        true
                    })
                ),
                ""
            );
            assert_eq!(releases, [(parent, 1), (child, 2)]);
        }
        let mut circular = descriptor(0);
        circular[15] = 0x80;
        circular[16] = 1 | (3 << 1);
        set_release(&mut circular, 24, 1, 1);
        let mut releases = 0;
        assert_eq!(
            scheduler.signal(
                0x8000,
                3,
                |_, out| {
                    *out = circular;
                    true
                },
                PlainRelease(|_, _| {
                    releases += 1;
                    true
                })
            ),
            "QMD chain limit"
        );
        assert_eq!(releases, 256);
    }

    #[test]
    fn dependent_validation_occurs_after_parent_release_but_before_child_execution() {
        let _guard = reset();
        let mut scheduler = NativeQmdScheduler::default();
        for (flags, expected) in [
            (1 | (2 << 1), "unsupported dependent QMD action"),
            (1 | (3 << 1) | (1 << 5), "second dependent QMD unsupported"),
        ] {
            let mut qmd = descriptor(0);
            qmd[16] = flags;
            qmd[15] = 0x100;
            set_release(&mut qmd, 24, 0x55, 9);
            let mut calls = Vec::new();
            assert_eq!(
                scheduler.signal(
                    1,
                    3,
                    |address, out| {
                        assert_eq!(address, 1, "dependent must not execute after rejection");
                        *out = qmd;
                        true
                    },
                    PlainRelease(|a, p| {
                        calls.push((a, p));
                        true
                    })
                ),
                expected
            );
            assert_eq!(calls, [(0x55, 9)]);
        }
    }

    #[test]
    fn dependency_counts_validation_and_cache_limit_match_upstream() {
        let _guard = reset();
        let mut scheduler = NativeQmdScheduler::default();
        assert_eq!(
            scheduler.signal(
                1,
                7 | (0x3ff << 14),
                |_, _| panic!(),
                PlainRelease(|_, _| true)
            ),
            ""
        );
        assert_eq!(
            scheduler.signal(
                1,
                3,
                |_, out| {
                    *out = descriptor(1023);
                    true
                },
                PlainRelease(|_, _| true)
            ),
            "QMD dependency underflow"
        );
        assert_eq!(
            scheduler.signal(2, 3, |_, _| false, PlainRelease(|_, _| true)),
            "unmapped QMD"
        );
        for (address, word, value) in [(3, 18, 0x31), (4, 4, 0x100)] {
            assert_eq!(
                scheduler.signal(
                    address,
                    3,
                    |_, out| {
                        *out = descriptor(0);
                        out[word] = value;
                        true
                    },
                    PlainRelease(|_, _| true)
                ),
                "unsupported QMD version or queue"
            );
        }
        assert_eq!(
            scheduler.signal(5, 6, |_, _| panic!(), PlainRelease(|_, _| true)),
            "unsupported PCAS action"
        );
        for address in 1..=4096 {
            assert_eq!(
                scheduler.signal(address, 0, |_, _| panic!(), PlainRelease(|_, _| true)),
                ""
            );
        }
        assert_eq!(
            scheduler.signal(4097, 1, |_, _| panic!(), PlainRelease(|_, _| true)),
            "QMD cache limit"
        );
        assert_eq!(
            scheduler.signal(1, 1, |_, _| panic!(), PlainRelease(|_, _| true)),
            ""
        );
        assert_eq!(std::mem::size_of::<Descriptor>(), 256);
        assert_eq!(std::mem::align_of::<Descriptor>(), 4);
    }
}
