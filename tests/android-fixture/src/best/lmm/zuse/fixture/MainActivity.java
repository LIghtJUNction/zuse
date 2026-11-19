package best.lmm.zuse.fixture;

import android.app.Activity;
import android.app.Presentation;
import android.content.Context;
import android.content.res.Configuration;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.PixelFormat;
import android.graphics.Point;
import android.graphics.Rect;
import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.media.Image;
import android.media.ImageReader;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.Process;
import android.os.SystemClock;
import android.text.Editable;
import android.text.InputFilter;
import android.text.InputType;
import android.text.TextWatcher;
import android.util.AtomicFile;
import android.util.DisplayMetrics;
import android.view.Display;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;
import android.view.inputmethod.EditorInfo;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.TextView;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.UUID;

/** Test-only app. Observes its own views and input, and requests no permissions. */
public final class MainActivity extends Activity {
    private static final String PACKAGE = "best.lmm.zuse.fixture";
    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    private static final Map<String, FixturePanel> PANELS = new LinkedHashMap<>();
    private static long creationOrder;
    private FixturePanel panel;
    private VirtualDisplay ownedDisplay;
    private ImageReader ownedReader;
    private FixturePresentation presentation;
    private String virtualError = "";
    private String virtualName = "";

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        immersive(getWindow());
        panel = new FixturePanel(this, this, "activity", "");
        setContentView(panel);
        panel.lifecycle = "created";
        panel.queueState();
        if (getIntent().getBooleanExtra("virtual_display", false)) {
            panel.post(this::createOwnedDisplay);
        }
    }

    @Override protected void onResume() {
        super.onResume();
        immersive(getWindow());
        if (panel != null) { panel.lifecycle = "resumed"; panel.queueState(); }
    }

    @Override protected void onPause() {
        if (panel != null) { panel.lifecycle = "paused"; panel.saveState(); }
        super.onPause();
    }

    @Override public void onConfigurationChanged(Configuration configuration) {
        super.onConfigurationChanged(configuration);
        immersive(getWindow());
        if (panel != null) panel.queueState();
    }

    @Override public void onWindowFocusChanged(boolean focus) {
        super.onWindowFocusChanged(focus);
        if (focus) immersive(getWindow());
    }

    @Override public void onBackPressed() {
        // Keep a test BACK inside the fixture instead of exposing another app.
        if (panel != null) { panel.backPresses++; panel.queueState(); }
    }

    @Override protected void onDestroy() {
        releaseOwnedDisplay();
        if (panel != null) panel.destroy();
        super.onDestroy();
    }

    @Override public void dump(String prefix, FileDescriptor fd, PrintWriter writer, String[] args) {
        if (panel != null) writer.println(prefix + "ZUSE_FIXTURE_STATE " + panel.lastJson);
        if (presentation != null && presentation.fixture != null) {
            writer.println(prefix + "ZUSE_FIXTURE_STATE " + presentation.fixture.lastJson);
        }
    }

    private static void immersive(Window window) {
        if (window == null) return;
        window.addFlags(WindowManager.LayoutParams.FLAG_FULLSCREEN);
        window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_HIDDEN
                | WindowManager.LayoutParams.SOFT_INPUT_ADJUST_NOTHING);
        window.getDecorView().setSystemUiVisibility(
                View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY | View.SYSTEM_UI_FLAG_FULLSCREEN
                | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION | View.SYSTEM_UI_FLAG_LAYOUT_STABLE);
        if (Build.VERSION.SDK_INT >= 30) {
            window.setDecorFitsSystemWindows(false);
            WindowInsetsController controller = window.getInsetsController();
            if (controller != null) {
                controller.hide(WindowInsets.Type.systemBars());
                controller.setSystemBarsBehavior(
                        WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
            }
        }
    }

    private void createOwnedDisplay() {
        if (ownedDisplay != null || isFinishing()) return;
        try {
            int width = boundedExtra("virtual_width", 640, 320, 1024);
            int height = boundedExtra("virtual_height", 960, 480, 1280);
            int dpi = boundedExtra("virtual_dpi", 160, 120, 320);
            ownedReader = ImageReader.newInstance(width, height, PixelFormat.RGBA_8888, 2);
            // Drain only our own render surface. Never capture or mirror another display.
            ownedReader.setOnImageAvailableListener(reader -> {
                try (Image frame = reader.acquireLatestImage()) {
                    // Deliberately discard each frame; external capture is what zuse tests.
                } catch (IllegalStateException ignored) {
                    // A final callback may arrive while this owned surface is closing.
                }
            }, MAIN);
            virtualName = "zuse-fixture-" + panel.instance;
            DisplayManager manager = (DisplayManager) getSystemService(DISPLAY_SERVICE);
            ownedDisplay = manager.createVirtualDisplay(virtualName, width, height, dpi,
                    ownedReader.getSurface(),
                    DisplayManager.VIRTUAL_DISPLAY_FLAG_PUBLIC
                            | DisplayManager.VIRTUAL_DISPLAY_FLAG_OWN_CONTENT_ONLY
                            | DisplayManager.VIRTUAL_DISPLAY_FLAG_PRESENTATION);
            if (ownedDisplay == null) throw new IllegalStateException("createVirtualDisplay returned null");
            presentation = new FixturePresentation(this, ownedDisplay.getDisplay());
            presentation.show();
            panel.queueState();
        } catch (RuntimeException error) {
            virtualError = error.getClass().getSimpleName() + ": " + String.valueOf(error.getMessage());
            releaseOwnedDisplay();
            panel.queueState();
        }
    }

    private int boundedExtra(String name, int fallback, int min, int max) {
        int value = getIntent().getIntExtra(name, fallback);
        if (value < min || value > max) throw new IllegalArgumentException(name + " out of range");
        return value;
    }

    private void releaseOwnedDisplay() {
        if (presentation != null) { presentation.dismiss(); presentation = null; }
        if (ownedDisplay != null) { ownedDisplay.release(); ownedDisplay = null; }
        if (ownedReader != null) { ownedReader.close(); ownedReader = null; }
        if (panel != null) panel.queueState();
    }

    private final class FixturePresentation extends Presentation {
        private FixturePanel fixture;
        FixturePresentation(Context context, Display display) {
            super(context, display, android.R.style.Theme_Material_Light_NoActionBar);
        }
        @Override protected void onCreate(Bundle state) {
            super.onCreate(state);
            immersive(getWindow());
            fixture = new FixturePanel(getContext(), MainActivity.this, "presentation", panel.instance);
            setContentView(fixture);
            Window window = getWindow();
            if (window != null) window.setLayout(
                    WindowManager.LayoutParams.MATCH_PARENT, WindowManager.LayoutParams.MATCH_PARENT);
            fixture.lifecycle = "shown";
            fixture.queueState();
        }
        @Override public void onBackPressed() {
            if (fixture != null) { fixture.backPresses++; fixture.queueState(); }
        }
        @Override public void dismiss() {
            if (fixture != null) fixture.destroy();
            super.dismiss();
        }
    }

    private static final class FixturePanel extends LinearLayout {
        final MainActivity owner;
        final String instance = UUID.randomUUID().toString();
        final String role;
        final String ownerInstance;
        final long order = ++creationOrder;
        final EditText input;
        final Button counterButton;
        final Button resetButton;
        final Button exitButton;
        final TextView heading;
        final TextView summary;
        final Swatches swatches;
        final GestureArea gesture;
        final ArrayDeque<JSONObject> keys = new ArrayDeque<>();
        int counter, editorActions, backPresses, keyDowns, keyUps;
        int inputFocusChanges, inputTouchEvents, inputLastTouchAction = -1;
        long inputLastFocusUptimeMs, inputLastTouchUptimeMs;
        long sequence;
        String lifecycle = "created";
        boolean destroyed, queued;
        String lastJson = "{}";
        String storageError = "";
        int lastDisplayId = -1;

        FixturePanel(Context context, MainActivity owner, String role, String ownerInstance) {
            super(context);
            this.owner = owner; this.role = role; this.ownerInstance = ownerInstance;
            PANELS.put(instance, this);
            setOrientation(VERTICAL);
            setPadding(dp(20), dp(20), dp(20), dp(20));
            setBackgroundColor(Color.WHITE);
            setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO_EXCLUDE_DESCENDANTS);
            setFocusableInTouchMode(true);

            heading = label("ZUSE_FIXTURE_V1  display -1\n" + instance.substring(0, 8), 20);
            heading.setMinLines(2);
            heading.setMaxLines(2);
            addView(heading, new LayoutParams(-1, -2));
            TextView subtitle = label("Only this app's synthetic test data", 13);
            addView(subtitle, new LayoutParams(-1, -2));

            swatches = new Swatches(context, instance);
            addView(swatches, row(dp(32), 12));

            input = new InputField(context, this);
            input.setOnFocusChangeListener((view, focus) -> {
                inputFocusChanges++; inputLastFocusUptimeMs = SystemClock.uptimeMillis(); queueState();
            });
            input.setContentDescription("zuse fixture input");
            input.setHint("Type fixture text");
            input.setTextColor(Color.BLACK);
            input.setTextSize(18);
            input.setSingleLine(true);
            input.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
            input.setImeOptions(EditorInfo.IME_ACTION_DONE | EditorInfo.IME_FLAG_NO_EXTRACT_UI);
            input.setShowSoftInputOnFocus(false);
            input.setFilters(new InputFilter[] { new InputFilter.LengthFilter(512) });
            addView(input, row(dp(56), 12));

            LinearLayout buttons = new LinearLayout(context);
            buttons.setOrientation(HORIZONTAL);
            counterButton = button(context, "Count: 0", "zuse fixture counter");
            resetButton = button(context, "Reset", "zuse fixture reset");
            exitButton = button(context, "Exit", "zuse fixture exit");
            buttons.addView(counterButton, new LayoutParams(0, dp(52), 1));
            buttons.addView(resetButton, new LayoutParams(0, dp(52), 1));
            buttons.addView(exitButton, new LayoutParams(0, dp(52), 1));
            addView(buttons, row(dp(52), 8));

            summary = label("Ready", 14);
            summary.setMinHeight(dp(48));
            addView(summary, row(-2, 8));
            gesture = new GestureArea(context, this);
            gesture.setContentDescription("zuse fixture gesture");
            gesture.setMinimumHeight(dp(100));
            LayoutParams area = new LayoutParams(-1, 0, 1);
            area.topMargin = dp(8);
            addView(gesture, area);

            counterButton.setOnClickListener(view -> { counter++; queueState(); });
            resetButton.setOnClickListener(view -> {
                counter = editorActions = backPresses = keyDowns = keyUps = 0;
                keys.clear(); input.setText(""); gesture.reset(); requestFocus(); queueState();
            });
            exitButton.setOnClickListener(view -> {
                if ("presentation".equals(role)) owner.releaseOwnedDisplay();
                else owner.finishAndRemoveTask();
            });
            input.addTextChangedListener(new TextWatcher() {
                @Override public void beforeTextChanged(CharSequence s, int start, int count, int after) {}
                @Override public void onTextChanged(CharSequence s, int start, int before, int count) {}
                @Override public void afterTextChanged(Editable text) { queueState(); }
            });
            input.setOnEditorActionListener((view, action, event) -> {
                editorActions++; queueState(); return true;
            });
            getViewTreeObserver().addOnGlobalLayoutListener(this::queueState);
            requestFocus();
        }

        private TextView label(String text, int size) {
            TextView view = new TextView(getContext());
            view.setText(text); view.setTextColor(Color.rgb(32, 33, 36)); view.setTextSize(size);
            view.setGravity(Gravity.CENTER_VERTICAL);
            return view;
        }
        private static Button button(Context context, String text, String description) {
            Button button = new Button(context);
            button.setText(text); button.setAllCaps(false); button.setContentDescription(description);
            return button;
        }
        private int dp(int value) { return Math.round(value * getResources().getDisplayMetrics().density); }
        private LayoutParams row(int height, int top) {
            LayoutParams params = new LayoutParams(-1, height); params.topMargin = dp(top); return params;
        }
        @Override protected void onAttachedToWindow() { super.onAttachedToWindow(); queueState(); }
        @Override public void onWindowFocusChanged(boolean focus) {
            super.onWindowFocusChanged(focus); queueState();
        }
        @Override public boolean dispatchKeyEvent(KeyEvent event) {
            try {
                JSONObject key = new JSONObject();
                key.put("action", event.getAction()).put("code", event.getKeyCode())
                        .put("name", KeyEvent.keyCodeToString(event.getKeyCode()))
                        .put("unicode", event.getUnicodeChar()).put("repeat", event.getRepeatCount())
                        .put("eventTimeMs", event.getEventTime());
                key.put("recipientDisplayId", getDisplay() == null ? -1 : getDisplay().getDisplayId());
                if (keys.size() == 24) keys.removeFirst();
                keys.addLast(key);
                if (event.getAction() == KeyEvent.ACTION_DOWN) keyDowns++;
                if (event.getAction() == KeyEvent.ACTION_UP) keyUps++;
            } catch (JSONException error) { throw new IllegalStateException(error); }
            boolean handled = super.dispatchKeyEvent(event);
            queueState(); return handled;
        }

        void queueState() {
            if (destroyed || queued) return;
            queued = true;
            MAIN.postDelayed(() -> { queued = false; if (!destroyed) saveState(); }, 24);
        }

        void destroy() {
            if (destroyed) return;
            destroyed = true; lifecycle = "destroyed"; saveState(); PANELS.remove(instance);
            refreshDisplayAlias(owner, lastDisplayId);
        }

        void saveState() {
            try {
                int displayId = getDisplay() == null ? -1 : getDisplay().getDisplayId();
                lastDisplayId = displayId;
                String title = "ZUSE_FIXTURE_V1  display " + displayId + "\n" + instance.substring(0, 8);
                if (!heading.getText().toString().equals(title)) heading.setText(title);
                String count = "Count: " + counter;
                if (!counterButton.getText().toString().equals(count)) counterButton.setText(count);
                String status = "Text " + input.length() + " | enter " + editorActions
                        + " | back " + backPresses + " | taps " + gesture.taps + " | swipes " + gesture.swipes;
                if (!summary.getText().toString().equals(status)) summary.setText(status);
                JSONObject state = new JSONObject();
                DisplayMetrics metrics = getResources().getDisplayMetrics();
                Point size = new Point();
                Display display = getDisplay();
                if (display != null) display.getRealSize(size);
                state.put("schema", 1).put("package", PACKAGE).put("instance", instance)
                        .put("role", role).put("ownerInstance", ownerInstance)
                        .put("pid", Process.myPid()).put("taskId", owner.getTaskId())
                        .put("displayId", displayId).put("displayName", display == null ? "" : display.getName())
                        .put("displayWidth", size.x).put("displayHeight", size.y)
                        .put("densityDpi", metrics.densityDpi).put("sequence", ++sequence)
                        .put("uptimeMs", SystemClock.uptimeMillis())
                        .put("ready", isAttachedToWindow() && getWidth() > 0 && getHeight() > 0
                                && !isLayoutRequested() && !destroyed)
                        .put("lifecycle", lifecycle).put("windowFocus", hasWindowFocus()).put("destroyed", destroyed)
                        .put("text", input.getText().toString())
                        .put("selectionStart", input.getSelectionStart()).put("selectionEnd", input.getSelectionEnd())
                        .put("inputDiagnostics", new JSONObject().put("focusChanges", inputFocusChanges)
                                .put("lastFocusUptimeMs", inputLastFocusUptimeMs)
                                .put("touchEvents", inputTouchEvents).put("lastTouchAction", inputLastTouchAction)
                                .put("lastTouchUptimeMs", inputLastTouchUptimeMs))
                        .put("counter", counter).put("editorActions", editorActions).put("backPresses", backPresses)
                        .put("keyDowns", keyDowns).put("keyUps", keyUps).put("keys", new JSONArray(keys))
                        .put("gesture", gesture.state()).put("storageError", storageError)
                        .put("ownedVirtualDisplayId", owner.ownedDisplay == null ? -1
                                : owner.ownedDisplay.getDisplay().getDisplayId())
                        .put("ownedVirtualDisplayName", owner.virtualName)
                        .put("virtualError", owner.virtualError);
                JSONObject controls = new JSONObject();
                controls.put("root", bounds(this)).put("input", bounds(input))
                        .put("counter", bounds(counterButton)).put("reset", bounds(resetButton))
                        .put("exit", bounds(exitButton)).put("gesture", bounds(gesture))
                        .put("swatches", bounds(swatches));
                state.put("controls", controls).put("pixelSamples", swatches.samples());
                lastJson = state.toString();
                write(owner, "state-" + instance + ".json", lastJson);
                write(owner, "latest.json", lastJson);
                refreshDisplayAlias(owner, displayId);
                JSONArray index = new JSONArray();
                for (FixturePanel item : PANELS.values()) {
                    index.put(new JSONObject().put("instance", item.instance).put("role", item.role)
                            .put("displayId", item.lastDisplayId).put("destroyed", item.destroyed)
                            .put("file", "state-" + item.instance + ".json"));
                }
                write(owner, "instances.json", index.toString());
            } catch (IOException error) {
                storageError = error.getClass().getSimpleName() + ": " + error.getMessage();
            } catch (JSONException error) {
                throw new IllegalStateException(error);
            }
        }

        static void refreshDisplayAlias(Context context, int displayId) {
            if (displayId < 0) return;
            FixturePanel selected = null;
            for (FixturePanel candidate : PANELS.values()) {
                if (candidate.destroyed || candidate.lastDisplayId != displayId) continue;
                if (selected == null || (candidate.hasWindowFocus() && !selected.hasWindowFocus())
                        || candidate.hasWindowFocus() == selected.hasWindowFocus() && candidate.order > selected.order) {
                    selected = candidate;
                }
            }
            try {
                if (selected != null) write(context, "display-" + displayId + ".json", selected.lastJson);
                else new AtomicFile(new File(context.getFilesDir(), "display-" + displayId + ".json")).delete();
            } catch (IOException error) {
                if (selected != null) selected.storageError = error.toString();
            }
        }

        static JSONObject bounds(View view) throws JSONException {
            int[] location = new int[2]; view.getLocationOnScreen(location);
            Rect visible = new Rect(); boolean shown = view.getGlobalVisibleRect(visible);
            return new JSONObject().put("x", location[0]).put("y", location[1])
                    .put("width", view.getWidth()).put("height", view.getHeight())
                    .put("centerX", location[0] + view.getWidth() / 2)
                    .put("centerY", location[1] + view.getHeight() / 2)
                    .put("visible", shown).put("focused", view.hasFocus())
                    .put("visibleLeft", visible.left).put("visibleTop", visible.top)
                    .put("visibleRight", visible.right).put("visibleBottom", visible.bottom);
        }
    }

    private static final class InputField extends EditText {
        private final FixturePanel panel;
        InputField(Context context, FixturePanel panel) { super(context); this.panel = panel; }
        @Override public boolean onTouchEvent(MotionEvent event) {
            boolean handled = super.onTouchEvent(event);
            if (panel != null) {
                panel.inputTouchEvents++; panel.inputLastTouchAction = event.getActionMasked();
                panel.inputLastTouchUptimeMs = event.getEventTime(); panel.queueState();
            }
            return handled;
        }
        @Override protected void onSelectionChanged(int start, int end) {
            super.onSelectionChanged(start, end);
            if (panel != null) panel.queueState();
        }
    }

    private static final class Swatches extends View {
        final int[] colors;
        final Paint paint = new Paint();
        Swatches(Context context, String instance) {
            super(context);
            int hash = instance.hashCode();
            colors = new int[] { 0xffe53935, 0xff1e8e3e, 0xff1a73e8,
                    Color.rgb(48 + (hash & 127), 48 + ((hash >>> 8) & 127), 48 + ((hash >>> 16) & 127)) };
        }
        @Override protected void onDraw(Canvas canvas) {
            for (int i = 0; i < colors.length; i++) {
                paint.setColor(colors[i]);
                canvas.drawRect(i * getWidth() / 4f, 0, (i + 1) * getWidth() / 4f, getHeight(), paint);
            }
        }
        JSONArray samples() throws JSONException {
            int[] location = new int[2]; getLocationOnScreen(location);
            JSONArray samples = new JSONArray();
            for (int i = 0; i < colors.length; i++) {
                samples.put(new JSONObject().put("x", location[0] + (2 * i + 1) * getWidth() / 8)
                        .put("y", location[1] + getHeight() / 2)
                        .put("argb", String.format(java.util.Locale.ROOT, "#%08X", colors[i]))
                        .put("red", Color.red(colors[i])).put("green", Color.green(colors[i]))
                        .put("blue", Color.blue(colors[i])));
            }
            return samples;
        }
    }

    private static final class GestureArea extends View {
        final FixturePanel panel;
        final Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
        int taps, swipes, moves, cancelled, recipientDisplayId = -1;
        float downX, downY, upX, upY, lastX, lastY, maxDistance;
        long durationMs, downTime;
        String lastKind = "none";
        GestureArea(Context context, FixturePanel panel) { super(context); this.panel = panel; setClickable(true); }
        void reset() {
            taps = swipes = moves = cancelled = 0; lastKind = "none";
            downX = downY = upX = upY = lastX = lastY = maxDistance = 0; durationMs = 0;
            invalidate();
        }
        @Override protected void onDraw(Canvas canvas) {
            canvas.drawColor(0xffe8f0fe);
            paint.setColor(0xff174ea6); paint.setTextSize(16 * getResources().getDisplayMetrics().scaledDensity);
            canvas.drawText("Gesture test area", 16, 28 * getResources().getDisplayMetrics().density, paint);
            if (!"none".equals(lastKind)) {
                paint.setStrokeWidth(4);
                canvas.drawLine(downX, downY, lastX, lastY, paint);
                canvas.drawCircle(lastX, lastY, 8, paint);
            }
        }
        @Override public boolean onTouchEvent(MotionEvent event) {
            lastX = event.getX(); lastY = event.getY();
            recipientDisplayId = getDisplay() == null ? -1 : getDisplay().getDisplayId();
            switch (event.getActionMasked()) {
                case MotionEvent.ACTION_DOWN:
                    downX = lastX; downY = lastY; maxDistance = 0; moves = 0;
                    downTime = event.getEventTime(); lastKind = "down";
                    getParent().requestDisallowInterceptTouchEvent(true); break;
                case MotionEvent.ACTION_MOVE:
                    moves++; maxDistance = Math.max(maxDistance,
                            (float) Math.hypot(lastX - downX, lastY - downY)); break;
                case MotionEvent.ACTION_UP:
                    upX = lastX; upY = lastY; durationMs = event.getEventTime() - downTime;
                    maxDistance = Math.max(maxDistance, (float) Math.hypot(upX - downX, upY - downY));
                    if (maxDistance >= 12 * getResources().getDisplayMetrics().density) {
                        swipes++; lastKind = "swipe";
                    } else { taps++; lastKind = "tap"; performClick(); }
                    break;
                case MotionEvent.ACTION_CANCEL:
                    cancelled++; lastKind = "cancel"; break;
                default: break;
            }
            invalidate(); panel.queueState(); return true;
        }
        @Override public boolean performClick() { super.performClick(); return true; }
        JSONObject state() throws JSONException {
            int[] location = new int[2]; getLocationOnScreen(location);
            return new JSONObject().put("taps", taps).put("swipes", swipes).put("moves", moves)
                    .put("cancelled", cancelled).put("lastKind", lastKind).put("durationMs", durationMs)
                    .put("recipientDisplayId", recipientDisplayId)
                    .put("downX", location[0] + downX).put("downY", location[1] + downY)
                    .put("upX", location[0] + upX).put("upY", location[1] + upY)
                    .put("suggestedSwipe", new JSONObject()
                            .put("x1", location[0] + getWidth() / 4).put("y1", location[1] + getHeight() / 2)
                            .put("x2", location[0] + 3 * getWidth() / 4).put("y2", location[1] + getHeight() / 2));
        }
    }

    private static void write(Context context, String name, String json) throws IOException {
        AtomicFile file = new AtomicFile(new File(context.getFilesDir(), name));
        FileOutputStream stream = null;
        try {
            stream = file.startWrite();
            stream.write((json + "\n").getBytes(StandardCharsets.UTF_8));
            file.finishWrite(stream);
        } catch (IOException error) {
            if (stream != null) file.failWrite(stream);
            throw error;
        }
    }
}
