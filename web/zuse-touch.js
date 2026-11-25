const sheet = new URL('./zuse-touch.css', import.meta.url).href;
const clamp = (n, min, max) => Math.min(max, Math.max(min, n));
const contactColor = id => `hsl(${(id * 47 + 144) % 360} 55% 72%)`;
const markup = `
<main class="wrap">
  <header><h1>Zuse 触控</h1><p class="state" id="state">等待宿主连接</p></header>
  <div class="surface-label"><span>触控板</span><output id="frames">已观测 0 帧</output></div>
  <canvas id="surface" aria-label="手势轨迹编辑与真实触点显示。下方坐标也可编辑。"></canvas>
  <p class="hint">圆点为起点，方框为终点。拖动端点，或点击板面设置选中的端点。</p>
  <fieldset id="editor">
    <div class="grid">
      <label>手势<select id="preset"><option value="swipe">平移</option><option value="spread">张开</option><option value="pinch">捏合</option><option value="tap">轻点</option></select></label>
      <label>手指数<input id="count" type="number" min="1" max="10" step="1" value="2"></label>
      <label>持续时间（ms）<input id="duration" type="number" min="0" max="10000" step="1" value="420"></label>
      <label>速度曲线<select id="curve"><option value="linear">匀速</option><option value="ease-in-out">缓入缓出</option><option value="bezier">贝塞尔</option></select></label>
      <label>当前手指<select id="contact"></select></label>
      <label>编辑端点<select id="end"><option value="0">起点</option><option value="1">终点</option></select></label>
    </div>
    <details><summary>坐标与采样参数</summary><div class="grid">
      <label>X（%）<input id="x" type="number" min="0" max="100" step=".1"></label>
      <label>Y（%）<input id="y" type="number" min="0" max="100" step=".1"></label>
      <label>采样步数<input id="steps" type="number" min="1" max="120" step="1" value="30"></label>
      <label>扰动（px）<input id="jitter" type="number" min="0" max="32" step="1" value="0"></label>
      <label>随机种子<input id="seed" type="number" min="0" max="4294967295" step="1" value="1"></label>
    </div></details>
  </fieldset>
  <div class="actions"><button id="random" type="button">随机轨迹</button><button id="stop" type="button" hidden>停止</button><button id="run" class="primary" type="button" disabled>执行手势</button></div>
  <output id="receipt" class="receipt" role="status" aria-live="polite"></output>
</main>`;

