// Explicit action watch. It only reports candidates; Rust dispatches trusted
// input after claiming the cached Element. No timers, OCR or selector rescans.
const key = Symbol.for('zuse.observation.v1');
const watches = globalThis[key] ??= new Map();
if (action === 'claim') {
  const watch = watches.get(id);
  if (!watch?.claim) throw Error('click watch document was replaced');
  return watch.claim(expected);
}
if (watches.has(id)) throw Error('click watch already exists');
const owner = document, ownerWindow = window, origin = location.origin, url = location.href;
if (window.top !== window) throw Error('click watches require the selected top-level document');
if (options.expected_origin !== null && options.expected_origin !== origin) {
  throw Error('click watch origin differs from expected_origin');
}
if (!document.hasFocus() || document.visibilityState !== 'visible') {
  throw Error('click watch requires the selected page to have focus');
}
// A simple compound selector can be maintained incrementally from each changed
// element. Relational/pseudo selectors would require an implicit tree rescan.
let bracket = 0, quoted = '', escaped = false;
for (const ch of options.selector) {
  if (escaped) { escaped = false; continue; }
  if (ch === '\\') { escaped = true; continue; }
  if (quoted) { if (ch === quoted) quoted = ''; continue; }
  if (bracket && (ch === '"' || ch === "'")) { quoted = ch; continue; }
  if (ch === '[') { bracket++; continue; }
  if (ch === ']') { bracket--; continue; }
  if (!bracket && (/\s/.test(ch) || ',>+~:|'.includes(ch))) {
    throw Error('click target selector must be a simple compound selector (tag, id, class or attributes)');
  }
}
document.createElement('div').matches(options.selector); // syntax validation only
const containers = document.querySelectorAll(options.scope);
if (containers.length !== 1) throw Error('click watch scope must match exactly one container');
const root = containers[0], candidates = new Set(), identities = new WeakMap();
let nextIdentity = 1, episode = 0, revision = 0, present = false, claimed = false;
let appeared = 0, candidate = null, terminal = null, closed = false;
let mutation, parents, resize;
const pending = new Map(), received = [];
const identity = element => {
  if (!identities.has(element)) identities.set(element, String(nextIdentity++));
  return identities.get(element);
};
const emit = payload => {
  if (closed) return;
  try { globalThis[binding](JSON.stringify({watch: id, ...payload})); } catch (_) {}
};
const stop = reason => {
  if (!terminal) { terminal = reason; emit({type: 'stopped', reason, episode}); }
};
const context = () => {
  if (owner !== document || ownerWindow !== window || location.origin !== origin) {
    stop('document_or_origin_changed'); return false;
  }
  if (!root.isConnected || root.ownerDocument !== owner) { stop('scope_removed'); return false; }
  if (!owner.hasFocus() || owner.visibilityState !== 'visible') { stop('focus_lost'); return false; }
  return !terminal;
};
const match = element => {
  const matches = element.matches(options.selector);
  if (matches) {
    if (!candidates.has(element) && candidates.size >= 512) throw Error('click watch exceeds 512 target candidates');
    candidates.add(element);
    if (resize) resize.observe(element);
  } else if (candidates.delete(element) && resize) resize.unobserve(element);
};
const walk = (element, budget) => {
  if (!(element instanceof Element)) return;
  if (--budget.left < 0) throw Error('click watch exceeds 4096 changed elements');
  match(element);
  const walker = owner.createTreeWalker(element, NodeFilter.SHOW_ELEMENT);
  let child;
  while ((child = walker.nextNode())) {
    if (--budget.left < 0) throw Error('click watch exceeds 4096 changed elements');
    match(child);
  }
};
const visible = element => {
  if (!element.isConnected || (element !== root && !root.contains(element))) return null;
  const rect = element.getBoundingClientRect();
  if (!(rect.width > 0 && rect.height > 0)) return null;
  for (let ancestor = element, depth = 0; ancestor; ancestor = ancestor.parentElement) {
    if (++depth > 64) throw Error('click target exceeds 64 ancestor levels');
    const style = getComputedStyle(ancestor);
    if (ancestor.hidden || ancestor.getAttribute('aria-hidden') === 'true' || style.display === 'none' ||
        style.visibility === 'hidden' || style.visibility === 'collapse' || Number(style.opacity) === 0) return null;
  }
  const left = Math.max(0, rect.x), top = Math.max(0, rect.y);
  const right = Math.min(innerWidth, rect.right), bottom = Math.min(innerHeight, rect.bottom);
  if (right <= left || bottom <= top) return null;
  return {bounds: {x: rect.x, y: rect.y, width: rect.width, height: rect.height},
    x: (left + right) / 2, y: (top + bottom) / 2};
};
const refresh = () => {
  if (closed || !context()) return;
  let selected = null, point = null;
  for (const element of candidates) {
    if (!element.isConnected || (element !== root && !root.contains(element))) {
      candidates.delete(element); resize?.unobserve(element); continue;
    }
    const geometry = visible(element);
    if (!geometry) continue;
    if (selected) { stop('ambiguous_visible_targets'); return; }
    selected = element; point = geometry;
  }
  if (!selected) {
    if (present) emit({type: 'gone', episode});
    present = false; candidate = null;
    return;
  }
  if (!present) { episode++; present = true; claimed = false; appeared = performance.now(); }
  if (claimed) return;
  const element = identity(selected);
  if (candidate && candidate.element === element &&
      JSON.stringify(candidate.bounds) === JSON.stringify(point.bounds) &&
      candidate.x === point.x && candidate.y === point.y) return;
  candidate = {type: 'candidate', episode, revision: ++revision, element, ...point,
    detected_at_ms: appeared};
  emit(candidate);
};
const process = records => {
  if (closed || terminal) return;
  try {
    const budget = {left: 4096};
    for (const record of records) {
      if (record.type === 'attributes') match(record.target);
      for (const node of record.addedNodes) walk(node, budget);
    }
    refresh();
  } catch (error) { stop(String(error.message || error)); }
};
const geometry = () => {
  try { refresh(); } catch (error) { stop(String(error.message || error)); }
};
const claim = expected => {
  const records = mutation.takeRecords();
  if (records.length) process(records);
  if (!context()) return {status: 'blocked', reason: terminal};
  if (!candidate || claimed || candidate.episode !== expected.episode || candidate.revision !== expected.revision) {
    return {status: 'stale'};
  }
  let element = null;
  for (const current of candidates) if (identity(current) === candidate.element) { element = current; break; }
  if (!element || !element.matches(options.selector)) return {status: 'stale'};
  const point = visible(element);
  if (!point) return {status: 'stale'};
  if (JSON.stringify(point.bounds) !== JSON.stringify(candidate.bounds) || point.x !== candidate.x || point.y !== candidate.y) {
    stop('target_moved_before_dispatch'); return {status: 'blocked', reason: terminal};
  }
  if (element.matches(':disabled') || element.getAttribute('aria-disabled') === 'true') {
    stop('target_disabled'); return {status: 'blocked', reason: terminal};
  }
  const hit = owner.elementFromPoint(candidate.x, candidate.y);
  if (!hit || (hit !== element && !element.contains(hit))) {
    stop('target_occluded'); return {status: 'blocked', reason: terminal};
  }
  claimed = true;
  const verified = {...candidate, status: 'ready', verified_at_ms: performance.now()};
  pending.set(episode, {element, candidate: verified});
  return verified;
};
const click = event => {
  if (!event.isTrusted) return;
  for (const [number, pendingClick] of pending) {
    if (event.target !== pendingClick.element && !pendingClick.element.contains(event.target)) continue;
    const now = performance.now();
    const receipt = {type: 'received', episode: number, element: pendingClick.candidate.element,
      received_at_ms: now, detected_to_click_ms: now - pendingClick.candidate.detected_at_ms,
      trusted: true};
    received.push(receipt); pending.delete(number); emit(receipt);
    break;
  }
};
const lostFocus = () => stop('focus_lost');
const visibility = () => { if (owner.visibilityState !== 'visible') lostFocus(); };
const pagehide = () => stop('document_replaced');
const snapshot = () => ({backend: 'dom-action', scope: options.scope, selector: options.selector,
  url, origin, connected: root.isConnected, episode, present, candidate, terminal, received});
