/* Private compositor-only click source for the marker hit-test regression.
 * This is not zuse's production pointer backend and never opens uinput.
 * Protocol: https://gitlab.freedesktop.org/wlroots/wlr-protocols/-/blob/master/unstable/wlr-virtual-pointer-unstable-v1.xml
 * Commands: click, quit. Output: READY, OK; UNSUPPORTED when not advertised.
 */
#define _GNU_SOURCE
#include <wayland-client.h>
#include <limits.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static const struct wl_interface pointer_interface;
static const struct wl_interface *create_types[] = {&wl_seat_interface, &pointer_interface};
static const struct wl_message manager_requests[] = {
    {"create_virtual_pointer", "?on", create_types}, {"destroy", "", NULL}
};
static const struct wl_interface manager_interface = {
    "zwlr_virtual_pointer_manager_v1", 1, 2, manager_requests, 0, NULL
};
static const struct wl_message pointer_requests[] = {
    {"motion", "uff", NULL}, {"motion_absolute", "uuuuu", NULL},
    {"button", "uuu", NULL}, {"axis", "uuf", NULL}, {"frame", "", NULL},
    {"axis_source", "u", NULL}, {"axis_stop", "uu", NULL},
    {"axis_discrete", "uufi", NULL}, {"destroy", "", NULL}
};
static const struct wl_interface pointer_interface = {
    "zwlr_virtual_pointer_v1", 1, 9, pointer_requests, 0, NULL
};
static struct wl_proxy *manager;

static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    (void)data; (void)version;
    if (!manager && strcmp(interface, "zwlr_virtual_pointer_manager_v1") == 0)
        manager = wl_registry_bind(registry, name, &manager_interface, 1);
}
static void removed(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}
static const struct wl_registry_listener listener = {global, removed};

static int private_socket(void) {
    const char *guard = getenv("ZUSE_PRIVATE_WAYLAND_FIXTURE");
    const char *headless = getenv("ZUSE_PRIVATE_WAYLAND_HEADLESS");
    const char *runtime = getenv("XDG_RUNTIME_DIR");
    const char *display = getenv("WAYLAND_DISPLAY");
    if (!guard || strcmp(guard, "1") || !headless || strcmp(headless, "1") ||
        !runtime || !display || !*display || getenv("WAYLAND_SOCKET")) return -1;
    char path[PATH_MAX], resolved[PATH_MAX], root[PATH_MAX];
    int length = display[0] == '/' ? snprintf(path, sizeof(path), "%s", display) :
                 snprintf(path, sizeof(path), "%s/%s", runtime, display);
    if (length < 0 || (size_t)length >= sizeof(path) ||
        !realpath(path, resolved) || !realpath(runtime, root)) return -1;
    size_t size = strlen(root);
    struct stat status;
    if (strncmp(resolved, root, size) || resolved[size] != '/' ||
        stat(resolved, &status) || !S_ISSOCK(status.st_mode)) return -1;
    return 0;
}

static int sync_display(struct wl_display *display) {
    alarm(3);
    int result = wl_display_roundtrip(display);
    alarm(0);
    return result;
}
static uint32_t milliseconds(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now)) return 0;
    return (uint32_t)((uint64_t)now.tv_sec * 1000 + now.tv_nsec / 1000000);
}
static void button(struct wl_proxy *pointer, bool down) {
    wl_proxy_marshal_flags(pointer, 2, NULL, 1, 0, milliseconds(), 0x110u,
                           down ? WL_POINTER_BUTTON_STATE_PRESSED : WL_POINTER_BUTTON_STATE_RELEASED);
    wl_proxy_marshal_flags(pointer, 4, NULL, 1, 0);
}

int main(void) {
    if (private_socket()) {
        fputs("fixture: refusing pointer connection outside the owned headless socket\n", stderr);
        return 2;
    }
    setvbuf(stdout, NULL, _IOLBF, 0);
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) return 1;
    struct wl_registry *registry = wl_display_get_registry(display);
    if (!registry || wl_registry_add_listener(registry, &listener, NULL) || sync_display(display) < 0)
        return 1;
    if (!manager) { puts("UNSUPPORTED"); wl_registry_destroy(registry); wl_display_disconnect(display); return 77; }
    struct wl_proxy *pointer = wl_proxy_marshal_flags(manager, 0, &pointer_interface, 1, 0, NULL, NULL);
    if (!pointer || sync_display(display) < 0) return 1;
    puts("READY");
    char line[64];
    bool down = false;
    int result = 0;
    while (fgets(line, sizeof(line), stdin)) {
        if (!strcmp(line, "quit\n")) break;
        if (strcmp(line, "click\n")) { puts("ERR invalid command"); continue; }
        /* Preserve the compositor's current position; zuse performed the move. */
        button(pointer, true); down = true;
        if (sync_display(display) < 0) { result = 1; break; }
        button(pointer, false); down = false;
        if (sync_display(display) < 0) { result = 1; break; }
        puts("OK");
    }
    if (!wl_display_get_error(display)) {
        if (down) button(pointer, false);
        wl_proxy_marshal_flags(pointer, 8, NULL, 1, WL_MARSHAL_FLAG_DESTROY);
        wl_proxy_marshal_flags(manager, 1, NULL, 1, WL_MARSHAL_FLAG_DESTROY);
        if (sync_display(display) < 0) result = 1;
    } else {
        wl_proxy_destroy(pointer); wl_proxy_destroy(manager);
    }
    wl_registry_destroy(registry);
    wl_display_disconnect(display);
    if (!result) puts("OK");
    return result;
}
