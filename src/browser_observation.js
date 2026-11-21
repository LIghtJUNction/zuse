// Runs inside the already selected CDP page. No timers or screenshot polling.
const key = Symbol.for('zuse.observation.v1');
const watches = globalThis[key] ??= new Map();
if (action === 'snapshot') {
  const watch = watches.get(id);
  if (!watch) throw Error('observation scope is stale; create a new watch after navigation');
  return watch.snapshot();
}
if (action === 'close') {
  const watch = watches.get(id);
  if (watch) watch.close();
  watches.delete(id);
  return {closed: true};
}
const matches = document.querySelectorAll(selector);
if (matches.length !== 1) throw Error('observation selector must match exactly one container');
if (action === 'watch' && watches.has(id)) throw Error('observation watch already exists');
const root = matches[0];
const values = new Map();
const identities = new WeakMap();
let next = 1, revision = 0, truncated = false, closed = false;
let mutation, parents, resize;
const identity = element => {
  let value = identities.get(element);
  if (!value) { value = String(next++); identities.set(element, value); }
  return value;
};
const privateText = element => element.closest('input,textarea,[contenteditable]:not([contenteditable="false"])');
const text = element => {
  if (privateText(element)) return '';
  const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
  let value = '', node;
  while ((node = walker.nextNode()) && value.length < 4096) {
    const parent = node.parentElement;
    if (!parent || privateText(parent) || parent.closest('script,style,noscript')) continue;
    const style = getComputedStyle(parent);
    if (style.display === 'none' || style.visibility === 'hidden') continue;
    value += node.data;
  }
  return value.trim().slice(0, 4096);
};
const role = element => element.getAttribute('role') || ({
  BUTTON: 'button', A: element.hasAttribute('href') ? 'link' : 'generic',
  INPUT: element.type === 'checkbox' ? 'checkbox' : element.type === 'password' ? 'password' : 'textbox',
  TEXTAREA: 'textbox', SELECT: 'combobox', IMG: 'img', DIALOG: 'dialog',
  H1: 'heading', H2: 'heading', H3: 'heading', LI: 'listitem'
}[element.tagName] || 'generic');
const record = element => {
  const r = element.getBoundingClientRect(), style = getComputedStyle(element);
  const content = text(element);
  const name = (element.getAttribute('aria-label') || element.getAttribute('alt') ||
    element.getAttribute('title') || content).slice(0, 4096);
  return {id: identity(element), role: role(element), name, text: content,
    bounds: {x: r.x, y: r.y, width: r.width, height: r.height},
    visible: element.isConnected && r.width > 0 && r.height > 0 &&
      style.display !== 'none' && style.visibility !== 'hidden' && style.opacity !== '0' &&
      !element.closest('[hidden],[aria-hidden="true"]'),
    disabled: !!element.disabled || element.getAttribute('aria-disabled') === 'true'};
};
const relevant = element => element === root || element.hasAttribute('role') ||
  element.hasAttribute('aria-label') || /^(BUTTON|A|INPUT|TEXTAREA|SELECT|IMG|DIALOG|H[1-6])$/.test(element.tagName) ||
  (element.childElementCount === 0 && !!text(element));
const update = element => {
  if (!root.contains(element) && element !== root) return false;
  if (!relevant(element)) return values.delete(element);
  if (!values.has(element) && values.size >= 512) { truncated = true; return false; }
  const value = record(element), before = values.get(element);
  values.set(element, value);
  if (resize) resize.observe(element);
  return JSON.stringify(value) !== JSON.stringify(before);
};
const walk = element => {
  if (!(element instanceof Element)) return false;
  let changed = update(element);
  const walker = document.createTreeWalker(element, NodeFilter.SHOW_ELEMENT);
  let node, count = 0;
  while ((node = walker.nextNode()) && count++ < 4096) changed = update(node) || changed;
  if (node) truncated = true;
  return changed;
};
const emit = () => {
  revision++;
  if (action === 'watch' && !closed) {
    try { globalThis[binding](JSON.stringify({watch: id, revision})); } catch (_) {}
  }
};
const prune = () => {
  let changed = false;
  for (const element of values.keys()) {
    if (!element.isConnected || (element !== root && !root.contains(element))) {
      values.delete(element);
      if (resize) resize.unobserve(element);
      changed = true;
    }
  }
  return changed;
};
const geometry = () => {
  let changed = prune();
  for (const element of values.keys()) changed = update(element) || changed;
  if (changed) emit();
};
const snapshot = () => ({backend: 'dom', scope: selector, connected: root.isConnected,
  nodes: root.isConnected ? [...values.values()] : [], truncated, errors: 0});
walk(root);
if (action === 'observe') return snapshot();
mutation = new MutationObserver(records => {
  let changed = prune();
  const dirty = new Set();
  for (const change of records) {
    let element = change.target instanceof Element ? change.target : change.target.parentElement;
    for (let depth = 0; element && depth < 64; depth++, element = element.parentElement) {
      if (element !== root && !root.contains(element)) break;
      dirty.add(element);
      if (element === root) break;
    }
    if (change.type === 'attributes' && change.target instanceof Element) changed = walk(change.target) || changed;
    for (const added of change.addedNodes) if (added instanceof Element) changed = walk(added) || changed;
  }
  for (const element of dirty) changed = update(element) || changed;
  if (changed) emit();
});
mutation.observe(root, {subtree: true, childList: true, characterData: true, attributes: true});
parents = new MutationObserver(geometry);
for (let node = root.parentElement; node; node = node.parentElement) parents.observe(node, {childList: true, attributes: true});
resize = new ResizeObserver(entries => {
  let changed = false;
  for (const entry of entries) changed = update(entry.target) || changed;
  if (changed) emit();
});
for (const element of values.keys()) resize.observe(element);
window.addEventListener('scroll', geometry, true);
window.addEventListener('resize', geometry);
root.addEventListener('transitionend', geometry);
const close = () => {
  closed = true; mutation.disconnect(); parents.disconnect(); resize.disconnect();
  window.removeEventListener('scroll', geometry, true); window.removeEventListener('resize', geometry);
  root.removeEventListener('transitionend', geometry);
};
watches.set(id, {snapshot, close});
return snapshot();
