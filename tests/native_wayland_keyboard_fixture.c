/* Persistent virtual keyboard for a disposable, private Hyprland test session.
 * Requires ZUSE_PRIVATE_WAYLAND_FIXTURE=1 before any Wayland connection.
 * Commands: caps, num, group1, neutral, down N, up N, press N, quit.
 * N is an XKB code in 8..255. Output: READY, then one OK or ERR per command.
 */
#define _GNU_SOURCE
#include <wayland-client.h>
#include <xkbcommon/xkbcommon.h>
#include <errno.h>
#include <inttypes.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

static const struct wl_interface virtual_keyboard_interface;
static const struct wl_interface *create_types[] = {
    &wl_seat_interface, &virtual_keyboard_interface
};
static const struct wl_message manager_requests[] = {
    {"create_virtual_keyboard", "on", create_types}
};
static const struct wl_interface manager_interface = {
    "zwp_virtual_keyboard_manager_v1", 1, 1, manager_requests, 0, NULL
};
static const struct wl_message keyboard_requests[] = {
    {"keymap", "uhu", NULL},
    {"key", "uuu", NULL},
    {"modifiers", "uuuu", NULL},
    {"destroy", "", NULL}
};
static const struct wl_interface virtual_keyboard_interface = {
    "zwp_virtual_keyboard_v1", 1, 4, keyboard_requests, 0, NULL
};

struct fixture {
    struct wl_display *display;
    struct wl_registry *registry;
    struct wl_seat *seat;
    struct wl_proxy *manager;
    struct wl_proxy *keyboard;
    struct xkb_context *context;
    struct xkb_keymap *map;
    struct xkb_state *state;
    xkb_mod_index_t lock, num;
    bool held[256];
};

static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    struct fixture *fixture = data;
    if (!fixture->seat && strcmp(interface, "wl_seat") == 0) {
        fixture->seat = wl_registry_bind(registry, name, &wl_seat_interface,
                                         version < 7 ? version : 7);
    } else if (!fixture->manager &&
               strcmp(interface, "zwp_virtual_keyboard_manager_v1") == 0) {
        fixture->manager = wl_registry_bind(registry, name, &manager_interface, 1);
    }
}
static void removed(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}
static const struct wl_registry_listener registry_listener = {global, removed};

static int sync_display(struct fixture *fixture) {
    if (wl_display_roundtrip(fixture->display) < 0) {
        fprintf(stderr, "fixture: Wayland roundtrip failed: %s\n", strerror(errno));
        return -1;
    }
    return 0;
}

static uint32_t milliseconds(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return 0;
    return (uint32_t)((uint64_t)now.tv_sec * 1000 + now.tv_nsec / 1000000);
}

static void send_modifiers(struct fixture *fixture) {
    wl_proxy_marshal_flags(fixture->keyboard, 2, NULL, 1, 0,
        xkb_state_serialize_mods(fixture->state, XKB_STATE_MODS_DEPRESSED),
        xkb_state_serialize_mods(fixture->state, XKB_STATE_MODS_LATCHED),
        xkb_state_serialize_mods(fixture->state, XKB_STATE_MODS_LOCKED),
        xkb_state_serialize_layout(fixture->state, XKB_STATE_LAYOUT_EFFECTIVE));
}

static void key_event(struct fixture *fixture, uint32_t code, bool pressed) {
    wl_proxy_marshal_flags(fixture->keyboard, 1, NULL, 1, 0,
                           milliseconds(), code - 8, pressed ? 1u : 0u);
    enum xkb_state_component changed = xkb_state_update_key(fixture->state, code,
                        pressed ? XKB_KEY_DOWN : XKB_KEY_UP);
    fixture->held[code] = pressed;
    /* Ordinary letters must not replay modifiers and hide lost state. */
    if (changed) send_modifiers(fixture);
}

static void neutral(struct fixture *fixture) {
    for (uint32_t code = 8; code < 256; ++code) {
        if (fixture->held[code]) key_event(fixture, code, false);
    }
    xkb_state_update_mask(fixture->state, 0, 0, 0, 0, 0, 0);
    send_modifiers(fixture);
}

static int upload_keymap(struct fixture *fixture) {
    char *text = xkb_keymap_get_as_string(fixture->map, XKB_KEYMAP_FORMAT_TEXT_V1);
    if (!text) return -1;
    size_t size = strlen(text) + 1;
    int fd = memfd_create("zuse-private-keyboard-fixture", MFD_CLOEXEC);
    if (fd < 0 || size > UINT32_MAX || ftruncate(fd, (off_t)size) != 0) {
        if (fd >= 0) close(fd);
        free(text);
        return -1;
    }
    size_t written = 0;
    while (written < size) {
        ssize_t count = write(fd, text + written, size - written);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) { close(fd); free(text); return -1; }
        written += (size_t)count;
    }
    free(text);
    wl_proxy_marshal_flags(fixture->keyboard, 0, NULL, 1, 0,
                           WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1, fd, (uint32_t)size);
    int result = sync_display(fixture);
    close(fd);
    return result;
}