const close = () => {
  closed = true; mutation.disconnect(); parents.disconnect(); resize.disconnect();
  ownerWindow.removeEventListener('scroll', geometry, true);
  ownerWindow.removeEventListener('resize', geometry);
  ownerWindow.removeEventListener('blur', lostFocus);
  ownerWindow.removeEventListener('pagehide', pagehide);
  owner.removeEventListener('visibilitychange', visibility);
  root.removeEventListener('transitionend', geometry);
  root.removeEventListener('animationend', geometry);
  root.removeEventListener('click', click, true);
};
walk(root, {left: 4096});
mutation = new MutationObserver(process);
mutation.observe(root, {subtree: true, childList: true, attributes: true});
parents = new MutationObserver(geometry);
for (let ancestor = root.parentElement; ancestor; ancestor = ancestor.parentElement) {
  parents.observe(ancestor, {childList: true, attributes: true});
}
resize = new ResizeObserver(geometry);
for (const element of candidates) resize.observe(element);
ownerWindow.addEventListener('scroll', geometry, true);
ownerWindow.addEventListener('resize', geometry);
ownerWindow.addEventListener('blur', lostFocus);
ownerWindow.addEventListener('pagehide', pagehide);
owner.addEventListener('visibilitychange', visibility);
root.addEventListener('transitionend', geometry);
root.addEventListener('animationend', geometry);
root.addEventListener('click', click, true);
watches.set(id, {snapshot, close, claim});
geometry();
return snapshot();
