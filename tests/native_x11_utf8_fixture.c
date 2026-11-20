/* Owned Xvfb/AT-SPI receiver. No code here is linked into the library. */
#include <gtk/gtk.h>
#include <gdk/gdkx.h>
#include <X11/Xlib.h>
#include <X11/Xatom.h>
#include <X11/keysym.h>
#include <X11/extensions/XTest.h>
#include <X11/XKBlib.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static GtkWidget *window, *entry, *other, *other_entry, *button;
static Display *display;
static unsigned pause_ms;
static int focus_on_change, mutate_selection;
static int changes;
static int clipboard_clears;
static void clipboard_get(GtkClipboard *clipboard, GtkSelectionData *data, guint info, gpointer value) {
    (void)clipboard; (void)value;
    if (info == 0) gtk_selection_data_set_text(data, "keep my clipboard 中文", -1);
    else {
        const guchar bytes[] = {0x00, 0xff, 0x2a, 0x80};
        gtk_selection_data_set(data, gtk_selection_data_get_target(data), 8, bytes, sizeof bytes);
    }
}
static void clipboard_clear(GtkClipboard *clipboard, gpointer value) {
    (void)clipboard; (void)value; clipboard_clears++;
}
static void changed(GtkEditable *editable, gpointer value) {
    (void)editable; (void)value; changes++;
    if (mutate_selection) {
        mutate_selection = 0;
        if(!strcmp(gtk_entry_get_text(GTK_ENTRY(entry)),"左🙂旧右"))
            gtk_entry_set_text(GTK_ENTRY(entry),"左🙂新右");
    }
    if (focus_on_change) {
        focus_on_change = 0;
        gtk_widget_grab_focus(other_entry);
        gdk_window_focus(gtk_widget_get_window(other), GDK_CURRENT_TIME);
        XSync(display,False);
    }
    if (pause_ms) { unsigned ms = pause_ms; pause_ms = 0; g_usleep(ms * 1000); }
}
static void state(void) {
    gchar *text = g_base64_encode((const guchar*)gtk_entry_get_text(GTK_ENTRY(entry)),
                                 strlen(gtk_entry_get_text(GTK_ENTRY(entry))));
    gchar *other_text = g_base64_encode((const guchar*)gtk_entry_get_text(GTK_ENTRY(other_entry)),
                                       strlen(gtk_entry_get_text(GTK_ENTRY(other_entry))));
    int start = -1, end = -1;
    gtk_editable_get_selection_bounds(GTK_EDITABLE(entry), &start, &end);
    printf("STATE %s %s %d %d %d %d %d\n", *text ? text : "-", *other_text ? other_text : "-",
           gtk_editable_get_position(GTK_EDITABLE(entry)), start, end, changes, clipboard_clears);
    g_free(text); g_free(other_text); fflush(stdout);
}
static void native_state(void) {
    int min, max, width;
    XDisplayKeycodes(display, &min, &max);
    KeySym *mapping = XGetKeyboardMapping(display, min, max-min+1, &width);
    gchar *map = g_base64_encode((const guchar*)mapping, (max-min+1)*width*sizeof(KeySym));
    char keys[32] = {0}; XQueryKeymap(display, keys);
    gchar *key = g_base64_encode((const guchar*)keys, sizeof keys);
    XkbStateRec kb = {0}; XkbGetState(display, XkbUseCoreKbd, &kb);
    printf("NATIVE %d %d %d %s %s %u,%u,%u,%u,%u,%u,%u,%u %lu %lu\n",
        min,max,width,map,key,kb.group,kb.locked_group,kb.base_group,kb.latched_group,
        kb.mods,kb.base_mods,kb.latched_mods,kb.locked_mods,
        XGetSelectionOwner(display,XInternAtom(display,"CLIPBOARD",False)),
        XGetSelectionOwner(display,XA_PRIMARY));
    fflush(stdout); g_free(map); g_free(key); XFree(mapping);
}
static gboolean command(GIOChannel *channel, GIOCondition condition, gpointer data) {
    (void)data;
    if (condition & G_IO_HUP) { gtk_main_quit(); return FALSE; }
    gchar *line = NULL;
    if (g_io_channel_read_line(channel, &line, NULL, NULL, NULL) != G_IO_STATUS_NORMAL) return TRUE;
    g_strchomp(line);
    if (!strcmp(line, "STATE")) { state(); g_free(line); return TRUE; }
    if (!strcmp(line, "NATIVE")) { native_state(); g_free(line); return TRUE; }
    if (!strncmp(line, "RESET ", 6)) {
        gsize length; gchar *decoded = (gchar*)g_base64_decode(line+6, &length);
        gchar *text = g_strndup(decoded, length); g_free(decoded);
        pause_ms = 0; focus_on_change = 0; mutate_selection = 0;
        gtk_entry_set_max_length(GTK_ENTRY(entry), 0);
        gtk_entry_set_text(GTK_ENTRY(entry), text); g_free(text);
        gtk_entry_set_text(GTK_ENTRY(other_entry), "other window untouched");
        gtk_editable_set_position(GTK_EDITABLE(entry), -1);
        gtk_widget_grab_focus(entry); gdk_window_focus(gtk_widget_get_window(window), GDK_CURRENT_TIME);
        changes = 0;
    } else if (!strncmp(line, "SELECT ", 7)) {
        int a,b; if(sscanf(line+7,"%d %d",&a,&b)==2)gtk_editable_select_region(GTK_EDITABLE(entry),a,b);
    } else if (!strncmp(line, "CARET ", 6)) {
        gtk_editable_set_position(GTK_EDITABLE(entry), atoi(line+6));
    } else if (!strncmp(line, "PAUSE ", 6)) {
        pause_ms = atoi(line+6);
    } else if (!strcmp(line,"FOCUS_ON_CHANGE")) {
        focus_on_change = 1;
    } else if (!strcmp(line,"MUTATE_SELECTION")) {
        mutate_selection = 1;
    } else if (!strncmp(line, "LIMIT ", 6)) {
        gtk_entry_set_max_length(GTK_ENTRY(entry), atoi(line+6));
    } else if (!strcmp(line, "BUTTON")) {
        gtk_widget_grab_focus(button);
    } else if (!strcmp(line, "SHIFT_DOWN") || !strcmp(line, "SHIFT_UP")) {
        XTestFakeKeyEvent(display, XKeysymToKeycode(display, XK_Shift_L), !strcmp(line, "SHIFT_DOWN"), 0);
        XSync(display,False);
    } else if (!strncmp(line, "PID ", 4)) {
        unsigned long pid = strtoul(line+4,NULL,10);
        XChangeProperty(display, GDK_WINDOW_XID(gtk_widget_get_window(window)), XInternAtom(display,"_NET_WM_PID",False), XA_CARDINAL,32,PropModeReplace,(const unsigned char*)&pid,1);
        XSync(display,False);
    } else if (!strncmp(line,"MACHINE ",8)) {
        char hostname[256] = {0};
        if(!strcmp(line+8,"local"))gethostname(hostname,sizeof hostname-1);
        else snprintf(hostname,sizeof hostname,"%s",line+8);
        XChangeProperty(display,GDK_WINDOW_XID(gtk_widget_get_window(window)),XInternAtom(display,"WM_CLIENT_MACHINE",False),XA_STRING,8,PropModeReplace,(const unsigned char*)hostname,strlen(hostname));
        XSync(display,False);
    } else if (!strcmp(line,"CLIPBOARD")) {
        GtkClipboard *clip=gtk_clipboard_get(GDK_SELECTION_CLIPBOARD);
        gchar *text=gtk_clipboard_wait_for_text(clip);
        GtkSelectionData *raw=gtk_clipboard_wait_for_contents(clip,gdk_atom_intern_static_string("application/x-zuse-test"));
        gchar *a=g_base64_encode((guchar*)(text?text:""),text?strlen(text):0);
        gchar *b=raw?g_base64_encode(gtk_selection_data_get_data(raw),gtk_selection_data_get_length(raw)):g_strdup("");
        printf("CLIPBOARD %s %s %d\n",a,b,clipboard_clears);fflush(stdout);
        g_free(text);g_free(a);g_free(b);if(raw)gtk_selection_data_free(raw);
        g_free(line);return TRUE;
    } else { fprintf(stderr,"unknown command: %s\n",line); exit(3); }
    while(gtk_events_pending()) gtk_main_iteration();
    puts("OK");fflush(stdout);g_free(line);return TRUE;
}
int main(int argc,char **argv) {
    gtk_init(&argc,&argv); display=gdk_x11_display_get_xdisplay(gdk_display_get_default());
    window=gtk_window_new(GTK_WINDOW_TOPLEVEL);gtk_window_set_title(GTK_WINDOW(window),"zuse UTF-8 receiver");
    gtk_window_set_default_size(GTK_WINDOW(window),480,150);gtk_window_move(GTK_WINDOW(window),20,30);
    GtkWidget *box=gtk_box_new(GTK_ORIENTATION_VERTICAL,8);gtk_container_add(GTK_CONTAINER(window),box);
    entry=gtk_entry_new();button=gtk_button_new_with_label("Not an editable target");
    gtk_box_pack_start(GTK_BOX(box),entry,FALSE,FALSE,0);gtk_box_pack_start(GTK_BOX(box),button,FALSE,FALSE,0);
    atk_object_set_name(gtk_widget_get_accessible(entry),"Zuse UTF-8 editable");
    g_signal_connect(entry,"changed",G_CALLBACK(changed),NULL);
    other=gtk_window_new(GTK_WINDOW_TOPLEVEL);gtk_window_set_title(GTK_WINDOW(other),"zuse other same-process window");
    gtk_window_set_default_size(GTK_WINDOW(other),350,120);gtk_window_move(GTK_WINDOW(other),540,30);
    other_entry=gtk_entry_new();gtk_container_add(GTK_CONTAINER(other),other_entry);gtk_widget_get_accessible(other_entry);
    gtk_widget_show_all(other);gtk_widget_show_all(window);gtk_widget_grab_focus(entry);
    gdk_window_focus(gtk_widget_get_window(window),GDK_CURRENT_TIME);
    GtkTargetEntry targets[]={{"UTF8_STRING",0,0},{"application/x-zuse-test",0,1}};
    gtk_clipboard_set_with_data(gtk_clipboard_get(GDK_SELECTION_CLIPBOARD),targets,2,clipboard_get,clipboard_clear,NULL);
    GIOChannel *input=g_io_channel_unix_new(0);g_io_add_watch(input,G_IO_IN|G_IO_HUP,command,NULL);
    printf("READY %lu %lu %d\n",GDK_WINDOW_XID(gtk_widget_get_window(window)),GDK_WINDOW_XID(gtk_widget_get_window(other)),getpid());fflush(stdout);
    gtk_main();g_io_channel_unref(input);return 0;
}
