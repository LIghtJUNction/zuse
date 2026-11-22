//! Bounded public 2D drawImage observation in the selected document.
//!
//! Records are rendering calls, not native controls or game entities. No pixels,
//! game variables, collision boxes, progress values, or input methods are read.
//! The owner token isolates cooperating Browser connections; it is not an
//! authentication boundary against the page that owns the JavaScript realm.
use super::*;
use crate::observation::Cancellation;

pub(crate) const MAX_TTL_MS: u64 = 15_000;
pub(crate) const MAX_BYTES: usize = 256 * 1024;
const TRANSPORT_BUDGET: Duration = Duration::from_secs(2);
const CONTROLLER_KEY: &str = "zuse.canvas.observer.v1";

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("canvas name must contain 1..64 ASCII letters, digits, '_' or '-'".into());
    }
    Ok(())
}

pub(crate) fn validate_watch(selector: &str, name: &str, ttl_ms: u64) -> Result<()> {
    validate_name(name)?;
    if selector.trim().is_empty() || selector.len() > 4096 || selector.contains('\0') {
        return Err("canvas selector must contain 1..4096 NUL-free bytes".into());
    }
    if !(1..=MAX_TTL_MS).contains(&ttl_ms) {
        return Err("canvas ttl_ms must be 1..15000".into());
    }
    Ok(())
}

fn state_expression(name: &str, owner: &str) -> String {
    format!(
        r#"(()=>{{const controller=globalThis[Symbol.for({})];if(!controller||controller.version!==1)throw Error("stale_target: canvas observer is unavailable in this document");return controller.state({},{});}})()"#,
        quote(CONTROLLER_KEY),
        quote(name),
        quote(owner)
    )
}

pub(crate) fn validate_condition(name: &str, condition: &str) -> Result<()> {
    validate_name(name)?;
    if condition.trim().is_empty() || condition.contains('\0') {
        return Err("canvas condition must be nonempty and NUL-free".into());
    }
    // Reserve the observer-owner envelope inside hold_until's 16 KiB limit.
    let maximum_owner = "x".repeat(128);
    if condition
        .len()
        .saturating_add(state_expression(name, &maximum_owner).len())
        .saturating_add(48)
        > 16384
    {
        return Err("canvas condition plus observer binding exceeds 16384 bytes".into());
    }
    Ok(())
}

impl Browser {
    /// Observe one unique, already-rendered canvas. No getContext call is made.
    /// One active observer is allowed per document. TTL closes the hook even if
    /// the host stops reading; explicit close releases its retained records too.
    pub fn canvas_watch(
        &mut self,
        selector: &str,
        name: &str,
        ttl_ms: u64,
        cancellation: &Cancellation,
    ) -> Result<String> {
        self.canvas_watch_with_history(selector, name, ttl_ms, true, cancellation)
    }

    /// With history=false, retain only source metadata and each source's latest
    /// draw. Event counts remain cumulative; this does not prove visibility.
    pub fn canvas_watch_with_history(
        &mut self,
        selector: &str,
        name: &str,
        ttl_ms: u64,
        history: bool,
        cancellation: &Cancellation,
    ) -> Result<String> {
        validate_watch(selector, name, ttl_ms)?;
        cancellation.check()?;
        let previously_owned = self.canvas_observer_owned;
        self.canvas_observer_owned = true;
        let id = format!(
            "canvas-{}-{}",
            std::process::id(),
            PROFILE_ID.fetch_add(1, Ordering::Relaxed)
        );
        let args = format!(
            r#"{{"op":"watch","selector":{},"name":{},"ttl_ms":{ttl_ms},"history":{history},"owner":{},"id":{},"state_expression":{}}}"#,
            quote(selector),
            quote(name),
            quote(&self.observation_binding),
            quote(&id),
            quote(&state_expression(name, &self.observation_binding))
        );
        let result = self.canvas_command(&args, Some(cancellation));
        if result.is_err() {
            // Evaluation may have installed the observer before its reply was
            // cancelled. Close only this attempted ID, never another owner.
            let cleanup = format!(
                r#"{{"op":"cleanup","owner":{},"id":{}}}"#,
                quote(&self.observation_binding),
                quote(&id)
            );
            if let Ok(receipt) = self.canvas_command(&cleanup, None) {
                self.canvas_observer_owned = with_json(&receipt, |root| {
                    Ok(
                        root.get("foreign_owner").and_then(Value::boolean) == Some(true)
                            && previously_owned,
                    )
                })
                .unwrap_or(true);
            }
        }
        result
    }

