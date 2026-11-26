#!/usr/bin/env python3
"""Linux/X11 product acceptance. Run on an Xvfb display with xdotool and xclip.
usage: python3 picker-smoke.py /path/to/zuse-writer /path/to/zuse /path/to/zuse-source /tmp/evidence
Only generated files and a private font directory are used. Never opens a machine link.
"""
import csv, hashlib, importlib.util, io, json, os, re, subprocess, sys, tempfile, time
from pathlib import Path
writer, engine, repo, dest = map(lambda p: Path(p).resolve(), sys.argv[1:5])
dest.mkdir(parents=True,exist_ok=True)
tmp = tempfile.TemporaryDirectory(prefix="picker-"); d=Path(tmp.name)
s=importlib.util.spec_from_file_location("fixture", repo/"tests/gfont_import.py")
fixture=importlib.util.module_from_spec(s); s.loader.exec_module(fixture)
font=d/"样本-行楷.gfont"; fixture.sample(font, 9)
article=d/"测试文章.txt"; article.write_bytes(b"\xef\xbb\xbfA\r\nA\n")
env={**os.environ,"ZUSE_BIN":str(engine),"ZUSE_FONT_DIR":str(d/"fonts"),"XDG_CONFIG_HOME":str(d/"config"),"ZUSE_SESSION_DIR":str(d/"session"),"ZUSE_WRITER_SESSION":"picker-test"}
def cmd(*args, **kw): return subprocess.run(list(map(str,args)),env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=True,**kw).stdout
def x(*a):return cmd("xdotool",*a).decode().strip()
def windows(pattern):
    try: return list(filter(None,x('search','--onlyvisible','--name',pattern).splitlines()))
    except subprocess.CalledProcessError: return []
def wait(f, timeout=8):
    end=time.monotonic()+timeout
    while time.monotonic()<end:
        v=f()
        if v:return v
        time.sleep(.09)
    raise AssertionError("condition timed out: "+str(f))
