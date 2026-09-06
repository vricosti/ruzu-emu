# Unified kernel waits — implementation and validation record

## Final status — 2026-09-06

Implementation and runtime validation completed on `refactor/unified-kernel-waits`.
The notes below retain the chronological prerequisite/debugging history; their
intermediate "pending" statements are superseded by this section.

- Parentless hosts and guests now share native synchronization objects, queues,
  notification selection, cancellation and hardware deadlines. Guest fibers
  and host condition variables remain distinct suspension mechanisms.
- Production MultiWait polling and EventObserver's periodic retry are removed.
  Local Event/CV wait-many and the extra session wakeup are restricted to tests.
- Native object lifetimes, pointer identity, timer cancellation, host restart
  identity and scheduler-lock entry/exit are covered by synthetic regressions.
- Latest isolated core comparison: 1,584 tests, 1,536 pass, the same 48 failures
  as the reference; all 14 added tests pass. Ordinary shared-process `cargo test
  -p core` still aborts on global fixture interference (also on the reference).
  `cargo test -p video_core`: 1,520 pass, one ignored, no failures; doc tests pass.
- Release build succeeds. Visible GUI startup, recorded route into gameplay,
  Stop and same-process restart were observed. The user subsequently confirmed
  that Stop and normal GUI closure both work with this final build.
- Ten-second idle worker sample: TimeWorker and EventObserver each show 0.00%
  CPU and 0.00 voluntary/involuntary switches per second. The prior prerequisite
  build's hands-off visible-scene TimeWorker sample also shows zero: the native
  path preserves the absence of periodic polling, not a demonstrated FPS gain.
  Reference: `../ruzu-capture-resumed-RDjnus/optimized-idle-worker.txt` with the
  matching before/after captures and measurement notes in
  `../ruzu-performance-p5Tlrv/REPORT.md`. Current evidence remains in
  `/tmp/ruzu-unified-waits.W3xit0/visible-hall-thread-sample.txt`.
- `DIFF.md` records actual ownership/lifetime adaptations and pre-existing
  structural limits. No artificial ID dummies or client impersonation added;
  deferred IPC fiber switching is preserved. Main and Eden were not modified.
- The user has now authorized commit/push on this branch. Their unrelated
  GOAL.md deletion, bases.json changes and .agents files remain excluded.

## Base and preserved work

- Branch: `refactor/unified-kernel-waits`, created from fetched `origin/main`
  (`df0f321f`). Main is unchanged.
- Required prerequisite `58083dfc` is not in main. Applied explicitly with
  `cherry-pick --no-commit`; no commit/push/merge is authorized for this task.
- Preserve the local deletion of GOAL.md, bases.json and .agents diagnostics.

## Verified history

- bcc90fcc implemented dummy blocking/wakeup.
- 16cdd26d added two artificial service-name dummies for ID alignment;
  beff4698 removed only those, not real host-thread identities.
- c9e989b2 removed IPC client-thread impersonation (dispatch-count corruption).
- 1acda68c fixed host service thread lifecycle and process scheduler wiring.

## Architecture and interrupted prerequisite

Use KSynchronizationObject's native objects, intrusive lists, queue callbacks,
cancellation and hardware timer for host and guest waits. Only suspension differs:
guest fibers versus the existing dummy KThread condition variable. Never borrow
the IPC client's identity or manufacture guest processes for host waiters.

MultiWait's parent-process and per-core scheduler requirements are unnecessary
for already-owned native objects (`wait_on_objects` does not use its scheduler).
However, removing these guards now would expose a missing prerequisite:
`initialize_dummy_thread(None, ...)` does not bind the global scheduler context,
so state changes cannot prepare the dummy block or enqueue its wakeup.

First implement and test that prerequisite in KThread/KernelCore and ensure host
blocking retains no mutable KThread reference. Then migrate MultiWait.
ServiceContext already creates native events during normal runtime; raw
Event::new and ServerManager's late event bridge still require caller auditing.
Null-System tests must not silently select a production polling path.

Parentless dummy prerequisite now passes its real-callback regression. The
MultiWait migration is interrupted for the next confirmed prerequisite:
PSC OperationEvent, Alarms and StandardUserSystemClockCore omit upstream's
ServiceContext/event creation. Implement those native owners before removing
MultiWait's fallback. ServerManager already attaches its wakeup event before
entering its runtime loop, but that late bridge must be audited for early signals.

Finite-wait prerequisite discovered: KHardwareTimer::register_absolute_task_by_id
did not set KThread::timer_task.time, unlike RegisterAbsoluteTaskImpl upstream.
Queue cancellation therefore skipped removal of these deadlines. Fixed in the
timer owner; the focused cancellation/tree-empty test and real parentless host
finite-deadline/cancel tests now pass.

Next prerequisite: KServerPort::EnqueueSession pushes into the queue but omits
upstream's notification on the empty-to-nonempty transition. Native port waits
would otherwise miss arrivals once polling is removed. Implement in the port
owner, auditing the caller-held KPort mutex before allowing scheduler unlock to
switch fibers. Do not introduce a switch while that wrapper mutex is held.

Wait callback identity is being changed from numeric object ID to the native
SynchronizationObjectState pointer (upstream compares object pointers). Process,
session and event ID namespaces can collide. Retain IDs only for diagnostics.

## Completed slices and remaining validation

1. Parentless dummy global-scheduler wiring, separate allocation for host
   suspension, native object retention until unlink and timer cancellation fixed.
