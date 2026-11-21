/* Owned Xvfb fixture: GTK pixels, explicit geometry, invisible input cover,
 * and XKB lock readback. No window manager, desktop bus, or external input.
 * stdin commands: state, single, duplicate, blank, show, hide, cover, uncover,
 * frame, unframe, geometry X Y W H, caps 0|1, quit. One JSON line per command.
 * Paint commands acknowledge after GTK's frame and an XSync barrier.
 */
#define _GNU_SOURCE
#include <gtk/gtk.h>
#include <gdk/gdkx.h>
#include <X11/XKBlib.h>
#include <X11/Xatom.h>
#include <X11/keysym.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static GtkWidget *window, *area;
static Display *display;
static Window xid, cover, frame;
static cairo_surface_t *icon;
static int icon_count = 1, episode_visible;
static int wanted_x = 80, wanted_y = 70, wanted_width = 320, wanted_height = 240;
static unsigned clicks, covered_clicks;
static double click_x, click_y;
static unsigned original_locks, num_mask, caps_mask;
static gboolean pending_reply = TRUE, first_reply = TRUE;

static int private_display(void) {
    const char *guard = getenv("ZUSE_PRIVATE_X11_FIXTURE");
    const char *name = getenv("DISPLAY");
    const char *auth = getenv("XAUTHORITY");
    const char *workspace = getenv("ZUSE_VISION_NATIVE_WORKSPACE");
    if (!guard || strcmp(guard, "1") || !name || name[0] != ':' || !auth || !workspace)
        return -1;
    char *end;
    long number = strtol(name + 1, &end, 10);
    if (number < 10000 || number >= 30000 || *end) return -1;
    char root[PATH_MAX], file[PATH_MAX];
    if (!realpath(workspace, root) || !realpath(auth, file)) return -1;
    size_t size = strlen(root);
    return strncmp(file, root, size) || file[size] != '/' ? -1 : 0;
}

static int inside(double x, double y, double x0, double x1, double y0, double y1) {
    return x >= x0 && x < x1 && y >= y0 && y < y1;
}

/* Rasterize independently at each size: 25 px is not a resized PNG. */
static cairo_surface_t *make_icon(unsigned size) {
    cairo_surface_t *surface = cairo_image_surface_create(CAIRO_FORMAT_RGB24, (int)size, (int)size);
    if (cairo_surface_status(surface) != CAIRO_STATUS_SUCCESS) return surface;
    unsigned char *data = cairo_image_surface_get_data(surface);
    int stride = cairo_image_surface_get_stride(surface);
    for (unsigned y = 0; y < size; y++) {
        uint32_t *row = (uint32_t *)(data + y * (unsigned)stride);
        for (unsigned x = 0; x < size; x++) {
            unsigned sum = 0;
            for (unsigned sy = 0; sy < 4; sy++) for (unsigned sx = 0; sx < 4; sx++) {
                double px = (x + (sx + .5) / 4.0) * 20.0 / size;
                double py = (y + (sy + .5) / 4.0) * 20.0 / size;
                int stroke = inside(px, py, 4, 6, 3, 17) || inside(px, py, 4, 16, 3, 5) ||
                             inside(px, py, 4, 16, 15, 17) || inside(px, py, 14, 16, 8, 17) ||
                             inside(px, py, 9, 16, 8, 10);
                sum += stroke ? 30 : inside(px, py, 9, 11, 11, 13) ? 100 : 230;
            }
            unsigned value = (sum + 8) / 16;
            row[x] = 0xff000000u | value << 16 | value << 8 | value;
        }
    }
    cairo_surface_mark_dirty(surface);
    return surface;
}

static void geometry(int *x, int *y, unsigned *width, unsigned *height) {
    Window root, child;
    int local_x, local_y;
    unsigned border, depth;
    XGetGeometry(display, xid, &root, &local_x, &local_y, width, height, &border, &depth);
    XTranslateCoordinates(display, xid, DefaultRootWindow(display), 0, 0, x, y, &child);
}

/* Emulate only a reparenting WM's tree, inside this disposable X server.
 * A sibling InputOnly cover in this frame must not be mistaken for the client.
 */
