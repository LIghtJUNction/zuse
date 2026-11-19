//! Desktop accessibility adapters using the operating system's built-in helpers.
//! Windows: UIAutomationClient through Windows PowerShell.
//! macOS: System Events accessibility through JavaScript for Automation.
use super::Result;

#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(super) fn snapshot(nodes: usize, depth: usize) -> Result<String> {
    run("snapshot", "0", "", 0, nodes, depth)
}
pub(super) fn action(path: &str, index: usize) -> Result<String> {
    run("action", path, "", index, 1, 0)
}
pub(super) fn focus(path: &str) -> Result<String> {
    run("focus", path, "", 0, 1, 0)
}
pub(super) fn set_text(path: &str, text: &str) -> Result<String> {
    run("set_text", path, text, 0, 1, 0)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn run(
    operation: &str,
    path: &str,
    value: &str,
    index: usize,
    nodes: usize,
    depth: usize,
) -> Result<String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            WINDOWS,
        ]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("/usr/bin/osascript");
        command.args(["-l", "JavaScript", "-e", MACOS]);
        command
    };
    // User input is data in the child environment, never helper source code.
    command
        .env("ZUSE_AX_OPERATION", operation)
        .env("ZUSE_AX_PATH", path)
        .env("ZUSE_AX_VALUE", value)
        .env("ZUSE_AX_INDEX", index.to_string())
        .env("ZUSE_AX_NODES", nodes.to_string())
        .env("ZUSE_AX_DEPTH", depth.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("start native accessibility helper: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or("accessibility stdout unavailable")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("accessibility stderr unavailable")?;
    let output = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take((4 << 20) + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let errors = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.take(65537).read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("native accessibility helper timed out after 20 s".to_owned());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(error.to_string());
            }
        }
    };
    let bytes = output
        .join()
        .map_err(|_| "accessibility reader failed")?
        .map_err(|e| e.to_string())?;
    let error_bytes = errors
        .join()
        .map_err(|_| "accessibility error reader failed")?
        .map_err(|e| e.to_string())?;
    let status = status?;
    if bytes.len() > 4 << 20 || error_bytes.len() > 65536 {
        return Err("native accessibility output exceeded its bound".into());
    }
    if !status.success() {
        return Err(format!(
            "native accessibility failed: {}",
            String::from_utf8_lossy(&error_bytes).trim()
        ));
    }
    let result = String::from_utf8(bytes)
        .map_err(|_| "native accessibility returned non-UTF-8")?
        .trim()
        .trim_start_matches('\u{feff}')
        .to_owned();
    crate::browser::with_json(&result, |_| Ok(()))?;
    Ok(result)
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn run(
    _operation: &str,
    _path: &str,
    _value: &str,
    _index: usize,
    _nodes: usize,
    _depth: usize,
) -> Result<String> {
    Err("native accessibility is supported on Linux, Windows and macOS".into())
}