2. MultiWait directly uses native objects; normal/light server ports now notify
   and dequeue under the scheduler lock. Notifications select native object
   identity, not numeric IDs. Closed sessions retain a valid selected index.
3. Time notification owners and EventObserver now create native events eagerly.
   EventObserver's 100 ms retry and MultiWait's 100 us native-object fallback are
   gone. Standalone Event CV/list helpers and the additional server-session
   manager wakeup are test-only. Production registration keeps only a boolean.
4. Host cancellation, termination, timed delivery, pause/resume, manual reset,
   simultaneous waiters, early wakeup, native process/port/session/thread,
   guest queue object lifetime and kernel restart have focused regressions.
5. Final full-suite comparison and release/runtime check are still in progress;
   do not commit/push or call the whole kernel port complete.

Hardware-timer deadlines are absolute global-time nanoseconds. The host CV has
no independent timeout or periodic polling. Verified multicore behavior: the
clock value advances while paused, delivery waits for resume, explicit signals
still wake host waiters while paused. Single-core tests advance guest CPU ticks.

## Runtime evidence so far

Evidence directory: `/tmp/ruzu-unified-waits.W3xit0`.
The first new release successfully ran FreeBrick, Stop, a second launch in the
same GUI process and Quit. Both TimeWorker and EventObserver disappeared at
Stop; their idle 10-second sample had zero measured CPU and context switches.
Later the old comparison executable (`d30e3244bc`, not this branch) froze before
FreeBrick's ball launch. Its screenshot and SIGUSR1 dump are preserved. Do not
use that stalled run as a comparable-scene performance baseline. No FPS gain
is established.

The recorded controller route subsequently reached the interactive hotel lobby
on the new build. A deliberate pause/resume test returned to animated rendering.
The final host-identity adjustment still needs the last rebuild/runtime pass.

An intermediate persistent-host identity change exposed two additional core
test failures: identity creation honored ScopedKernelForTest but revalidation
read KERNEL_PTR. Using get_kernel_ref consistently and retaining identity when
both scheduler references are absent fixes the two targeted transfer-memory
cleanup tests. Re-run all isolated tests before concluding no regressions.

## Latest boot validation correction

The final GUI run stalled during service initialization. With user-enabled
ptrace, `/tmp/ruzu-unified-waits.W3xit0/loading-stacks.txt` identified CoreTiming
inside priority-update holding GSC and per-core scheduler mutexes, then lazily
initializing a parentless dummy and re-locking GSC to obtain its scheduler-lock
address. The boot thread waited in KProcess::attach_scheduler. The debugger
detached; the GUI subsequently exited after the normal Quit action.

Parentless dummy initialization now reads the kernel's already-published lock
address without acquiring the GSC wrapper. The focused regression passes.
The isolated comparison now has 1,584 tests: 1,536 pass, and the same 48 failures
as the 1,570-test reference (no added failures). Release rebuild and runtime
recheck pending. Logs use the `boot-fix` prefix/suffix in the evidence directory.

That next runtime exposed a second identity-lifecycle error: fast TLS access
still used an old dummy, so scheduler-lock entry disabled dispatch on it before
normal access replaced it during unlock (CoreTiming enable_dispatch assertion).
Fast access now refreshes cached host identities too; explicit guest/service
identities retain their direct path. The restart regression also exercises
scheduler lock/unlock and verifies a balanced dispatch count. All 1,584 isolated
tests have the same outcomes after this fix. Logs: `identity-fixed-*` and
`identity-fast-path-test.log`; the ordinary whole core suite still aborts due
to shared global fixture interference, as on the reference. Rebuild/runtime
validation is still pending at this point.

The identity-fixed release built successfully (2m26s). Its next run had no
identity assertion, but waited in Vulkan acquire-next-image with the NV service
in wait_host_stalled and CoreTiming paused. The user then confirmed having
minimized the window. Do not treat that run as a comparable visible-scene
benchmark or as proof of a native-wait regression. It later displayed the title;
Stop returned to the list and removed CoreTiming, CPUCore, HLE, TimeWorker and
EventObserver threads. A same-process restart with the recorded controller
sequence is underway with the window explicitly visible (`visible-restart-replay.log`).

Visible same-process restart succeeded: all seven recorded presses completed,
and `visible-restart-hall.png` confirms the interactive hotel lobby (27 FPS
instantaneous status, not a performance comparison). No new panic in this run.
The validated release remains running for the user's tests; no further input
injection is scheduled. The post-restart worker sample is
`visible-hall-thread-sample.txt`. No commit/push/merge was performed.

## Remaining scope limits

- Alarms metadata-only container and complete port endpoint parent/close
  ownership remain pre-existing structural work, not certified by this change.
- Unrelated services still materialize some Event IPC bridges lazily. Their
  pending-signaled mirror is not used for native host waits. ServerManager
  prepares its existing native bridge before entering its selection loop.
- Explicit HLE IPC transaction/deferred-fiber-switch infrastructure is retained.

## Validation reference

Pristine worktree: `/home/vricosti/Dev/emulators/ruzu-perf-baseline-tests.4Ua86R`.
Previous isolated core test comparison: 48 identical pre-existing failures on
df0f321f and 58083dfc; ordinary all-in-one core test runs also have global-kernel
fixture interference. Evidence: `/home/vricosti/Dev/emulators/ruzu-capture-resumed-RDjnus`.
Do not claim the core suite is green or a FPS gain from these earlier results.
