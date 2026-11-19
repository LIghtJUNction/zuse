use super::super::{Result, image_metadata, quote, split_chord, validate_text};
use std::{ffi::{c_char,c_int,c_uint,c_ulong,c_void,CStr,CString}, path::Path, ptr, sync::Once};
type Dpy = *mut c_void;
type Window = c_ulong;
type Atom = c_ulong;
unsafe extern "C" {
    fn dlopen(name:*const c_char, flags:c_int)->*mut c_void;
    fn dlsym(handle:*mut c_void,name:*const c_char)->*mut c_void;
    fn dlclose(handle:*mut c_void)->c_int;
}
struct Library(*mut c_void);
impl Library {
    fn open(name:&CStr)->Result<Self> {
        // SAFETY: name is NUL-terminated and flags are RTLD_NOW | RTLD_LOCAL.
        let p=unsafe{dlopen(name.as_ptr(),2)};
        if p.is_null(){Err(format!("native library {} is unavailable",name.to_string_lossy()))} else {Ok(Self(p))}
    }
    unsafe fn symbol<T:Copy>(&self,name:&CStr)->Result<T> {
        // SAFETY: caller declares the exact documented symbol signature.
        let p=unsafe{dlsym(self.0,name.as_ptr())};
        if p.is_null(){Err(format!("missing native symbol {}",name.to_string_lossy()))}
        else {Ok(unsafe{std::mem::transmute_copy(&p)})}
    }
}
impl Drop for Library { fn drop(&mut self) {unsafe{dlclose(self.0);}} }
macro_rules! api {
    ($($name:ident($($arg:ty),*)->$ret:ty;)+) => {
        #[allow(non_snake_case)]
        struct Api { $($name:unsafe extern "C" fn($($arg),*)->$ret,)+ }
        impl Api { unsafe fn load(lib:&Library)->Result<Self> {Ok(Self{
            $($name:unsafe{lib.symbol(CStr::from_bytes_with_nul_unchecked(concat!(stringify!($name),"\0").as_bytes()))?},)+
        })} }
    }
}
api! {
    XOpenDisplay(*const c_char)->Dpy;
    XCloseDisplay(Dpy)->c_int;
    XDefaultRootWindow(Dpy)->Window;
    XDefaultScreen(Dpy)->c_int;
    XQueryPointer(Dpy,Window,*mut Window,*mut Window,*mut c_int,*mut c_int,*mut c_int,*mut c_int,*mut c_uint)->c_int;
    XFlush(Dpy)->c_int;
    XSync(Dpy,c_int)->c_int;
    XInternAtom(Dpy,*const c_char,c_int)->Atom;
    XGetWindowProperty(Dpy,Window,Atom,std::ffi::c_long,std::ffi::c_long,c_int,Atom,*mut Atom,*mut c_int,*mut c_ulong,*mut c_ulong,*mut *mut u8)->c_int;
    XQueryTree(Dpy,Window,*mut Window,*mut Window,*mut *mut Window,*mut c_uint)->c_int;
    XGetGeometry(Dpy,Window,*mut Window,*mut c_int,*mut c_int,*mut c_uint,*mut c_uint,*mut c_uint,*mut c_uint)->c_int;
    XTranslateCoordinates(Dpy,Window,Window,c_int,c_int,*mut c_int,*mut c_int,*mut Window)->c_int;
    XFetchName(Dpy,Window,*mut *mut c_char)->c_int;
    XGetClassHint(Dpy,Window,*mut ClassHint)->c_int;
    XGetInputFocus(Dpy,*mut Window,*mut c_int)->c_int;
    XSetInputFocus(Dpy,Window,c_int,c_ulong)->c_int;
    XRaiseWindow(Dpy,Window)->c_int;
    XGetImage(Dpy,Window,c_int,c_int,c_uint,c_uint,c_ulong,c_int)->*mut XImage;
    XDestroyImage(*mut XImage)->c_int;
    XGetPixel(*mut XImage,c_int,c_int)->c_ulong;
    XStringToKeysym(*const c_char)->c_ulong;
    XKeysymToKeycode(Dpy,c_ulong)->u8;
    XDisplayKeycodes(Dpy,*mut c_int,*mut c_int)->c_int;
    XGetKeyboardMapping(Dpy,u8,c_int,*mut c_int)->*mut c_ulong;
    XChangeKeyboardMapping(Dpy,c_int,c_int,*const c_ulong,c_int)->c_int;
    XFree(*mut c_void)->c_int;
    XSendEvent(Dpy,Window,c_int,std::ffi::c_long,*mut c_void)->c_int;
    XSetErrorHandler(Option<unsafe extern "C" fn(Dpy,*mut c_void)->c_int>)->Option<unsafe extern "C" fn(Dpy,*mut c_void)->c_int>;
}
#[repr(C)]
struct ClassHint {name:*mut c_char,class:*mut c_char}
#[repr(C)]
struct XImage {
    width:c_int,height:c_int,xoffset:c_int,format:c_int,data:*mut c_char,
    byte_order:c_int,bitmap_unit:c_int,bitmap_bit_order:c_int,bitmap_pad:c_int,
    depth:c_int,bytes_per_line:c_int,bits_per_pixel:c_int,
    red_mask:c_ulong,green_mask:c_ulong,blue_mask:c_ulong,
}
#[repr(C)]
struct KeyEvent {
    kind:c_int,serial:c_ulong,send_event:c_int,display:Dpy,window:Window,root:Window,subwindow:Window,
    time:c_ulong,x:c_int,y:c_int,x_root:c_int,y_root:c_int,state:c_uint,keycode:c_uint,same_screen:c_int,
}
struct XTest {
    _lib:Library,
    key:unsafe extern "C" fn(Dpy,c_uint,c_int,c_ulong)->c_int,
    button:unsafe extern "C" fn(Dpy,c_uint,c_int,c_ulong)->c_int,
    motion:unsafe extern "C" fn(Dpy,c_int,c_int,c_int,c_ulong)->c_int,
}
impl XTest {
    fn load()->Result<Self> {
        let lib=Library::open(c"libXtst.so.6")?;
        // SAFETY: function pointer types match XTEST 2.2.
        unsafe { Ok(Self { key:lib.symbol(c"XTestFakeKeyEvent")?,button:lib.symbol(c"XTestFakeButtonEvent")?,
            motion:lib.symbol(c"XTestFakeMotionEvent")?,_lib:lib }) }
    }
}
pub(super) struct X11 { api:Api, display:Dpy, root:Window, test:Option<XTest>, _lib:Library }
impl Drop for X11 {fn drop(&mut self){unsafe{(self.api.XCloseDisplay)(self.display);}}}
unsafe extern "C" fn ignore_error(_:Dpy,_:*mut c_void)->c_int{0}
impl X11 {
    pub fn connect()->Result<Self>{
        let lib=Library::open(c"libX11.so.6")?;
        // SAFETY: dynamically loaded Xlib functions retain their library owner.
        unsafe {
            let api=Api::load(&lib)?;
            static ERROR_HANDLER:Once=Once::new();
            ERROR_HANDLER.call_once(||{(api.XSetErrorHandler)(Some(ignore_error));});
            let display=(api.XOpenDisplay)(ptr::null());
            if display.is_null(){return Err("XOpenDisplay failed; set DISPLAY to an accessible X11 display".into());}
            let root=(api.XDefaultRootWindow)(display);
            Ok(Self{api,display,root,test:XTest::load().ok(),_lib:lib})
        }
    }
    pub fn has_input(&self)->bool{self.test.is_some()}
    pub fn position(&self)->Result<(i32,i32)>{
        let(mut root,mut child,mut x,mut y,mut wx,mut wy,mut state)=(0,0,0,0,0,0,0);
        unsafe {
            if (self.api.XQueryPointer)(self.display,self.root,&mut root,&mut child,&mut x,&mut y,&mut wx,&mut wy,&mut state)==0{
                return Err("pointer is on a different X11 screen".into());
            }
        } Ok((x,y))
    }
    fn flush(&self){unsafe{(self.api.XFlush)(self.display);}}
    pub fn move_to(&mut self,x:i32,y:i32)->Result<()>{
        let test=self.test.as_ref().ok_or("XTest extension library unavailable")?;
        unsafe{(test.motion)(self.display,(self.api.XDefaultScreen)(self.display),x,y,0);}
        self.flush();Ok(())
    }
    pub fn button(&mut self,button:&str,action:&str)->Result<()>{
        let code=match button{"left"=>1,"middle"=>2,"right"=>3,"back"=>8,"forward"=>9,_=>return Err("button must be left, middle, right, back or forward".into())};
        let test=self.test.as_ref().ok_or("XTest extension library unavailable")?;
        unsafe{match action{
            "down"=>{(test.button)(self.display,code,1,0);},
            "up"=>{(test.button)(self.display,code,0,0);},
            "click"=>{(test.button)(self.display,code,1,0);(test.button)(self.display,code,0,0);},
            _=>return Err("button action must be down, up or click".into()),
        }}
        self.flush();Ok(())
    }
    pub fn scroll(&mut self,dx:i32,dy:i32)->Result<()>{
        if dx.unsigned_abs()>1000||dy.unsigned_abs()>1000{return Err("scroll is limited to 1000 notches per axis".into());}
        let test=self.test.as_ref().ok_or("XTest extension library unavailable")?;
        for (amount,positive,negative) in [(dy,5,4),(dx,7,6)]{
            let button=if amount<0{negative}else{positive};
            for _ in 0..amount.unsigned_abs(){unsafe{(test.button)(self.display,button,1,0);(test.button)(self.display,button,0,0);}}
        }self.flush();Ok(())
    }
    fn keys(&self,chord:&str)->Result<Vec<u8>>{
        let mut result=Vec::new();
        for name in split_chord(chord)?{
            let normalized=match name.to_ascii_lowercase().as_str(){
                "ctrl"|"control"=>"Control_L","alt"=>"Alt_L","shift"=>"Shift_L","super"|"win"|"meta"|"cmd"=>"Super_L",
                "enter"|"return"=>"Return","escape"|"esc"=>"Escape","backspace"=>"BackSpace","tab"=>"Tab","space"=>"space",
                "delete"|"del"=>"Delete","insert"=>"Insert","home"=>"Home","end"=>"End","pageup"=>"Page_Up","pagedown"=>"Page_Down",
                "left"=>"Left","right"=>"Right","up"=>"Up","down"=>"Down",_=>name};
            let c=CString::new(normalized).map_err(|_|"NUL in key")?;
            let sym=unsafe{(self.api.XStringToKeysym)(c.as_ptr())};
            if sym==0{return Err(format!("unknown X11 keysym {name:?}"));}
            let code=unsafe{(self.api.XKeysymToKeycode)(self.display,sym)};
            if code==0{return Err(format!("key {name:?} is absent from the X11 keyboard map"));}
            result.push(code);
        }Ok(result)
    }
    pub fn key(&mut self,chord:&str)->Result<()>{
        let keys=self.keys(chord)?;
        let test=self.test.as_ref().ok_or("XTest extension library unavailable")?;
        unsafe{
            for &code in &keys{(test.key)(self.display,u32::from(code),1,0);}
            for &code in keys.iter().rev(){(test.key)(self.display,u32::from(code),0,0);}
        }self.flush();Ok(())
    }
    pub fn type_text(&mut self,text:&str)->Result<()>{self.type_common(None,text)}
    pub fn type_into(&mut self,window:&str,text:&str)->Result<()>{let id=self.window_id(window)?;self.type_common(Some(id),text)}
    fn type_common(&mut self,window:Option<Window>,text:&str)->Result<()>{
        validate_text(text)?;
        if text.is_empty(){return Ok(());}
        if window.is_none()&&self.test.is_none(){return Err("XTest extension library unavailable".into());}
        let(mut min,mut max,mut per)=(0,0,0);
        // Read the active keyboard map once; existing symbols never change the map.
        let mapping=unsafe{
            (self.api.XDisplayKeycodes)(self.display,&mut min,&mut max);
            if min<8||max>255||min>max{return Err("invalid X11 keyboard-map range".into());}
            let saved=(self.api.XGetKeyboardMapping)(self.display,min as u8,max-min+1,&mut per);
            if saved.is_null()||per<=0||per>64{return Err("could not read X11 keyboard mapping".into());}
            let values=std::slice::from_raw_parts(saved,((max-min+1)*per) as usize).to_vec();
            (self.api.XFree)(saved.cast());values
        };
        let shift_code=unsafe{(self.api.XKeysymToKeycode)(self.display,0xffe1)};
        let emit=|code:u8,shift:bool|{
            unsafe{
                if let Some(w)=window{
                    self.send_key(w,code,true,u32::from(shift));
                    self.send_key(w,code,false,u32::from(shift));
                }else if let Some(test)=self.test.as_ref(){
                    if shift{(test.key)(self.display,u32::from(shift_code),1,0);}
                    (test.key)(self.display,u32::from(code),1,0);
                    (test.key)(self.display,u32::from(code),0,0);
                    if shift{(test.key)(self.display,u32::from(shift_code),0,0);}
                }
                (self.api.XSync)(self.display,0);
            }
        };
        for ch in text.chars(){
            let sym=match ch{'\n'|'\r'=>0xff0d,'\t'=>0xff09,'\u{8}'=>0xff08,c if (c as u32)<=255=>c as c_ulong,c=>0x01000000|c as c_ulong};
            let mut found=None;
            for (index,row) in mapping.chunks_exact(per as usize).enumerate(){
                if row[0]==sym{found=Some(((min as usize+index) as u8,false));break;}
                if per>1&&row[1]==sym{found=Some(((min as usize+index) as u8,true));break;}
            }
            if let Some((code,shift))=found{emit(code,shift);continue;}
            // Unicode symbols absent from the map use one temporary row. X11
            // clients receive MappingNotify and must refresh their XKB/Xlib map.
            let original=&mapping[((max-min)*per) as usize..];
            let row=vec![sym;per as usize];
            unsafe{(self.api.XChangeKeyboardMapping)(self.display,max,per,row.as_ptr(),1);(self.api.XSync)(self.display,0);}
            std::thread::sleep(std::time::Duration::from_millis(5));
            emit(max as u8,false);
            std::thread::sleep(std::time::Duration::from_millis(10));
            unsafe{(self.api.XChangeKeyboardMapping)(self.display,max,per,original.as_ptr(),1);(self.api.XSync)(self.display,0);}
        }
        Ok(())
    }
    unsafe fn send_key(&self,window:Window,code:u8,down:bool,state:u32){
        let mut event=[0 as c_ulong;24];
        let value=KeyEvent{kind:if down{2}else{3},serial:0,send_event:1,display:self.display,window,
            root:self.root,subwindow:0,time:0,x:1,y:1,x_root:1,y_root:1,state,keycode:u32::from(code),same_screen:1};
        unsafe{
            event.as_mut_ptr().cast::<KeyEvent>().write(value);
            (self.api.XSendEvent)(self.display,window,0,if down{1}else{2},event.as_mut_ptr().cast());
        }
    }
    pub fn key_into(&mut self,window:&str,chord:&str)->Result<()>{
        let w=self.window_id(window)?;let codes=self.keys(chord)?;
        let mut state=0u32;
        for (name,&code) in split_chord(chord)?.into_iter().zip(&codes){
            unsafe{self.send_key(w,code,true,state);}
            state|=match name.to_ascii_lowercase().as_str(){"shift"=>1,"ctrl"|"control"=>4,"alt"=>8,"super"|"meta"|"win"=>64,_=>0};
        }
        for &code in codes.iter().rev(){unsafe{self.send_key(w,code,false,state);}}
        self.flush();Ok(())
    }
    pub fn window_id(&self,id:&str)->Result<Window>{
        let w=if id=="active"{
            let(mut w,mut revert)=(0,0);unsafe{(self.api.XGetInputFocus)(self.display,&mut w,&mut revert);}w
        }else if let Some(hex)=id.strip_prefix("0x"){Window::from_str_radix(hex,16).map_err(|_|"invalid hexadecimal X11 window ID")?}
        else{id.parse().map_err(|_|"window ID must be decimal, 0x hexadecimal, or active")?};
        if w<=1{return Err("no active application window".into());}
        self.geometry(w)?;Ok(w)
    }
    fn geometry(&self,w:Window)->Result<(i32,i32,u32,u32)>{
        let(mut root,mut x,mut y,mut width,mut height,mut border,mut depth)=(0,0,0,0,0,0,0);
        let status=unsafe{(self.api.XGetGeometry)(self.display,w,&mut root,&mut x,&mut y,&mut width,&mut height,&mut border,&mut depth)};
        if status==0{return Err(format!("window 0x{w:x} does not exist"));}
        let mut child=0;
        unsafe{(self.api.XTranslateCoordinates)(self.display,w,self.root,0,0,&mut x,&mut y,&mut child);}
        Ok((x,y,width,height))
    }
    pub fn focus(&mut self,id:&str)->Result<()>{
        let w=self.window_id(id)?;
        unsafe{(self.api.XRaiseWindow)(self.display,w);(self.api.XSetInputFocus)(self.display,w,2,0);}
        self.flush();Ok(())
    }
    fn property(&self,w:Window,name:&CStr)->Option<(c_int,Vec<u8>)>{
        let atom=unsafe{(self.api.XInternAtom)(self.display,name.as_ptr(),1)};
        if atom==0{return None;}
        let(mut actual,mut format,mut count,mut after,mut data)=(0,0,0,0,ptr::null_mut());
        let status=unsafe{(self.api.XGetWindowProperty)(self.display,w,atom,0,16_384,0,0,&mut actual,&mut format,&mut count,&mut after,&mut data)};
        if status!=0||data.is_null(){return None;}
        let bytes=match format{8=>count as usize,16=>count as usize*2,32=>count as usize*std::mem::size_of::<c_ulong>(),_=>0};
        let out=unsafe{std::slice::from_raw_parts(data,bytes.min(1_048_576)).to_vec()};
        unsafe{(self.api.XFree)(data.cast());}Some((format,out))
    }
    pub fn windows(&self)->Result<String>{
        let mut wins=Vec::new();
        if let Some((32,bytes))=self.property(self.root,c"_NET_CLIENT_LIST"){
            for item in bytes.chunks_exact(std::mem::size_of::<c_ulong>()){
                let mut arr=[0;std::mem::size_of::<c_ulong>()];arr.copy_from_slice(item);wins.push(c_ulong::from_ne_bytes(arr));
            }
        }else{
            let(mut root,mut parent,mut children,mut count)=(0,0,ptr::null_mut(),0);
            unsafe{
                if (self.api.XQueryTree)(self.display,self.root,&mut root,&mut parent,&mut children,&mut count)==0{return Err("XQueryTree failed".into());}
                if !children.is_null(){wins.extend_from_slice(std::slice::from_raw_parts(children,count.min(4096) as usize));(self.api.XFree)(children.cast());}
            }
        }
        let mut result=String::from("[");
        for w in wins.into_iter().take(4096){
            let Ok((x,y,width,height))=self.geometry(w) else{continue};
            let title=if let Some((8,data))=self.property(w,c"_NET_WM_NAME"){String::from_utf8_lossy(&data).trim_end_matches('\0').to_owned()}
            else{let mut name=ptr::null_mut();unsafe{(self.api.XFetchName)(self.display,w,&mut name);if name.is_null(){String::new()}else{let s=CStr::from_ptr(name).to_string_lossy().into_owned();(self.api.XFree)(name.cast());s}}};
            let mut hint=ClassHint{name:ptr::null_mut(),class:ptr::null_mut()};
            let class=unsafe{
                (self.api.XGetClassHint)(self.display,w,&mut hint);
                let s=if hint.class.is_null(){String::new()}else{CStr::from_ptr(hint.class).to_string_lossy().into_owned()};
                if !hint.name.is_null(){(self.api.XFree)(hint.name.cast());}if !hint.class.is_null(){(self.api.XFree)(hint.class.cast());}s
            };
            let pid=self.property(w,c"_NET_WM_PID").and_then(|(f,b)|if f==32&&b.len()>=std::mem::size_of::<c_ulong>(){let mut a=[0;std::mem::size_of::<c_ulong>()];let n=a.len();a.copy_from_slice(&b[..n]);Some(c_ulong::from_ne_bytes(a))}else{None});
            if result.len()>1{result.push(',');}
            let wine=class.to_ascii_lowercase().contains("wine")||class.to_ascii_lowercase().ends_with(".exe");
            result.push_str(&format!("{{\"id\":\"0x{w:x}\",\"title\":{},\"class\":{},\"pid\":{},\"x\":{x},\"y\":{y},\"width\":{width},\"height\":{height},\"wine\":{wine}}}",quote(&title),quote(&class),pid.map(|p|p.to_string()).unwrap_or("null".into())));
        }result.push(']');Ok(result)
    }
    pub fn screenshot(&self,path:&Path,window:Option<&str>)->Result<String>{
        let w=window.map(|w|self.window_id(w)).transpose()?.unwrap_or(self.root);
        let(_,_,width,height)=self.geometry(w)?;
        if width==0||height==0||width>16_384||height>16_384||u64::from(width)*u64::from(height)>67_108_864{
            return Err("capture dimensions exceed limits".into());
        }
        let image=unsafe{(self.api.XGetImage)(self.display,w,0,0,width,height,c_ulong::MAX,2)};
        if image.is_null(){return Err("XGetImage failed; the window must be mapped and drawable".into());}
        let channel=|pixel:c_ulong,mask:c_ulong|->u8{
            if mask==0{0}else{let shift=mask.trailing_zeros();let value=(pixel&mask)>>shift;let max=mask>>shift;((value*255)/max) as u8}
        };
        let result=super::super::png::write_png(path,width,height,|x,y|unsafe{
            let pixel=(self.api.XGetPixel)(image,x as i32,y as i32);
            [channel(pixel,(*image).red_mask),channel(pixel,(*image).green_mask),channel(pixel,(*image).blue_mask)]
        });
        unsafe{(self.api.XDestroyImage)(image);}
        result?;Ok(image_metadata(path,width,height,"x11"))
    }
}
