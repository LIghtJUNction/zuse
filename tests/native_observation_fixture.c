#include <gtk/gtk.h>
#include <stdio.h>
#include <string.h>

static GtkWidget *label;
static GtkWidget *outside;
static GtkWidget *scope;

static gboolean command(GIOChannel *channel, GIOCondition condition, gpointer unused) {
    (void)unused;
    if (condition & (G_IO_HUP | G_IO_ERR)) { gtk_main_quit(); return FALSE; }
    gchar *line = NULL;
    if (g_io_channel_read_line(channel, &line, NULL, NULL, NULL) != G_IO_STATUS_NORMAL) return TRUE;
    g_strchomp(line);
    if (!strcmp(line, "ready")) gtk_label_set_text(GTK_LABEL(label), "ready");
    else if (!strcmp(line, "next")) gtk_label_set_text(GTK_LABEL(label), "next");
    else if (!strcmp(line, "outside")) gtk_label_set_text(GTK_LABEL(outside), "unrelated");
    else if (!strcmp(line, "hide")) gtk_widget_hide(scope);
    else if (!strcmp(line, "quit")) gtk_main_quit();
    g_free(line);
    return TRUE;
}

int main(int argc, char **argv) {
    gtk_init(&argc, &argv);
    GtkWidget *window = gtk_window_new(GTK_WINDOW_TOPLEVEL);
    gtk_window_set_title(GTK_WINDOW(window), "zuse-observation-fixture");
    gtk_window_set_default_size(GTK_WINDOW(window), 320, 160);
    GtkWidget *box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 8);
    gtk_container_add(GTK_CONTAINER(window), box);
    scope = gtk_box_new(GTK_ORIENTATION_VERTICAL, 0);
    atk_object_set_name(gtk_widget_get_accessible(scope), "zuse-observation-scope");
    gtk_box_pack_start(GTK_BOX(box), scope, TRUE, TRUE, 0);
    label = gtk_label_new("waiting");
    gtk_box_pack_start(GTK_BOX(scope), label, TRUE, TRUE, 0);
    outside = gtk_label_new("outside");
    gtk_box_pack_start(GTK_BOX(box), outside, FALSE, FALSE, 0);
    g_signal_connect(window, "destroy", G_CALLBACK(gtk_main_quit), NULL);
    GIOChannel *input = g_io_channel_unix_new(0);
    g_io_add_watch(input, G_IO_IN | G_IO_HUP | G_IO_ERR, command, NULL);
    gtk_widget_show_all(window);
    puts("READY"); fflush(stdout);
    gtk_main();
    g_io_channel_unref(input);
    return 0;
}