static void wrap_window(gboolean enabled) {
    if ((enabled && frame) || (!enabled && !frame)) return;
    if (cover) { XDestroyWindow(display, cover); cover = 0; }
    int x, y, revert;
    unsigned width, height;
    Window focused;
    geometry(&x, &y, &width, &height);
    XGetInputFocus(display, &focused, &revert);
    Window root = DefaultRootWindow(display);
    Atom clients = XInternAtom(display, "_NET_CLIENT_LIST", False);
    if (enabled) {
        frame = XCreateSimpleWindow(display, root, x - 10, y - 10, width + 20, height + 20, 0, 0, 0);
        XReparentWindow(display, xid, frame, 10, 10);
        XMapWindow(display, xid);
        XMapRaised(display, frame);
        XChangeProperty(display, root, clients, XA_WINDOW, 32, PropModeReplace, (unsigned char *)&xid, 1);
    } else {
        XReparentWindow(display, xid, root, x, y);
        XMapRaised(display, xid);
        XDestroyWindow(display, frame); frame = 0;
        XDeleteProperty(display, root, clients);
    }
    /* Reparenting briefly unmaps a client. Restore the exact private focus. */
    XSetInputFocus(display, focused, revert, CurrentTime);
    XSync(display, False);
}

static void reply(const char *kind) {
    XSync(display, False);
    XkbStateRec state;
    if (XkbGetState(display, XkbUseCoreKbd, &state) != Success) {
        puts("{\"reply\":\"error\",\"message\":\"XkbGetState failed\"}");
        return;
    }
    int x, y;
    unsigned width, height;
    geometry(&x, &y, &width, &height);
    printf("{\"reply\":\"%s\",\"window\":\"0x%lx\",\"frame\":\"0x%lx\",\"pid\":%ld,"
           "\"x\":%d,\"y\":%d,\"width\":%u,\"height\":%u,\"icon_size\":25,"
           "\"clicks\":%u,\"covered_clicks\":%u,\"click_x\":%.3f,\"click_y\":%.3f,"
           "\"cover\":%s,\"visible\":%s,\"locked_mods\":%u,\"base_mods\":%u,"
           "\"latched_mods\":%u,\"group\":%u,\"num_mask\":%u,\"caps_mask\":%u}\n",
           kind, xid, frame, (long)getpid(), x, y, width, height, clicks, covered_clicks,
           click_x, click_y, cover ? "true" : "false", episode_visible ? "true" : "false",
           (unsigned)state.locked_mods, (unsigned)state.base_mods,
           (unsigned)state.latched_mods, (unsigned)state.group, num_mask, caps_mask);
}

static void after_paint(GdkFrameClock *clock, gpointer data) {
    (void)clock; (void)data;
    if (!pending_reply) return;
    XSync(display, False);
    int x, y;
    unsigned width, height;
    geometry(&x, &y, &width, &height);
    if (x != wanted_x || y != wanted_y || width != (unsigned)wanted_width || height != (unsigned)wanted_height) {
        gtk_widget_queue_draw(area);
        return;
    }
    pending_reply = FALSE;
    reply(first_reply ? "ready" : "ok");
    first_reply = FALSE;
}

static gboolean draw(GtkWidget *widget, cairo_t *cr, gpointer data) {
    (void)widget; (void)data;
    cairo_set_source_rgb(cr, 230.0 / 255, 230.0 / 255, 230.0 / 255);
    cairo_paint(cr);
    for (int i = 0; i < icon_count; i++) {
        cairo_set_source_surface(cr, icon, i ? 200 : 40, 50);
        cairo_pattern_set_filter(cairo_get_source(cr), CAIRO_FILTER_NEAREST);
        cairo_rectangle(cr, i ? 200 : 40, 50, 25, 25);
        cairo_fill(cr);
    }
    if (episode_visible) {
        /* Exactly cell column=2,row=1 in the 4x3 grid of a 320x240 frame. */
        cairo_set_antialias(cr, CAIRO_ANTIALIAS_NONE);
        cairo_set_source_rgb(cr, 30.0 / 255, 30.0 / 255, 30.0 / 255);
        cairo_rectangle(cr, 160, 80, 80, 80);
        cairo_fill(cr);
    }
    return TRUE;
}

