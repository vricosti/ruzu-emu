# Metal pause diagnostics

Launch the GUI binary directly so it inherits the opt-in environment:

```sh
RUZU_PROFILE_METAL_STALLS=1 ./target/release/ruzu.app/Contents/MacOS/ruzu
```

Select Metal and reproduce the pause. Keep the existing shader caches and repeat
the same route in the same game session. The normal Ruzu log receives two kinds
of lines (Info logging must be enabled):

- `METAL_STALL_OP`: an instrumented CPU operation took at least 100 ms.
  `t_ms` is its start time relative to profiler initialization; `wall_ms` is its
  duration. The thread and source location identify its execution context.
- `METAL_STALL_FRAME`: successful presentation submissions on this thread were
  at least 100 ms apart. Per-operation count, total and maximum wall times cover
  that interval. `t_ms` marks the end of the interval. The first submission only
  establishes a baseline, avoiding a fake startup frame gap.

`MslCompile` and pipeline categories measure synchronous creation, not cache hits.
`ShaderBuild` includes guest shader translation and MSL compilation. `GpuWait`
measures command-buffer completion waits; `Drawable` measures nextDrawable.
`Upload` and `Download` measure the instrumented CPU copy/encoding/readback paths,
not GPU transfer execution time. The existing `RUZU_PROFILE_METAL_SUBMISSIONS`
and `RUZU_PROFILE_METAL_STAGES` tools provide GPU measurements separately.

These are **inclusive** times: nested MSL compilation or GPU waits can appear
inside another operation. Do not sum categories to calculate total busy time.
Per-frame aggregates are thread-local; another thread's long operation has its
own OP line and is not automatically attributed to a presentation interval.
Successful submission is not proof that a drawable was displayed at that instant.

If a pause coincides with pipeline creation and disappears on a second pass,
compilation is a strong lead. A long GpuWait or Drawable instead warrants GPU
workload/presentation analysis. A large gap with small measured operations is
unexplained by these probes: profile guest threads, scheduling and uninstrumented
work rather than declaring a GPU fault. Pausing, minimizing or stopping the game
can also cause gaps; note these actions when interpreting a log.

The switch is read once per process. Disabled profiling performs no clock reads,
thread-local aggregation or output. Enabled profiling uses fixed-size per-thread
storage and formats logs only above the threshold. It does not force GPU waits,
capture images, change caches or record every draw. It still adds measurement
overhead, so confirm performance conclusions without profiling too.

## Library reuse

`METAL_SHADER_CACHE` reports lookup hits/misses every 128 requests when this
profiling switch is enabled. A lookup hit can include a retry after a compiler
error, so it is not itself proof of a saved compilation; correlate with
`MslCompile` counts. The per-device cache compares complete MSL source and
language version and shares libraries/functions, never pipeline states or
per-artifact binding metadata. Compiler math mode remains fixed.

Retention is FIFO-bounded to 2048 library entries and 32 MiB of source keys
(not total driver memory). Live shader modules retain their own native objects
after eviction. Oversized sources compile without being retained by this cache.
The cache is in-memory: it does not replace the persistent pipeline archive or
eliminate compilation for previously unseen source.
