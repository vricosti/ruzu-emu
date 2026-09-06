// SPDX-License-Identifier: GPL-3.0-or-later
// Synthetic application API used only by renderdoc_bridge.py. No GPU is accessed.
#include <stdbool.h>
#include <renderdoc_app.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#ifdef SYNTHETIC_TARGET
int main(void) { sleep(2); return 0; }
#endif

static uint32_t captures;
static char filename[4096] = "/tmp/synthetic.rdc";

static void set_prefix(const char *prefix) {
    snprintf(filename, sizeof(filename), "%s_synthetic.rdc", prefix);
}
static uint32_t count(void) { return captures; }
static uint32_t busy(void) {
    const char *mode = getenv("FAKE_CAPTURE_MODE");
    return mode && strcmp(mode, "busy") == 0;
}
static void trigger(uint32_t frames) {
    const char *mode = getenv("FAKE_CAPTURE_MODE");
    if (mode && strcmp(mode, "timeout") == 0) return;
    FILE *file = fopen(filename, "w");
    if (file) { fputs("synthetic, not an RDC file\n", file); fclose(file); }
    captures += frames;
}
static uint32_t get_capture(uint32_t index, char *path, uint32_t *length, uint64_t *timestamp) {
    if (index >= captures) return 0;
    if (path) strcpy(path, filename);
    if (length) *length = (uint32_t)strlen(filename) + 1;
    if (timestamp) *timestamp = 0;
    return 1;
}

int RENDERDOC_GetAPI(RENDERDOC_Version version, void **output) {
    static RENDERDOC_API_1_6_0 api = {
        .SetCaptureFilePathTemplate = set_prefix,
        .GetNumCaptures = count,
        .IsFrameCapturing = busy,
        .TriggerMultiFrameCapture = trigger,
        .GetCapture = get_capture,
    };
    if (version != eRENDERDOC_API_Version_1_6_0) return 0;
    *output = &api;
    return 1;
}
