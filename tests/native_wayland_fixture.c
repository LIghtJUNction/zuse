/* Private GTK4 Wayland fixture for native_wayland.py. */
#include <gtk/gtk.h>
#include <stdio.h>
#include <unistd.h>
static const char *title;
static void changed(GtkEditable *entry, gpointer data) {
    (void)data;
    const unsigned char *text=(const unsigned char*)gtk_editable_get_text(entry);
    fputs("TEXT ", stdout);
    for (;*text;text++) printf("%02x",*text);
    puts(""); fflush(stdout);
}
static gboolean key(GtkEventControllerKey *controller, guint keyval, guint code,
                    GdkModifierType state, gpointer data) {
    (void)controller; (void)data;
    printf("KEY %u %u %u\n",keyval,code,(unsigned)state);fflush(stdout);
    printf("HELD DOWN %u %u %u %lld\n",keyval,code,(unsigned)state,(long long)g_get_monotonic_time());fflush(stdout);
    /* A deterministic shortcut target: log Ctrl+T without letting an entry
     * action transpose text and obscure the following keypad assertion. */
    return keyval==GDK_KEY_t && (state&GDK_CONTROL_MASK)!=0;
}
static void released(GtkEventControllerKey *controller, guint keyval, guint code,
                     GdkModifierType state, gpointer data) {
    (void)controller; (void)data;
    printf("HELD UP %u %u %u %lld\n",keyval,code,(unsigned)state,(long long)g_get_monotonic_time());fflush(stdout);
}
static void click(GtkGestureClick *gesture, int count, double x,double y,gpointer data) {
    (void)count;(void)data;
    printf("BUTTON %u %.1f %.1f\n",gtk_gesture_single_get_current_button(GTK_GESTURE_SINGLE(gesture)),x,y);
    fflush(stdout);
}
static gboolean scroll(GtkEventControllerScroll *controller, double x,double y,gpointer data) {
    (void)controller;(void)data;
    printf("SCROLL %.1f %.1f\n",x,y);fflush(stdout);return FALSE;
}
static void draw(GtkDrawingArea *area,cairo_t *cr,int w,int h,gpointer data) {
    (void)area;(void)w;(void)h;(void)data;
    cairo_set_source_rgb(cr,32.0/255,160.0/255,96.0/255);cairo_paint(cr);
}
static gboolean timeout(gpointer data) { g_application_quit(G_APPLICATION(data));return G_SOURCE_REMOVE; }
static void activate(GtkApplication *app,gpointer data) {
    (void)data;
    GtkWidget *window=gtk_application_window_new(app);
    gtk_window_set_title(GTK_WINDOW(window),title);
    gtk_window_set_default_size(GTK_WINDOW(window),440,260);
    gtk_window_set_decorated(GTK_WINDOW(window),FALSE);
    GtkWidget *box=gtk_box_new(GTK_ORIENTATION_VERTICAL,0);
    GtkWidget *entry=gtk_entry_new();
    gtk_widget_set_size_request(entry,-1,40);
    g_signal_connect(entry,"changed",G_CALLBACK(changed),NULL);
    gtk_box_append(GTK_BOX(box),entry);
    GtkWidget *area=gtk_drawing_area_new();
    gtk_widget_set_vexpand(area,TRUE);
    gtk_drawing_area_set_draw_func(GTK_DRAWING_AREA(area),draw,NULL,NULL);
    gtk_box_append(GTK_BOX(box),area);
    gtk_window_set_child(GTK_WINDOW(window),box);
    GtkEventController *keys=gtk_event_controller_key_new();
    gtk_event_controller_set_propagation_phase(keys,GTK_PHASE_CAPTURE);
    g_signal_connect(keys,"key-pressed",G_CALLBACK(key),NULL);
    g_signal_connect(keys,"key-released",G_CALLBACK(released),NULL);
    gtk_widget_add_controller(window,keys);
    GtkGesture *pointer=gtk_gesture_click_new();
    gtk_gesture_single_set_button(GTK_GESTURE_SINGLE(pointer),0);
    g_signal_connect(pointer,"pressed",G_CALLBACK(click),NULL);
    gtk_widget_add_controller(area,GTK_EVENT_CONTROLLER(pointer));
    GtkEventController *wheel=gtk_event_controller_scroll_new(GTK_EVENT_CONTROLLER_SCROLL_BOTH_AXES);
    g_signal_connect(wheel,"scroll",G_CALLBACK(scroll),NULL);
    gtk_widget_add_controller(area,wheel);
    gtk_window_present(GTK_WINDOW(window));
    gtk_widget_grab_focus(entry);
    printf("READY %ld\n",(long)getpid());fflush(stdout);
    g_timeout_add_seconds(120,timeout,app);
}
int main(int argc,char **argv) {
    title=argc>1?argv[1]:"zuse private Wayland fixture";
    GtkApplication *app=gtk_application_new("dev.zuse.NativeFixture",G_APPLICATION_NON_UNIQUE);
    g_signal_connect(app,"activate",G_CALLBACK(activate),NULL);
    int status=g_application_run(G_APPLICATION(app),1,argv);
    g_object_unref(app);return status;
}
