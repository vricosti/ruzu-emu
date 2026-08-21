# Native Metal Renderer for macOS

Implement a renderer on a dedicated branch that uses Apple's Metal API directly on macOS.

The implementation must not route rendering through Vulkan, MoltenVK, OpenGL, or another graphics API compatibility layer. Eden remains the source of truth for Maxwell guest semantics, operation ordering, cache lifecycle, and renderer ownership. Metal-specific resource binding, synchronization, pipeline, render-pass, and presentation behavior must be implemented with native Metal concepts.

The goal is complete only when:

- the Metal renderer can be selected and initialized on macOS;
- ruzu and ruzu-cmd compile with the Metal backend;
- Mario Kart 8 Deluxe boots with that backend;
- the game reaches a race; and
- visible game content is rendered during the race.

Do not stop at a clear-screen, presentation-only, null-rasterizer, Vulkan-backed, or otherwise partial compatibility implementation.