    /// Return bounded drawing records and the last draw per observed source.
    /// A source may be reused by several sprites; it is not an entity identity.
    pub fn canvas_read(&mut self, name: &str, cancellation: &Cancellation) -> Result<String> {
        validate_name(name)?;
        self.canvas_command(
            &format!(
                r#"{{"op":"read","name":{},"owner":{}}}"#,
                quote(name),
                quote(&self.observation_binding)
            ),
            Some(cancellation),
        )
    }

    /// Cleanup is permitted after cancellation and has its own short deadline.
    pub fn canvas_close(&mut self, name: &str, _cancellation: &Cancellation) -> Result<String> {
        validate_name(name)?;
        let result = self.canvas_command(
            &format!(
                r#"{{"op":"close","name":{},"owner":{}}}"#,
                quote(name),
                quote(&self.observation_binding)
            ),
            None,
        );
        if result.is_ok() {
            self.canvas_observer_owned = false;
        }
        result
    }

    /// Bind a synchronous hold-until condition to this Browser's observer.
    /// `state` contains only our own bounded public-draw records. The caller's
    /// expression remains ordinary JavaScript, not a security sandbox.
    pub fn canvas_condition(&self, name: &str, condition: &str) -> Result<String> {
        validate_condition(name, condition)?;
        let expression = format!(
            "(()=>{{const state={};return ({});}})()",
            state_expression(name, &self.observation_binding),
            condition
        );
        if expression.len() > 16_384 {
            return Err("canvas condition plus observer binding exceeds 16384 bytes".into());
        }
        Ok(expression)
    }

    /// Call before detaching a selected page and from Browser::close. It never
    /// closes another Browser connection's watch. TTL is the disconnect fallback.
    pub(crate) fn close_canvas_observer(&mut self) -> Result<()> {
        if self.closed || !self.canvas_observer_owned {
            return Ok(());
        }
        let result = self.canvas_command(
            &format!(
                r#"{{"op":"cleanup","owner":{}}}"#,
                quote(&self.observation_binding)
            ),
            None,
        )?;
        self.canvas_observer_owned = false;
        with_json(&result, |root| {
            if root.get("restore_conflict").and_then(Value::boolean) == Some(true) {
                return Err("canvas observer cleanup reported restore_conflict; the page's replacement was preserved".into());
            }
            Ok(())
        })
    }