// All coordinates use the touch surface's bounding rectangle, in the range 0..1.
export class ZuseTouch extends HTMLElement {
  #host = null;
  #operation = null;
  #points = [];
  #observed = new Map();
  #actualFrames = 0;
  #resize;
  #finger = null;
  #paint = 0;
  #passive = new Set();
  constructor() {
    super();
    const root = this.attachShadow({mode:'open'});
    const link = document.createElement('link');
    link.rel = 'stylesheet'; link.href = sheet;
    const template = document.createElement('template');
    template.innerHTML = markup;
    root.append(link, template.content.cloneNode(true));
    this.el = Object.fromEntries([...root.querySelectorAll('[id]')].map(node=>[node.id,node]));
    const e = this.el;
    e.preset.onchange = e.count.onchange = () => this.#preset();
    e.contact.onchange = e.end.onchange = () => this.#selection();
    e.x.onchange = e.y.onchange = () => {
      if (!e.x.value || !e.y.value || !e.x.checkValidity() || !e.y.checkValidity()) return;
      this.#setPoint(e.x.valueAsNumber / 100, e.y.valueAsNumber / 100);
    };
    e.random.onclick = () => this.#randomize();
    e.run.onclick = () => this.#run();
    e.stop.onclick = () => { if(this.#operation) { this.#operation.abort(); e.stop.disabled = true; e.receipt.textContent='正在请求停止…'; } };
    e.surface.onpointerdown = event => this.#down(event);
    e.surface.onpointermove = event => {
      if (event.pointerId === this.#finger && !this.#operation) {
        const p = this.#normalized(event.clientX, event.clientY); this.#setPoint(p.x,p.y);
      }
    };
    e.surface.onpointerup = e.surface.onpointercancel = e.surface.onlostpointercapture = event => {
      if(event.pointerId === this.#finger) this.#finger = null;
    };
    for (const kind of ['touchstart','touchmove','touchend','touchcancel']) {
      e.surface.addEventListener(kind,event=>{
        if (!event.isTrusted || !this.#operation) return;
        const contacts = [...event.touches].filter(t=>t.target===e.surface).map(t=>({id:t.identifier,...this.#normalized(t.clientX,t.clientY)}));
        this.observeFrame({contacts});
      }, {passive:true});
    }
    this.#resize = new ResizeObserver(()=>this.#redraw());
    this.#preset();
  }
  connectedCallback() { this.#resize.observe(this.el.surface); this.#status(); this.#redraw(); }
  disconnectedCallback() { this.#resize.disconnect(); this.#operation?.abort(); cancelAnimationFrame(this.#paint); this.#paint=0; }
  set transport(send) {
    if (send != null && typeof send !== 'function') throw new TypeError('transport must be a function');
    this.#host=send; this.#status();
  }
  get transport() { return this.#host; }
  surfaceBounds() { return this.el.surface.getBoundingClientRect(); }
  #status() {
    const busy=Boolean(this.#operation), e=this.el;
    e.state.textContent=busy?'执行中':this.#host?'宿主已连接':'等待宿主连接';
    e.run.disabled=!this.#host||busy; e.editor.disabled=busy; e.random.disabled=busy;
    e.stop.hidden=!busy; e.stop.disabled=Boolean(this.#operation?.signal.aborted);
  }
  #normalized(x,y) {
    const rect=this.surfaceBounds();
    return {x:clamp((x-rect.left)/rect.width,0,1), y:clamp((y-rect.top)/rect.height,0,1)};
  }
  #preset() {
    const e=this.el;
    if(!e.count.value || !e.count.checkValidity()) return;
    const n=e.count.valueAsNumber;
    this.#points=Array.from({length:n},(_,id)=>{
      const angle=(2*Math.PI*id/n)+Math.PI, c=Math.cos(angle), s=Math.sin(angle);
      if(e.preset.value==='spread'||e.preset.value==='pinch'){
        const [a,b]=e.preset.value==='spread'?[.14,.38]:[.38,.14];
        return {id,x0:.5+a*c,y0:.5+a*s,x1:.5+b*c,y1:.5+b*s};
      }
      const y=(id+1)/(n+1),tap=e.preset.value==='tap';
      return {id,x0:tap?.5:.20,y0:y,x1:tap?.5:.80,y1:y};
    });
    const selected=clamp(Number(e.contact.value)||0,0,n-1);
    e.contact.replaceChildren(...this.#points.map(p=>new Option(String(p.id+1),String(p.id))));
    e.contact.value=String(selected);
    this.#selection();
  }
  #selection() {
    const e=this.el, p=this.#points[Number(e.contact.value)], end=e.end.value;
    if(!p) return;
    e.x.value=(100*p['x'+end]).toFixed(1); e.y.value=(100*p['y'+end]).toFixed(1); this.#redraw();
  }
  #setPoint(x,y) {
    const e=this.el, p=this.#points[Number(e.contact.value)], end=e.end.value;
    p['x'+end]=x; p['y'+end]=y; this.#selection();
  }
  #down(event) {
    if(this.#operation || event.button!==0 || this.#finger!==null) return;
    const e=this.el, rect=this.surfaceBounds(), p=this.#normalized(event.clientX,event.clientY);
    let nearest=23;
    for(const path of this.#points) for(const end of ['0','1']){
      const d=Math.hypot((p.x-path['x'+end])*rect.width,(p.y-path['y'+end])*rect.height);
      if(d<nearest){nearest=d;e.contact.value=String(path.id);e.end.value=end;}
    }
    this.#finger=event.pointerId; e.surface.setPointerCapture(event.pointerId); this.#setPoint(p.x,p.y);
  }
  #randomize() {
    const e=this.el;
    if(!e.seed.reportValidity()||e.seed.value==='') return;
    let n=e.seed.valueAsNumber>>>0;
    const random=()=>{n=(Math.imul(n,1664525)+1013904223)>>>0;return .07+.86*n/4294967296;};
    for(const p of this.#points) Object.assign(p,{x0:random(),y0:random(),x1:random(),y1:random()});
    this.#selection();
  }
  observeFrame(frame) {
    if(!this.#operation||!Array.isArray(frame?.contacts)||frame.contacts.length>10) return;
    this.#actualFrames++; this.#passive.clear();
    for(const {id,x,y} of frame.contacts){
      if(!Number.isInteger(id)||!Number.isFinite(x)||!Number.isFinite(y)) continue;
      if(!this.#observed.has(id)) {
        if(this.#observed.size>=10) continue;
        this.#observed.set(id,[]);
      }
      const trace=this.#observed.get(id);
      if(trace.length>=256) trace.shift();
      trace.push({x:clamp(x,0,1),y:clamp(y,0,1)}); this.#passive.add(id);
    }
    this.el.frames.textContent='已观测 '+this.#actualFrames+' 帧'; this.#redraw();
  }
  async #run() {
    if(!this.#host||this.#operation) return;
    const e=this.el;
    for(const input of e.editor.querySelectorAll('input')){
      if(!input.value || !input.checkValidity()) {const details=input.closest('details');if(details) details.open=true;input.reportValidity();input.focus();return;}
    }
    const request={version:1, duration_ms:e.duration.valueAsNumber, steps:e.steps.valueAsNumber,
      curve:e.curve.value,seed:e.seed.valueAsNumber,jitter:e.jitter.valueAsNumber,
      contacts:this.#points.map(p=>({...p}))};
    const controller=new AbortController(); this.#operation=controller;
    this.#observed.clear();this.#passive.clear();this.#actualFrames=0;
    e.frames.textContent='已观测 0 帧';e.receipt.textContent='等待宿主回执…';this.#status();this.#redraw();
    try{
      const receipt=await this.#host(request,{signal:controller.signal,onFrame:frame=>{if(this.#operation===controller)this.observeFrame(frame);}});
      if(!receipt || receipt.ok!==true) throw Error(receipt?.error||'宿主未确认完成');
      const detail=[];
      if(Number.isFinite(receipt.elapsed_ms))detail.push(Number(receipt.elapsed_ms.toFixed(1))+' ms');
      if(Number.isSafeInteger(receipt.frames))detail.push(receipt.frames+' 帧');
      e.receipt.textContent='宿主确认完成'+(detail.length?' · '+detail.join(' · '):'');
    }catch(error){ e.receipt.textContent='执行结果 · '+String(error?.message||error).slice(0,500); }
    finally{this.#operation=null;this.#passive.clear();this.#status();this.#redraw();}
  }
  #redraw(){if(!this.isConnected||this.#paint) return;this.#paint=requestAnimationFrame(()=>{this.#paint=0;this.#draw();});}
  #draw() {
    const canvas=this.el.surface, rect=this.surfaceBounds(), w=rect.width,h=rect.height;
    if(!w||!h) return;
    const dpr=Math.min(devicePixelRatio||1,2);
    if(canvas.width!==Math.round(w*dpr)||canvas.height!==Math.round(h*dpr)){canvas.width=Math.round(w*dpr);canvas.height=Math.round(h*dpr);}
    const ctx=canvas.getContext('2d');if(!ctx)return;
    ctx.setTransform(dpr,0,0,dpr,0,0);ctx.clearRect(0,0,w,h);
    ctx.lineWidth=1;ctx.strokeStyle='#2b302d';
    for(let i=1;i<4;i++){ctx.beginPath();ctx.moveTo(i*w/4,0);ctx.lineTo(i*w/4,h);ctx.moveTo(0,i*h/4);ctx.lineTo(w,i*h/4);ctx.stroke();}
    for(const p of this.#points){
      const x0=p.x0*w,y0=p.y0*h,x1=p.x1*w,y1=p.y1*h,color=contactColor(p.id);
      ctx.strokeStyle=color;ctx.fillStyle=color;ctx.lineWidth=p.id===Number(this.el.contact.value)?1.75:1;
      ctx.setLineDash([4,5]);ctx.beginPath();ctx.moveTo(x0,y0);ctx.lineTo(x1,y1);ctx.stroke();ctx.setLineDash([]);
      ctx.beginPath();ctx.arc(x0,y0,5,0,Math.PI*2);ctx.fill();ctx.strokeRect(x1-5,y1-5,10,10);
      ctx.font='12px monospace';ctx.fillText(String(p.id+1),clamp(x0+8,0,w-17),clamp(y0-5,13,h-2));
    }
    for(const [id,trace] of this.#observed){
      if(!trace.length)continue;
      ctx.strokeStyle=contactColor(id);ctx.lineWidth=3;ctx.beginPath();ctx.moveTo(trace[0].x*w,trace[0].y*h);
      for(const p of trace.slice(1))ctx.lineTo(p.x*w,p.y*h);
      ctx.stroke();
      if(this.#passive.has(id)){const p=trace[trace.length-1];ctx.fillStyle=ctx.strokeStyle;ctx.beginPath();ctx.arc(p.x*w,p.y*h,6,0,Math.PI*2);ctx.fill();}
    }
  }
}
if(!customElements.get('zuse-touch')) customElements.define('zuse-touch',ZuseTouch);