static int initialize(struct fixture *fixture) {
    fixture->context = xkb_context_new(XKB_CONTEXT_NO_FLAGS);
    if (!fixture->context) return -1;
    const struct xkb_rule_names names = {
        .rules = "evdev", .model = "pc105", .layout = "us,de",
        .variant = "", .options = ""
    };
    fixture->map = xkb_keymap_new_from_names(fixture->context, &names,
                                            XKB_KEYMAP_COMPILE_NO_FLAGS);
    if (!fixture->map) return -1;
    fixture->state = xkb_state_new(fixture->map);
    if (!fixture->state || xkb_keymap_num_layouts(fixture->map) != 2) return -1;
    fixture->lock = xkb_keymap_mod_get_index(fixture->map, "Lock");
    fixture->num = xkb_keymap_mod_get_index(fixture->map, "Mod2");
    if (fixture->lock == XKB_MOD_INVALID || fixture->lock >= 32 ||
        fixture->num == XKB_MOD_INVALID || fixture->num >= 32) return -1;

    fixture->display = wl_display_connect(NULL);
    if (!fixture->display) {
        fprintf(stderr, "fixture: cannot connect to the explicitly selected private display\n");
        return -1;
    }
    fixture->registry = wl_display_get_registry(fixture->display);
    if (!fixture->registry ||
        wl_registry_add_listener(fixture->registry, &registry_listener, fixture) != 0 ||
        sync_display(fixture) != 0 || !fixture->seat || !fixture->manager) {
        fprintf(stderr, "fixture: seat or virtual keyboard manager unavailable\n");
        return -1;
    }
    fixture->keyboard = wl_proxy_marshal_flags(fixture->manager, 0,
        &virtual_keyboard_interface, 1, 0, fixture->seat, NULL);
    if (!fixture->keyboard || upload_keymap(fixture) != 0) return -1;
    neutral(fixture);
    return sync_display(fixture);
}

static void cleanup(struct fixture *fixture) {
    if (fixture->keyboard) {
        if (wl_display_get_error(fixture->display) == 0) {
            neutral(fixture);
            (void)sync_display(fixture);
            wl_proxy_marshal_flags(fixture->keyboard, 3, NULL, 1,
                                   WL_MARSHAL_FLAG_DESTROY);
            fixture->keyboard = NULL;
            (void)sync_display(fixture);
        } else {
            wl_proxy_destroy(fixture->keyboard);
            fixture->keyboard = NULL;
        }
    }
    if (fixture->manager) wl_proxy_destroy(fixture->manager);
    if (fixture->seat) wl_seat_destroy(fixture->seat);
    if (fixture->registry) wl_registry_destroy(fixture->registry);
    if (fixture->display) wl_display_disconnect(fixture->display);
    if (fixture->state) xkb_state_unref(fixture->state);
    if (fixture->map) xkb_keymap_unref(fixture->map);
    if (fixture->context) xkb_context_unref(fixture->context);
}

/* Return 1 for quit, 0 for success, -1 for invalid input, -2 for transport loss. */
static int command(struct fixture *fixture, const char *line) {
    if (strcmp(line, "quit") == 0) return 1;
    if (strcmp(line, "neutral") == 0) {
        neutral(fixture);
    } else if (strcmp(line, "caps") == 0 || strcmp(line, "num") == 0 ||
               strcmp(line, "group1") == 0) {
        neutral(fixture);
        uint32_t locked = strcmp(line, "caps") == 0 ? (1u << fixture->lock) :
                          strcmp(line, "num") == 0 ? (1u << fixture->num) : 0;
        xkb_state_update_mask(fixture->state, 0, 0, locked, 0, 0,
                              strcmp(line, "group1") == 0 ? 1 : 0);
        send_modifiers(fixture);
    } else {
        const char *argument;
        enum { DOWN, UP, PRESS } operation;
        if (strncmp(line, "down ", 5) == 0) { operation = DOWN; argument = line + 5; }
        else if (strncmp(line, "up ", 3) == 0) { operation = UP; argument = line + 3; }
        else if (strncmp(line, "press ", 6) == 0) { operation = PRESS; argument = line + 6; }
        else return -1;
        char *end;
        errno = 0;
        unsigned long code = strtoul(argument, &end, 10);
        if (errno || end == argument || *end || code < 8 || code > 255) return -1;
        if ((operation == UP && !fixture->held[code]) ||
            (operation != UP && fixture->held[code])) return -1;
        key_event(fixture, (uint32_t)code, operation != UP);
        if (operation == PRESS) {
            if (sync_display(fixture) != 0) return -2;
            key_event(fixture, (uint32_t)code, false);
        }
    }
    return sync_display(fixture) == 0 ? 0 : -2;
}

int main(void) {
    const char *guard = getenv("ZUSE_PRIVATE_WAYLAND_FIXTURE");
    if (!guard || strcmp(guard, "1") != 0) {
        fprintf(stderr, "fixture: refusing connection; set ZUSE_PRIVATE_WAYLAND_FIXTURE=1 only in a private compositor test\n");
        return 2;
    }
    if (!getenv("WAYLAND_DISPLAY") || !*getenv("WAYLAND_DISPLAY")) {
        fprintf(stderr, "fixture: an explicit private WAYLAND_DISPLAY is required\n");
        return 2;
    }
    setvbuf(stdout, NULL, _IOLBF, 0);
    struct fixture fixture = {0};
    if (initialize(&fixture) != 0) { cleanup(&fixture); return 1; }
    puts("READY");
    char line[128];
    int exit_status = 0;
    while (fgets(line, sizeof(line), stdin)) {
        size_t size = strlen(line);
        if (!size || (line[size - 1] != '\n' && !feof(stdin))) {
            int c; while ((c = getchar()) != '\n' && c != EOF) {}
            puts("ERR command too long"); continue;
        }
        line[strcspn(line, "\r\n")] = '\0';
        int result = command(&fixture, line);
        if (result == 1) { cleanup(&fixture); puts("OK"); return 0; }
        if (result == -2) { puts("ERR Wayland connection failed"); exit_status = 1; break; }
        puts(result == 0 ? "OK" : "ERR invalid command or key state");
    }
    cleanup(&fixture);
    return exit_status;
}