    fn canvas_command(
        &mut self,
        args: &str,
        cancellation: Option<&Cancellation>,
    ) -> Result<String> {
        if self.closed {
            return Err("browser connection is closed".into());
        }
        if let Some(cancellation) = cancellation {
            cancellation.check()?;
        }
        let expression = format!("({OBSERVER})({args})");
        let deadline = Instant::now() + TRANSPORT_BUDGET;
        let result = self.request_until("Runtime.evaluate", &format!(r#"{{"expression":{},"returnByValue":true,"awaitPromise":false,"userGesture":false,"timeout":2000}}"#, quote(&expression)), true, deadline, cancellation)?;
        with_json(&result, |root| {
            if let Some(exception) = root.get("exceptionDetails") {
                return Err(format!(
                    "canvas observation JavaScript failed: {}",
                    exception.raw()
                ));
            }
            let value = root
                .get("result")
                .and_then(|result| result.get("value"))
                .ok_or("canvas observation returned no receipt")?;
            if value.get("ok").and_then(Value::boolean) == Some(false) {
                return Err(value
                    .get("error")
                    .and_then(Value::string)
                    .map(|text| text.chars().collect())
                    .unwrap_or_else(|| "canvas observation failed".into()));
            }
            if value.raw().len() > MAX_BYTES {
                return Err("canvas observation reply exceeded 256 KiB".into());
            }
            Ok(value.raw().to_owned())
        })
    }
}

// The program observes only arguments passed to public drawing APIs and native
// rendering properties. It does not enumerate application globals or contexts.
const OBSERVER: &str = r#"function(args) {
  'use strict';
  const KEY = Symbol.for('zuse.canvas.observer.v1');
  const MAX_EVENTS = 4096, MAX_SOURCES = 64, MAX_BYTES = 262144;
  const keepHistory = args.history !== false;
  const fail = message => { throw Error(message); };
  try {
    let previous = globalThis[KEY];
    if (previous && (previous.version !== 1 || typeof previous.info !== 'function' || typeof previous.close !== 'function'))
      fail('unsupported_state: canvas observer namespace is occupied');
    if (args.op !== 'watch') {
      if (!previous) {
        if (args.op === 'cleanup' || args.op === 'close') return {ok:true,status:'closed',closed:false,missing:true,restored:true,restore_conflict:false};
        fail('stale_target: canvas observer is unavailable in this document');
      }
      if (args.op === 'cleanup') {
        const info = previous.info();
        if (info.owner !== args.owner || (args.id && info.watch_id !== args.id))
          return {ok:true,status:'unchanged',foreign_owner:true,closed:false,restore_conflict:false};
        return previous.close(info.name,args.owner);
      }
      if (args.op === 'read') return previous.read(args.name,args.owner);
      if (args.op === 'close') return previous.close(args.name,args.owner);
      fail('unsupported_state: unknown canvas observation operation');
    }
    if (previous) {
      const info = previous.info();
      if (info.active) fail(info.owner === args.owner ? 'unsupported_state: a canvas watch is already active in this document' : 'stale_target: canvas watch belongs to another Browser connection');
      previous.retire();
      if (globalThis[KEY] !== previous) fail('stale_target: canvas observer namespace changed');
      delete globalThis[KEY];
      previous = null;
    }
    let selected;
    try { selected = document.querySelectorAll(args.selector); }
    catch (_) { fail('invalid canvas selector'); }
    if (selected.length !== 1) fail(selected.length ? 'ambiguous_target: canvas selector must match exactly one element' : 'stale_target: canvas selector matched no element');
    let canvas = selected[0];
    if (!(canvas instanceof HTMLCanvasElement) || !canvas.isConnected || canvas.ownerDocument !== document)
      fail('unsupported_state: selected element is not a connected canvas in this document');
    const proto = globalThis.CanvasRenderingContext2D && CanvasRenderingContext2D.prototype;
    const descriptor = proto && Object.getOwnPropertyDescriptor(proto,'drawImage');
    const get = (prototype,key) => prototype && Object.getOwnPropertyDescriptor(prototype,key)?.get;
    const canvasGetter = get(proto,'canvas');
    const alphaGetter = get(proto,'globalAlpha'), compositeGetter = get(proto,'globalCompositeOperation');
    const transform = proto && proto.getTransform;
    if (!descriptor || typeof descriptor.value !== 'function' || (!descriptor.configurable && !descriptor.writable) || !canvasGetter || !alphaGetter || !compositeGetter || typeof transform !== 'function')
      fail('unsupported_state: public 2D drawImage observation is unavailable');
    const original = descriptor.value, doc = document, width = canvas.width, height = canvas.height;
    if (!width || !height) fail('unsupported_state: selected canvas has an empty bitmap');
    const now = performance.now.bind(performance), started = now(), deadline = started + args.ttl_ms;
    const encode = new TextEncoder();
    const size = value => encode.encode(JSON.stringify(value)).length;
    const headerBudget=size({selector:args.selector,state_expression:args.state_expression,id:args.id,name:args.name})+4096;
    const sources = [], events = [], latest = new Map();
    let sourceIds = new WeakMap(), bytes = 0, latestBytes = 0, seq = 0, skipped = 0;
    let active = true, status = 'watching', reason = null, error = null, restored = false, conflict = false, timer = null;
    let stopped = null, bridge;
    const readers = [];
    const addReader = (kind,constructor,w,h,src,current) => {
      if (!constructor) return;
      const p=constructor.prototype, wg=get(p,w), hg=get(p,h), sg=src&&get(p,src), cg=current&&get(p,current);
      if (wg && hg) readers.push({kind,wg,hg,sg,cg});
    };
    addReader('image',globalThis.HTMLImageElement,'naturalWidth','naturalHeight','src','currentSrc');
    addReader('canvas',globalThis.HTMLCanvasElement,'width','height');
    addReader('image_bitmap',globalThis.ImageBitmap,'width','height');
    addReader('offscreen_canvas',globalThis.OffscreenCanvas,'width','height');
    addReader('video',globalThis.HTMLVideoElement,'videoWidth','videoHeight','src','currentSrc');
    const publicSource = image => {
      for (const reader of readers) {
        try {
          const w=Reflect.apply(reader.wg,image,[]),h=Reflect.apply(reader.hg,image,[]);
          let uri=reader.cg?Reflect.apply(reader.cg,image,[]):'';
          if (!uri && reader.sg) uri=Reflect.apply(reader.sg,image,[]);
          const data=typeof uri==='string'&&uri.startsWith('data:');
          return {kind:reader.kind,width:w,height:h,src:data?'data:[omitted]':String(uri||'').slice(0,2048),uri_redacted:data,uri_truncated:!data&&String(uri||'').length>2048};
        } catch (_) {}
      }
      fail('unsupported_state: drawImage source has no supported public dimensions');
    };
    const snapshot = (history) => {
      const at=stopped===null?now():stopped;
      const result={ok:true,schema:'zuse.canvas.v1',watch_id:args.id,name:args.name,status,active,stop_reason:reason,error,available:latest.size>0,unavailable_reason:latest.size?null:'no_matching_2d_drawImage_observed',coverage:'intercepted_2d_drawImage',visibility:'unverified',coordinate_space:'canvas_bitmap',canvas:{selector:args.selector,width,height},elapsed_ms:Math.max(0,Math.round(at-started)),event_count:seq,retained_event_count:events.length,history_recorded:keepHistory,source_count:sources.length,skipped_calls:skipped,truncated:status==='limit',restored,restore_conflict:conflict,sources:sources.map(source=>({...source})),latest:Array.from(latest.values(),entry=>({...entry.event,args:entry.event.args.slice(),matrix:entry.event.matrix.slice(),source_rect:entry.event.source_rect.slice(),destination_rect:entry.event.destination_rect.slice(),quad:entry.event.quad.map(point=>point.slice()),center:entry.event.center.slice(),source:{...sources[entry.event.source_id]},age_ms:Math.max(0,Math.round(at-started-entry.event.at_ms))})),state_expression:args.state_expression};
      if(history) result.events=events.map(event=>({...event,args:event.args.slice(),matrix:event.matrix.slice(),source_rect:event.source_rect.slice(),destination_rect:event.destination_rect.slice(),quad:event.quad.map(point=>point.slice()),center:event.center.slice()}));
      return result;
    };
    const stop = (next,why) => {
      if(!active) return;
      active=false;status=next;reason=why;stopped=now();
      if(timer!==null) { clearTimeout(timer);timer=null; }
      const current=Object.getOwnPropertyDescriptor(proto,'drawImage');
      if(current && current.value===wrapped) {
        try { Object.defineProperty(proto,'drawImage',descriptor);restored=true; }
        catch (_) { conflict=true; }
      } else conflict=true;
      try { if(canvas) canvas.removeEventListener('contextlost',lost); } catch (_) {}
      try { globalThis.removeEventListener('pagehide',pagehide); } catch (_) {}
      canvas=null;
    };
    const guard = () => {
      if(!active) return false;
      if(now()>=deadline) { stop('expired','ttl');return false; }
      if(document!==doc || !canvas || canvas.ownerDocument!==doc || !canvas.isConnected) { stop('stale','document_or_canvas_changed');return false; }
      if(canvas.width!==width || canvas.height!==height) { stop('stale','canvas_resized');return false; }
      if(Object.getOwnPropertyDescriptor(proto,'drawImage')?.value!==wrapped) { stop('stale','restore_conflict');return false; }
      return true;
    };
    const authorized = (name,owner) => {
      if(name!==args.name || owner!==args.owner) fail('stale_target: canvas observer name or owner does not match');
      if(document!==doc) fail('stale_target: canvas observer belongs to another document');
    };
    const lost=()=>stop('stale','context_lost'),pagehide=()=>stop('stale','page_hidden');
    const record = (image,raw,metadata,matrix,alpha,composite) => {
      if(!active) return;
      if(keepHistory&&events.length>=MAX_EVENTS) { stop('limit','events');return; }
      let source_id=sourceIds.get(image),source=source_id===undefined?null:sources[source_id];
      if(source && (source.src!==metadata.src || source.width!==metadata.width || source.height!==metadata.height)) { stop('stale','source_changed');return; }
      if(source_id===undefined && sources.length>=MAX_SOURCES) { stop('limit','sources');return; }
      if(source_id===undefined) { source_id=sources.length;source={id:source_id,...metadata}; }
      let sx=0,sy=0,sw=metadata.width,sh=metadata.height,dx,dy,dw,dh;
      if(raw.length===2) [dx,dy]=raw,dw=sw,dh=sh;
      else if(raw.length===4) [dx,dy,dw,dh]=raw;
      else [sx,sy,sw,sh,dx,dy,dw,dh]=raw;
      if(!sw||!sh||!dw||!dh||!metadata.width||!metadata.height) { skipped++;return; }
      const [a,b,c,d,e,f]=matrix,point=(x,y)=>[a*x+c*y+e,b*x+d*y+f];
      const event={seq:seq+1,at_ms:Math.max(0,Math.round(now()-started)),source_id,args:raw,matrix,source_rect:[sx,sy,sw,sh],destination_rect:[dx,dy,dw,dh],quad:[point(dx,dy),point(dx+dw,dy),point(dx+dw,dy+dh),point(dx,dy+dh)],center:point(dx+dw/2,dy+dh/2),axis_rotation:Math.atan2(b,a),alpha,composite};
      if(event.quad.some(point=>point.some(value=>!Number.isFinite(value)))||event.center.some(value=>!Number.isFinite(value)))
        fail('unsupported_state: transformed drawImage geometry is not finite');
      const eventSize=keepHistory?size(event)+1:0,sourceSize=sourceIds.has(image)?0:size(source)+1;
      const previousSize=latest.get(source_id)?.size||0,newLatestSize=size({...event,source,age_ms:15000})+1;
      if(headerBudget+bytes+eventSize+sourceSize+latestBytes-previousSize+newLatestSize>MAX_BYTES) { stop('limit','bytes');return; }
      if(!sourceIds.has(image)) { sourceIds.set(image,source_id);sources.push(source); }
      seq++;if(keepHistory)events.push(event);bytes+=eventSize+sourceSize;latestBytes+=newLatestSize-previousSize;latest.set(source_id,{event,size:newLatestSize});
    };
    function wrapped(...all) {
      if(active&&now()>=deadline) stop('expired','ttl');
      let target=false;
      try { target=active&&Reflect.apply(canvasGetter,this,[])===canvas; } catch (_) {}
      if(!target) return Reflect.apply(original,this,all);
      let captured=null,problem=null;
      try {
        if(guard()) {
          const raw=all.slice(1);
          if(![2,4,8].includes(raw.length)||raw.some(value=>typeof value!=='number'||!Number.isFinite(value)))
            fail('unsupported_state: drawImage observation requires finite primitive numeric arguments');
          const m=Reflect.apply(transform,this,[]),matrix=[m.a,m.b,m.c,m.d,m.e,m.f];
          if(matrix.some(value=>!Number.isFinite(value))) fail('unsupported_state: drawImage transform is not finite');
          captured={image:all[0],raw,metadata:publicSource(all[0]),matrix,alpha:Reflect.apply(alphaGetter,this,[]),composite:Reflect.apply(compositeGetter,this,[])};
        }
      } catch (e) { problem=String(e&&e.message||e).slice(0,512); }
      let result;
      try { result=Reflect.apply(original,this,all); }
      catch (e) { skipped++;throw e; }
      try {
        if(active&&!guard()) return result;
        if(active&&problem) { error=problem;stop('unsupported','observation_unavailable'); }
        else if(active&&captured) record(captured.image,captured.raw,captured.metadata,captured.matrix,captured.alpha,captured.composite);
      } catch (e) { error=String(e&&e.message||e).slice(0,512);stop('unsupported','observation_failed'); }
      return result;
    }
    bridge=Object.freeze({
      version:1,
      info(){guard();return {owner:args.owner,name:args.name,watch_id:args.id,active,status};},
      state(name,owner){authorized(name,owner);if(!guard()) fail('stale_target: canvas observation is '+status+' ('+reason+')');return snapshot(false);},
      read(name,owner){authorized(name,owner);guard();return snapshot(true);},
      close(name,owner){authorized(name,owner);stop('closed','explicit_close');const result={ok:true,status:conflict?'restore_conflict':'closed',closed:true,restored,restore_conflict:conflict,watch_id:args.id,event_count:seq,retained_event_count:events.length,history_recorded:keepHistory,source_count:sources.length};events.length=0;sources.length=0;latest.clear();sourceIds=new WeakMap();if(globalThis[KEY]===bridge)delete globalThis[KEY];return result;},
      retire(){if(active)fail('stale_target: cannot retire an active canvas watch');events.length=0;sources.length=0;latest.clear();sourceIds=new WeakMap();}
    });
    try {
      Object.defineProperty(proto,'drawImage',{...descriptor,value:wrapped});
      Object.defineProperty(globalThis,KEY,{value:bridge,configurable:true,writable:false,enumerable:false});
      canvas.addEventListener('contextlost',lost);
      globalThis.addEventListener('pagehide',pagehide);
      timer=setTimeout(()=>stop('expired','ttl'),args.ttl_ms);
    } catch (e) { stop('unsupported','installation_failed');throw e; }
    return snapshot(false);
  } catch (e) { return {ok:false,error:String(e&&e.message||e).slice(0,1024)}; }
}"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canvas_observation_arguments_are_bounded_before_page_access() {
        for name in ["", "has space", "../canvas", "canvas\0"] {
            assert!(validate_name(name).is_err());
        }
        assert!(validate_name(&"x".repeat(64)).is_ok());
        assert!(validate_name(&"x".repeat(65)).is_err());
        assert!(validate_watch("#park-canvas", "parking", 15_000).is_ok());
        for ttl in [0, 15_001] {
            assert!(validate_watch("canvas", "parking", ttl).is_err());
        }
        for selector in ["", "  ", "canvas\0"] {
            assert!(validate_watch(selector, "parking", 1000).is_err());
        }
        assert!(validate_watch(&"x".repeat(4097), "parking", 1000).is_err());
        assert!(validate_condition("parking", "state.latest.length > 0").is_ok());
        for condition in ["", "  ", "true\0"] {
            assert!(validate_condition("parking", condition).is_err());
        }
        assert!(validate_condition("parking", &"x".repeat(16384)).is_err());
    }
    #[test]
    fn canvas_state_expression_binds_the_browser_owner_as_quoted_data() {
        let expression = state_expression("parking", "owner\"\\\n");
        assert!(expression.contains("Symbol.for(\"zuse.canvas.observer.v1\")"));
        assert!(expression.contains(".state(\"parking\",\"owner\\\"\\\\\\n\")"));
    }
}