static gboolean pressed(GtkWidget *widget, GdkEventButton *event, gpointer data) {
    (void)widget; (void)data;
    if (event->type == GDK_BUTTON_PRESS && event->button == 1) {
        clicks++; click_x = event->x; click_y = event->y;
    }
    return TRUE;
}

static GdkFilterReturn input_cover(GdkXEvent *event, GdkEvent *gdk_event, gpointer data) {
    (void)gdk_event; (void)data;
    XEvent *xevent = event;
    if (cover && xevent->xany.window == cover) {
        if (xevent->type == ButtonPress) covered_clicks++;
        return GDK_FILTER_REMOVE;
    }
    return GDK_FILTER_CONTINUE;
}

static gboolean command(GIOChannel *channel, GIOCondition condition, gpointer data) {
    (void)data;
    if ((condition & G_IO_ERR) || ((condition & G_IO_HUP) && !(condition & G_IO_IN))) {
        gtk_main_quit(); return G_SOURCE_REMOVE;
    }
    gchar *line = NULL;
    gsize length = 0;
    GIOStatus status = g_io_channel_read_line(channel, &line, &length, NULL, NULL);
    if (status == G_IO_STATUS_EOF) { gtk_main_quit(); return G_SOURCE_REMOVE; }
    if (status == G_IO_STATUS_AGAIN) return G_SOURCE_CONTINUE;
    if (status != G_IO_STATUS_NORMAL || length > 128 || !line) {
        g_free(line); puts("{\"reply\":\"error\",\"message\":\"invalid command\"}"); return G_SOURCE_CONTINUE;
    }
    g_strchomp(line);
    gboolean repaint = FALSE;
    int x, y, width, height, caps;
    char extra;
    if (!strcmp(line, "quit")) {
        reply("bye"); g_free(line); gtk_main_quit(); return G_SOURCE_REMOVE;
    } else if (!strcmp(line, "single") || !strcmp(line, "duplicate") || !strcmp(line, "blank")) {
        icon_count = !strcmp(line, "single") ? 1 : !strcmp(line, "duplicate") ? 2 : 0;
        episode_visible = 0; repaint = TRUE;
    } else if (!strcmp(line, "show") || !strcmp(line, "hide")) {
        icon_count = 0; episode_visible = !strcmp(line, "show"); repaint = TRUE;
    } else if (sscanf(line, "geometry %d %d %d %d %c", &x, &y, &width, &height, &extra) == 4 &&
               x >= 0 && y >= 0 && width >= 240 && width <= 500 && height >= 180 && height <= 400) {
        wanted_x = x; wanted_y = y; wanted_width = width; wanted_height = height;
        gtk_window_move(GTK_WINDOW(window), x, y);
        gtk_window_resize(GTK_WINDOW(window), width, height);
        gdk_window_move_resize(gtk_widget_get_window(window), x, y, width, height);
        repaint = TRUE;
    } else if (!strcmp(line, "cover")) {
        if (!cover) {
            Window parent = frame ? frame : DefaultRootWindow(display), child;
            XTranslateCoordinates(display, xid, parent, 0, 0, &x, &y, &child);
            XSetWindowAttributes attributes = {.override_redirect = True,
                .event_mask = ButtonPressMask | ButtonReleaseMask};
            cover = XCreateWindow(display, parent, x + 38, y + 48, 29, 29,
                                  0, 0, InputOnly, CopyFromParent,
                                  CWOverrideRedirect | CWEventMask, &attributes);
            XMapRaised(display, cover);
        }
    } else if (!strcmp(line, "uncover")) {
        if (cover) { XDestroyWindow(display, cover); cover = 0; }
    } else if (!strcmp(line, "frame") || !strcmp(line, "unframe")) {
        wrap_window(!strcmp(line, "frame")); repaint = TRUE;
    } else if (sscanf(line, "caps %d %c", &caps, &extra) == 1 && (caps == 0 || caps == 1) && caps_mask) {
        XkbLockModifiers(display, XkbUseCoreKbd, caps_mask, caps ? caps_mask : 0);
    } else if (strcmp(line, "state")) {
        puts("{\"reply\":\"error\",\"message\":\"unknown command\"}");
        g_free(line); return G_SOURCE_CONTINUE;
    }
    g_free(line);
    if (repaint) { pending_reply = TRUE; gtk_widget_queue_draw(area); }
    else reply("ok");
    return G_SOURCE_CONTINUE;
}

