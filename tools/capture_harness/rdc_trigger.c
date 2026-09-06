// SPDX-License-Identifier: GPL-3.0-or-later
// Optional Linux LD_PRELOAD bridge. Build against the installed RenderDoc SDK header.
// No fixed paths, game-specific timings, global key injection or busy trigger-file polling.
#define _GNU_SOURCE
#include <stdbool.h>
#include <renderdoc_app.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

static uint64_t monotonic_ms(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000;
}

static int reply(int fd, const char *message) {
    size_t remaining = strlen(message);
    while (remaining) {
        ssize_t sent = send(fd, message, remaining, MSG_NOSIGNAL);
        if (sent < 0 && errno == EINTR) continue;
        if (sent <= 0) return 0;
        message += sent;
        remaining -= (size_t)sent;
    }
    return 1;
}

static void *control(void *arg) {
    int fd = (int)(intptr_t)arg;
    FILE *input = fdopen(fd, "r");
    if (!input) { close(fd); return NULL; }
    RENDERDOC_API_1_6_0 *api = NULL;
    pRENDERDOC_GetAPI get_api = (pRENDERDOC_GetAPI)dlsym(RTLD_DEFAULT, "RENDERDOC_GetAPI");
    if (!get_api || !get_api(eRENDERDOC_API_Version_1_6_0, (void **)&api)) {
        reply(fd, "ERROR RenderDoc API 1.6.0 unavailable\n");
        fclose(input);
        return NULL;
    }
    const char *prefix = getenv("RUZU_CAPTURE_PREFIX");
    if (prefix && *prefix) api->SetCaptureFilePathTemplate(prefix);
    if (!reply(fd, "READY\n")) { fclose(input); return NULL; }
    char command[128];
    while (fgets(command, sizeof(command), input)) {
        unsigned frames = 0, timeout_ms = 0;
        if (sscanf(command, "CAPTURE %u %u", &frames, &timeout_ms) != 2 ||
            frames == 0 || frames > 16 || timeout_ms == 0 || timeout_ms > 300000) {
            if (!reply(fd, "ERROR invalid command\n")) break;
            continue;
        }
        if (api->IsFrameCapturing()) {
            if (!reply(fd, "ERROR another capture is active\n")) break;
            continue;
        }
        uint32_t before = api->GetNumCaptures();
        api->TriggerMultiFrameCapture(frames);
        uint64_t end = monotonic_ms() + timeout_ms;
        while (api->GetNumCaptures() < before + frames && monotonic_ms() < end) {
            struct timespec delay = {0, 10000000};
            nanosleep(&delay, NULL);
        }
        if (api->GetNumCaptures() < before + frames) {
            reply(fd, "ERROR capture timeout (check active API/window)\n");
            break; // Do not queue more captures behind an unresolved request.
        }
        // ACK is sent only after RenderDoc reports completed capture files.
        char response[65536] = "DONE";
        size_t used = strlen(response);
        for (uint32_t index = before; index < before + frames; ++index) {
            char path[4096];
            uint32_t length = 0;
            if (!api->GetCapture(index, NULL, &length, NULL) || length >= sizeof(path) ||
                !api->GetCapture(index, path, &length, NULL)) {
                strcpy(response, "ERROR capture filename unavailable");
                used = strlen(response);
                break;
            }
            path[sizeof(path) - 1] = 0;
            int written = snprintf(response + used, sizeof(response) - used, " %s", path);
            if (written < 0 || (size_t)written >= sizeof(response) - used - 2) {
                strcpy(response, "ERROR capture filenames too long");
                used = strlen(response);
                break;
            }
            used += (size_t)written;
        }
        response[used++] = '\n';
        response[used] = 0;
        if (!reply(fd, response)) break;
    }
    fclose(input);
    return NULL;
}

__attribute__((constructor)) static void start_control(void) {
    const char *value = getenv("RUZU_CAPTURE_CONTROL_FD");
    if (!value) return;
    char *end = NULL;
    long parsed = strtol(value, &end, 10);
    if (!*value || *end || parsed < 3 || parsed > INT32_MAX) return;
    int fd = (int)parsed;
    // Do not propagate the harness channel to subprocesses of the target.
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    unsetenv("RUZU_CAPTURE_CONTROL_FD");
    pthread_t thread;
    if (pthread_create(&thread, NULL, control, (void *)(intptr_t)fd) == 0)
        pthread_detach(thread);
    else close(fd);
}