#[cfg(target_os = "windows")]
const WINDOWS: &str = r#"
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$walker=[System.Windows.Automation.TreeWalker]::ControlViewWalker
$root=[System.Windows.Automation.AutomationElement]::RootElement
$operation=$env:ZUSE_AX_OPERATION
$path=$env:ZUSE_AX_PATH
function Clip([string]$text,[int]$limit) { if($null -eq $text){return ''}; if($text.Length -gt $limit){return $text.Substring(0,$limit)}; return $text }
function Resolve-Node([string]$address) {
    $node=$root
    foreach($part in ($address -split '/' | Select-Object -Skip 1)) {
        $node=$walker.GetFirstChild($node)
        for($i=0;$i -lt [int]$part -and $null -ne $node;$i++){ $node=$walker.GetNextSibling($node) }
        if($null -eq $node){throw 'Accessibility path is stale; take a fresh snapshot'}
    }
    return $node
}
function Get-Actions($node) {
    $actions=New-Object 'System.Collections.Generic.List[object]'
    $pattern=$null
    if($node.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern,[ref]$pattern)){$actions.Add(@{index=0;name='invoke'})}
    $pattern=$null
    if($node.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern,[ref]$pattern)){$actions.Add(@{index=1;name='toggle'})}
    $pattern=$null
    if($node.TryGetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern,[ref]$pattern)){$actions.Add(@{index=2;name='expand_or_collapse'})}
    $pattern=$null
    if($node.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern,[ref]$pattern)){$actions.Add(@{index=3;name='select'})}
    return ,$actions.ToArray()
}
if($operation -eq 'snapshot') {
    $script:nodes=New-Object 'System.Collections.Generic.List[object]'
    $script:truncated=$false; $script:errors=0; $script:bytes=0; $script:exhausted=$false
    $maxNodes=[int]$env:ZUSE_AX_NODES; $maxDepth=[int]$env:ZUSE_AX_DEPTH
    $clock=[System.Diagnostics.Stopwatch]::StartNew()
    function Walk($node,[string]$address,[int]$level) {
        if($script:exhausted -or $script:nodes.Count -ge $maxNodes -or $clock.ElapsedMilliseconds -ge 5000){$script:truncated=$true;return}
        try {
            $current=$node.Current
            $name=Clip $current.Name 4096
            $role=$current.ControlType.ProgrammaticName
            $text=''
            if($current.IsPassword){$text='[redacted]'}else{
                $pattern=$null
                if($node.TryGetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern,[ref]$pattern)){$text=Clip ($pattern.DocumentRange.GetText(4096)) 4096}else{
                    $pattern=$null
                    if($node.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern,[ref]$pattern)){$text=Clip $pattern.Current.Value 4096}
                }
            }
            $rect=$current.BoundingRectangle
            $bounds=$null
            if(-not $rect.IsEmpty){$bounds=@{x=$rect.X;y=$rect.Y;width=$rect.Width;height=$rect.Height}}
            $item=[ordered]@{path=$address;role=$role;name=$name;text=$text;children=$null;bounds=$bounds;actions=(Get-Actions $node);automationId=$current.AutomationId}
            $encoded=ConvertTo-Json -InputObject $item -Depth 6 -Compress
            $size=[System.Text.Encoding]::UTF8.GetByteCount($encoded)
            if($script:bytes+$size -gt 2097152){$script:truncated=$true;$script:exhausted=$true;return}
            $script:bytes+=$size; $script:nodes.Add($item)
            $child=$walker.GetFirstChild($node)
            if($level -ge $maxDepth){if($null -ne $child){$script:truncated=$true};return}
            $index=0
            while($null -ne $child){
                if($script:exhausted -or $script:nodes.Count -ge $maxNodes -or $clock.ElapsedMilliseconds -ge 5000){$script:truncated=$true;break}
                Walk $child ($address+'/'+$index) ($level+1)
                $child=$walker.GetNextSibling($child);$index++
            }
        }catch{$script:errors++}
    }
    Walk $root '0' 0
    if($script:nodes.Count -eq 0){throw 'Windows UI Automation is unavailable'}
    ConvertTo-Json -InputObject @{backend='windows-uia';nodes=$script:nodes.ToArray();truncated=$script:truncated;errors=$script:errors} -Depth 8 -Compress
}else{
    $node=Resolve-Node $path
    switch($operation) {
        'focus' {$node.SetFocus()}
        'set_text' {
            if($node.Current.IsPassword){throw 'Password input requires human assistance'}
            $pattern=$null
            if(-not $node.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern,[ref]$pattern)){throw 'Control does not support the UI Automation Value pattern'}
            if($pattern.Current.IsReadOnly){throw 'Control is read-only'}
            $pattern.SetValue($env:ZUSE_AX_VALUE)
        }
        'action' {
            $index=[int]$env:ZUSE_AX_INDEX
            switch($index) {
                0 {$pattern=$node.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern);$pattern.Invoke()}
                1 {$pattern=$node.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern);$pattern.Toggle()}
                2 {$pattern=$node.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern);if($pattern.Current.ExpandCollapseState -eq [System.Windows.Automation.ExpandCollapseState]::Expanded){$pattern.Collapse()}else{$pattern.Expand()}}
                3 {$pattern=$node.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern);$pattern.Select()}
                default {throw 'Unsupported accessibility action index'}
            }
        }
        default {throw 'Unknown accessibility operation'}
    }
    ConvertTo-Json -InputObject @{path=$path;completed=$true;operation=$operation} -Compress
}
"#;

