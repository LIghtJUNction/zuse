// A standalone Android page can use the embedded server. Other hosts supply their
// own transport when registering the component.
const origin = 'http://127.0.0.1:43497';
await customElements.whenDefined('zuse-touch');
const board = document.querySelector('zuse-touch');

function nonce() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, value => value.toString(16).padStart(2, '0')).join('');
}

function embeddedHint() {
  const hint = document.createElement('p');
  hint.textContent = '请从宿主页面独立打开工作台以执行手势';
  hint.style.cssText = 'text-align:center;color:#9ce3c4;font:14px system-ui;margin:12px';
  board.after(hint);
}

async function attach() {
  const response = await fetch('/api/session', {
    headers: {'X-Zuse-Workbench': '1'},
    cache: 'no-store'
  });
  if (!response.ok) return;
  const session = await response.json();
  if (!session.android) return;

  const instance = nonce();
  const headers = {'Content-Type': 'application/json', 'X-Zuse-Token': session.token};
  board.dataset.zuseInstance = instance;
  board.transport = async (request, {signal}) => {
    if (signal.aborted) throw Error('已取消');
    // Keep every native contact inside the visible canvas even if the submit
    // button scrolled into view on a narrow phone.
    board.shadowRoot.querySelector('#surface').scrollIntoView({block:'center',behavior:'instant'});
    const key = nonce();
    const call = (path, body) => fetch(path, {
      method: 'POST', headers, body: JSON.stringify({instance, key, ...body})
    });
    const cancel = () => { call('/api/cancel', {}).catch(() => {}); };
    signal.addEventListener('abort', cancel, {once:true});
    try {
      const reply = await call('/api/gesture', {request});
      const receipt = await reply.json();
      if (!reply.ok || !receipt.ok) throw Error(receipt.error || '触控请求失败');
      return receipt;
    } finally {
      signal.removeEventListener('abort', cancel);
    }
  };
}

if (board && location.origin === origin) {
  if (window.top === window) {
    await attach();
  } else {
    embeddedHint();
  }
}
