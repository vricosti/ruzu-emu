# Home Menu Options — 2026-09-16

Live diagnosis: Home Menu polls handle 0 and loops on ResultInvalidHandle.
The guest holder is OLSC INativeHandleHolder. Transfer controller commands 5/9
return bare success instead of child objects; command 0 then hits the parent.

Interrupted slice: transfer task controller. Prerequisites: wire native handle
holder with an owned ServiceContext event and add Eden's empty stopper object.
Prerequisites and controller replies implemented and audited in DIFF.md.
All 11 OLSC tests pass. Full core suite terminates with STATUS_ACCESS_VIOLATION.
Release rebuilt successfully on 2026-09-17 (5m01s), including the Web/eShop
completion deadlock fix. Standalone DLL build refreshed successfully at
build/x86_64-pc-windows-msvc/release/ruzu.exe.
User runtime verification remains pending.