int main(int argc, char **argv) {
    if (private_display() || argc != 2) {
        fputs("fixture requires its owned Xvfb runner and a template output path\n", stderr); return 2;
    }
    setvbuf(stdout, NULL, _IOLBF, 0);
    gtk_init(&argc, &argv);
    GdkDisplay *gdk_display = gdk_display_get_default();
    if (!GDK_IS_X11_DISPLAY(gdk_display)) return 2;
    display = GDK_DISPLAY_XDISPLAY(gdk_display);
    XkbStateRec state;
    if (XkbGetState(display, XkbUseCoreKbd, &state) != Success) return 1;
    original_locks = state.locked_mods;
    num_mask = XkbKeysymToModifiers(display, XK_Num_Lock);
    caps_mask = XkbKeysymToModifiers(display, XK_Caps_Lock);
    cairo_surface_t *template = make_icon(20);
    cairo_status_t written = cairo_surface_write_to_png(template, argv[1]);
    cairo_surface_destroy(template);
    if (written != CAIRO_STATUS_SUCCESS) return 1;
    icon = make_icon(25);
    if (cairo_surface_status(icon) != CAIRO_STATUS_SUCCESS) return 1;
    window = gtk_window_new(GTK_WINDOW_TOPLEVEL);
    gtk_window_set_title(GTK_WINDOW(window), "zuse-owned-native-vision");
    gtk_window_set_decorated(GTK_WINDOW(window), FALSE);
    gtk_window_set_default_size(GTK_WINDOW(window), wanted_width, wanted_height);
    gtk_window_move(GTK_WINDOW(window), wanted_x, wanted_y);
    area = gtk_drawing_area_new();
    gtk_widget_add_events(area, GDK_BUTTON_PRESS_MASK | GDK_BUTTON_RELEASE_MASK);
    g_signal_connect(area, "draw", G_CALLBACK(draw), NULL);
    g_signal_connect(area, "button-press-event", G_CALLBACK(pressed), NULL);
    gtk_container_add(GTK_CONTAINER(window), area);
    gtk_widget_show_all(window);
    xid = GDK_WINDOW_XID(gtk_widget_get_window(window));
    unsigned long pid = (unsigned long)getpid();
    XChangeProperty(display, xid, XInternAtom(display, "_NET_WM_PID", False), XA_CARDINAL,
                    32, PropModeReplace, (unsigned char *)&pid, 1);
    g_signal_connect(gtk_widget_get_frame_clock(area), "after-paint", G_CALLBACK(after_paint), NULL);
    gdk_window_add_filter(NULL, input_cover, NULL);
    GIOChannel *input = g_io_channel_unix_new(STDIN_FILENO);
    g_io_channel_set_encoding(input, NULL, NULL);
    g_io_channel_set_flags(input, g_io_channel_get_flags(input) | G_IO_FLAG_NONBLOCK, NULL);
    g_io_add_watch(input, G_IO_IN | G_IO_HUP | G_IO_ERR, command, NULL);
    gtk_widget_queue_draw(area);
    gtk_main();
    if (cover) { XDestroyWindow(display, cover); cover = 0; }
    wrap_window(FALSE);
    XkbLockModifiers(display, XkbUseCoreKbd, 0xff, original_locks);
    XSync(display, False);
    gdk_window_remove_filter(NULL, input_cover, NULL);
    g_io_channel_unref(input);
    gtk_widget_destroy(window);
    cairo_surface_destroy(icon);
    return 0;
}