def key(*ks): x("key","--clearmodifiers",*ks); time.sleep(.15)
def click(xp,yp):x("mousemove","--window",wid,xp,yp,"click",1); time.sleep(.30)
def listing():return json.loads(cmd(engine,"font","list"))['fonts']
def focus(id):x("windowfocus",id);time.sleep(.18)
def proof(filename):cmd("import","-window",wid,dest/filename)
def choose_file(title, path):
    dialog=wait(lambda: next(iter(windows(title)),None));focus(dialog)
    # GTK and Tk both return a normal OS path; font conversion itself is real.
    # For native GTK, Control+L gives an editable location. Text arrives as UTF-8 through X selection.
    key("ctrl+l");key("ctrl+a")
    clip=subprocess.Popen(["xclip","-selection","clipboard","-quiet","-loops","0"],env=env,stdin=subprocess.PIPE,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    clip.stdin.write(os.fsencode(path));clip.stdin.close();time.sleep(.2)
    try:key("ctrl+v");key("Return");wait(lambda:dialog not in windows(title))
    finally:
        if clip.poll() is None:clip.terminate();clip.wait(timeout=3)
    focus(wid)

last_ocr=""
def search_label(label):
    global last_ocr
    """Locate the real painted English label. Works with any window size; no fixed button coordinates."""
    img=dest/"last-window.png"
    cmd("import","-window",wid,img)
    proc=subprocess.Popen(["convert",str(img),"-resize","200%","-colorspace","Gray","-contrast-stretch","1%x1%","png:-"],stdout=subprocess.PIPE)
    try:
        t=subprocess.run(["tesseract","stdin","stdout","-l","eng","--psm","11","tsv"],stdin=proc.stdout,capture_output=True,check=True,text=False).stdout.decode()
    finally:
        proc.stdout.close();proc.wait()
    from difflib import SequenceMatcher
    words=[row for row in csv.DictReader(io.StringIO(t),delimiter="\t",quoting=csv.QUOTE_NONE)
           if (row.get("text") or "").strip() and all((row.get(k) or "").isdigit() for k in ("top","left","width","height"))]
    last_ocr=" ".join(w["text"] for w in words)
    def normalized(word):return re.sub(r"[^a-z0-9]","",word.lower())
    # If just the leading verb on a blue button is misread, its specific suffix is still a valid hit.
    aliases={"Choose font file":"font file","Import custom font":"custom font","Open text file":"text file"}
    phrases=[normalized(label)]
    if label in aliases:phrases.append(normalized(aliases[label]))
    boxes=[(normalized(row["text"]),int(row["left"]),int(row["top"]),int(row["width"]),int(row["height"])) for row in words]
    boxes=[b for b in boxes if b[0]]
    best=0.83
    point=None
    for i,(token,left,top,width,height) in enumerate(boxes):
        text="";right=left;bottom=top+height
        for token,x,y,w,h in boxes[i:i+len(label.split())+3]:
            # Stay within a single painted button. Tesseract occasionally splits its text across blocks.
            if abs((y+h/2)-(top+height/2))>max(height,h)*.65 or x<right-4 or x-right>32:break
            text+=token;right=x+w;bottom=max(bottom,y+h)
            rating=max(SequenceMatcher(None,text,p).ratio() for p in phrases)
            if rating>best:
                best=rating;point=((left+right)//4,(top+bottom)//4)
            if rating==1:return point
    return point
def click_label(label):
    try:pt=wait(lambda:search_label(label),timeout=20)
    except AssertionError as e:raise AssertionError(f"Could not find {label!r} in Writer: {last_ocr}") from e
    click(*pt)
def stamp(s):print(s,flush=True)
p=subprocess.Popen([str(writer)],cwd=d,env=env,stdout=(dest/"stdout").open("w"),stderr=(dest/"stderr").open("w"))
try:
    wid=wait(lambda: next(iter(windows("Zuse Writer")),None))
    focus(wid);time.sleep(1)
    click_label("English")
    proof("writer-first-screen.png")
    click_label("Choose font")
    click_label("Choose font file")
    dialog=wait(lambda: next(iter(windows("Choose font file")),None))
    focus(dialog);key("Escape");wait(lambda:dialog not in windows("Choose font file"))
    assert listing()==["builtin"]
    focus(wid);click_label("Choose font file");choose_file("Choose font file",font)
    # No path or ASCII name is typed in Writer.
    click_label("Import custom font");wait(lambda:len(listing())==2)
    name=next(n for n in listing() if n!="builtin")
    original=(d/"fonts"/(name+".zfont")).read_bytes()
    assert name.startswith("local-") and name.isascii()
    assert json.loads(cmd(engine,"-c",f'write.preview(text="A",font="{name}",size=1)'))["results"][0]["value"]["strokes"]==[[[0,.25],[1,.25]],[[.25,.75],[.75,.75]]]
    proof("writer-font-picker.png")
    click_label("Choose font file");choose_file("Choose font file",font)
    click_label("Import custom font");wait(lambda:len(listing())==3)
    assert name+"-2" in listing() and (d/"fonts"/(name+".zfont")).read_bytes()==original
    stamp(dict(alias=name,duplicate=name+"-2",original_sha256=hashlib.sha256(original).hexdigest()))
    click_label("Workspace");click_label("Edit article")
    click_label("Open text file");choose_file("Open a UTF-8 text file",article)
    time.sleep(.7);proof("writer-article-open.png")
    # Copy all draft text from the native editor, independently verifying CRLF normalization and BOM removal.
    key("ctrl+a"); key("ctrl+c")
    actual=cmd("xclip","-selection","clipboard","-o",timeout=5).decode()
    assert actual=="A\nA\n",repr(actual)
    click_label("Apply text");stamp("article A\\nA imported and applied")
except BaseException:
    if "wid" in globals():proof("failure.png")
    cmd("import","-window","root",dest/"failure-desktop.png")
    print("Windows at failure:", x("search","--onlyvisible","--name",".","getwindowname","%@"),flush=True)
    raise
finally:
    if p.poll() is None:p.terminate();p.wait(timeout=5)
    subprocess.run([engine,"session","close","picker-test"],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
