# Document applet investigation — 2026-09-17

## Current implementation status

- Follow-up native crash: Windows minidump ruzu.exe.2184.dmp identifies
  `RoContext: no free process context available` in RegisterProcess. Kernel
  endpoint mirrors still held the request manager/handler after port closure.
  DestroySession now detaches that additional manager Arc after kernel cleanup,
  outside endpoint/process locks. Real RO handler regression covers six closes
  and context reuse; failed before the fix and now passes. All 26 ServerManager
  and nine RO tests pass. Full core suite still aborts with 0xc0000005
  (D:/tmp/ruzu-ro-destruction-core-tests.log). build.bat succeeded in 3m59s;
  standalone Release refreshed, GUI left closed. User runtime retest pending.
- Repeated-page retest with filesystem/server traces: index.html and style.css
  are opened; ldr:ro reports both sessions closed, but third creation fails.
  Reproduced owner-lookup error with a service-process mirror registered before
  the applet. Fix records KSession.process_id once on initial registration and
  resolves that identity even through a mirrored registry. Regression failed
  before the fix and passes afterward (five cycles, two-slot port); ten other
  focused session/port/finalization tests pass. Full core suite still aborts
  with 0xc0000005 (D:/tmp/ruzu-session-owner-core-tests.log).
  build.bat succeeded in 4m21s and refreshed the standalone Release executable.
  Live page switching not yet retested; GUI left closed.
- Follow-up regression: command 21 omitted its Out<u8> ContentAttributes.
  Verified offline in the installed Web applet (main NSO offsets 0x65b5f8,
  0x655808, 0x64d2d8). The stale TLS byte 0x53 caused FSP InvalidArgument
  and guest fatal. Response now includes attributes=0; FSP validation stays.
  Handler-to-guest-TLS regression and both document-path/eight FSP tests pass.
  Full core suite still aborts with 0xc0000005 (also reports unrelated failed
  tests); log: D:/tmp/ruzu-document-attributes-core-tests.log.
  build.bat succeeded in 4m14s and refreshed the standalone Release executable.
  User retest: Intellectual Property Notice remains white. Logs confirm the
  document NCA now mounts successfully. Texture errors occur during rendering;
  their causal role is unverified. A later repeated opening fails to create an
  ldr:ro session with 0xe01 (OutOfSessions), followed by a guest fatal. Session
  lifetime and rendering remain unresolved; do not claim this menu is fixed.
- NS command 21 now resolves CNMT content types to provider-backed guest paths;
  patch content takes precedence. Missing documents still return an error.
- Its prerequisite FSP commands 8 and 10 now mount document/control/data NCAs
  through the existing NCA reader, RomFS extraction and IFileSystem service.
  Guest paths are compared only against the content catalogue, never opened
  through the host filesystem. External container files use a virtual
  UserContent root, not a synthetic physical gamecard handle.
- Five document-focused tests and all eight FspSrv tests pass. Full
  `cargo test -p core --locked --offline` still terminates with 0xc0000005;
  output: D:/tmp/ruzu-document-core-tests.log. No full-suite success claim.
- Release build succeeded in 4m11s; standalone executable refreshed at
  build/x86_64-pc-windows-msvc/release/ruzu.exe. Runtime document rendering
  remains unverified. User will launch Ruzu themselves; no instance started.
- ACC system commands 130/136 are unchanged. Switchbrew lists their names but
  not their response structures/offline-cache result contract, and Eden has no
  implementation. Next account slice needs that contract first. No fake online
  account/cache data or guessed success response was added.

## Earlier investigation and prerequisite history

## Next implementation prerequisites (2026-09-17)

- NS command 21: Switchbrew NS_services#GetApplicationContentPath specifies
  a type-0x16 output path buffer, content type and application ID; prefer update
  content then base, report absent content, return a guest FS content path.
- The consumer needs FSP command 8 OpenFileSystemWithId. Eden registers it as
  nullptr; Ruzu has no handler and its generic fallback returns success without
  a filesystem object. Implementing NS alone cannot enable document loading.
- ACC IManagerForSystemService command 130 is nullptr in Eden too. The existing
  IManagerForApplication implementation is a distinct interface contract and
  cannot be transplanted without verifying system-service layout and semantics.
- User authorized extending Eden's missing commands on 2026-09-17. Implement
  the FSP prerequisite before NS, using Switchbrew NS/NCM and libnx fs.c
  contracts (command 8; command 10 with content attributes since firmware 16).
  Resolve only catalogued VFS content, never guest-supplied host filesystem paths.
- ACC system command 130 remains paused pending an independently verified
  response/error contract; do not transplant the application-service cache.

- GetDocumentInterface and commands 23/92 are wired and tested.
- GetApplicationContentPath (21) remains unimplemented in Eden as well as Ruzu.
  The guest handles this failure as "Unable to load data". Implementing document
  loading requires establishing the content-path/FS mounting contract first;
  no invented path or success response has been added.
- Switchbrew NS_services documents the command, but the checked source does
  not supply a complete implementation to port.
- Independent ldr:ro failure 0xE01 is OutOfSessions. KProcess handle teardown
  removed parent sessions before server destruction, preventing port slot release.
  Parent registry lifetime corrected; focused teardown/repeated-port tests pass.
- Release rebuild and standalone refresh succeeded (4m11s). Runtime retest
  remains pending. No complete document-loading claim.
- Retest still hit ldr:ro 0xE01. Additional cause: session-owner lookup omitted
  guest applet process_list entries. Lookup now includes live/retiring applets;
  removed owners with outstanding sessions remain in deferred finalization.
  New regression exercises lookup through removal before server close and passes.
  Latest release rebuild and standalone refresh succeeded (4m08s).
  Runtime retest pending; full core tests still end in STATUS_ACCESS_VIOLATION.