#[cfg(target_os = "macos")]
const MACOS: &str = r#"
ObjC.import('Foundation');
function env(name){var value=$.NSProcessInfo.processInfo.environment.objectForKey(name);return ObjC.unwrap(value)||'';}
var se=Application('System Events');
var operation=env('ZUSE_AX_OPERATION'), path=env('ZUSE_AX_PATH');
function children(node){return node===null?se.applicationProcesses():node.uiElements();}
function resolve(address){
    var node=null,parts=address.split('/');
    for(var i=1;i<parts.length;i++){var list=children(node),index=Number(parts[i]);if(index>=list.length)throw Error('Accessibility path is stale; take a fresh snapshot');node=list[index];}
    return node;
}
function scalar(value){if(value===null||value===undefined)return '';if(typeof value==='string'||typeof value==='number'||typeof value==='boolean')return String(value).slice(0,4096);return '';}
function role(node){if(node===null)return 'desktop';try{return scalar(node.role());}catch(e){return 'application';}}
function secure(node){if(node===null)return false;try{return /secure|password/i.test(String(node.subrole())+' '+role(node));}catch(e){return /secure|password/i.test(role(node));}}
function actions(node){
    if(node===null)return [];
    try{var list=node.actions(),result=[];for(var i=0;i<Math.min(list.length,64);i++)result.push({index:i,name:scalar(list[i].name()).slice(0,256)});return result;}catch(e){return [];}
}
function main(){
    if(operation==='snapshot'){
        var maxNodes=Number(env('ZUSE_AX_NODES')),maxDepth=Number(env('ZUSE_AX_DEPTH'));
        var nodes=[],truncated=false,errors=0,bytes=0,exhausted=false,deadline=Date.now()+5000;
        function walk(node,address,level){
            if(exhausted||nodes.length>=maxNodes||Date.now()>=deadline){truncated=true;return;}
            try{
                var name=node===null?'Desktop':scalar(node.name()),text='',bounds=null;
                if(secure(node))text='[redacted]';else if(node!==null){try{text=scalar(node.value());}catch(e){}}
                if(node!==null){try{var p=node.position(),s=node.size();bounds={x:p[0],y:p[1],width:s[0],height:s[1]};}catch(e){}}
                var item={path:address,role:role(node),name:name,text:text,children:null,bounds:bounds,actions:actions(node)};
                var size=unescape(encodeURIComponent(JSON.stringify(item))).length;
                if(bytes+size>2097152){truncated=true;exhausted=true;return;}
                bytes+=size;nodes.push(item);
                var list=children(node);
                if(level>=maxDepth){if(list.length)truncated=true;return;}
                for(var i=0;i<list.length;i++){
                    if(exhausted||nodes.length>=maxNodes||Date.now()>=deadline){truncated=true;break;}
                    walk(list[i],address+'/'+i,level+1);
                }
            }catch(e){errors++;}
        }
        // Check authorization before traversal so denial is a failing operation.
        se.applicationProcesses();
        walk(null,'0',0);
        if(nodes.length===0)throw Error('System Events accessibility is unavailable');
        return {backend:'macos-system-events',nodes:nodes,truncated:truncated,errors:errors};
    }
    var node=resolve(path);
    if(node===null)throw Error('The desktop root cannot be edited');
    if(operation==='focus'){node.attributes.byName('AXFocused').value=true;}
    else if(operation==='set_text'){
        if(secure(node))throw Error('Password input requires human assistance');
        node.value=env('ZUSE_AX_VALUE');
    }else if(operation==='action'){
        var list=node.actions(),index=Number(env('ZUSE_AX_INDEX'));
        if(index>=list.length)throw Error('Accessibility action does not exist');
        list[index].perform();
    }else throw Error('Unknown accessibility operation');
    return {path:path,completed:true,operation:operation};
}
JSON.stringify(main());
"#;
