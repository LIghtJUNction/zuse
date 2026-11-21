// Runs in an isolated world of the selected main frame. Keep the element itself:
// replacing it with an identical node must invalidate the hold.
(() => {
    const node = document.activeElement;
    const url = location.href;
    const listeners = new AbortController();
    let stale = false;
    const invalidate = () => { stale = true; };
    for (const type of ["blur", "focusout", "pagehide", "visibilitychange", "popstate", "hashchange"]) {
        window.addEventListener(type, invalidate, {capture: true, signal: listeners.signal});
    }
    // Also retire listeners if the controller disappears during attachment.
    const expiry = setTimeout(() => { invalidate(); listeners.abort(); }, 65000);
    return {
        node,
        valid() {
            return !stale && document.hasFocus() && document.visibilityState === "visible" &&
                location.href === url && node?.isConnected && document.activeElement === node &&
                !["IFRAME", "FRAME", "OBJECT", "EMBED"].includes(node.tagName);
        },
        close() { clearTimeout(expiry); listeners.abort(); }
    };
})()
