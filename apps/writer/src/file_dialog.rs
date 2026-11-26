//! System open dialogs run off the event loop. Dropping the picker closes its helper.
use std::{
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    time::Duration,
};

#[derive(Clone, Copy)]
pub(crate) enum FileKind {
    Font,
    Text,
}

impl FileKind {
    fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Font => &["gfont", "zfont"],
            Self::Text => &["txt"],
        }
    }
    pub(crate) fn accepts(self, path: &str) -> bool {
        Path::new(path)
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|ext| {
                self.extensions()
                    .iter()
                    .any(|v| ext.eq_ignore_ascii_case(v))
            })
    }
}

type Answer = Result<Option<String>, String>;
pub(crate) struct FileDialog {
    receiver: Receiver<Answer>,
    closed: Arc<AtomicBool>,
}

impl FileDialog {
    pub(crate) fn open(kind: FileKind, title: &str) -> Self {
        let title = crate::locale::text(title).to_owned();
        let (tx, receiver) = mpsc::channel();
        let closed = Arc::new(AtomicBool::new(false));
        let flag = closed.clone();
        std::thread::spawn(move || {
            let _ = tx.send(choose(kind, &title, &flag));
        });
        Self { receiver, closed }
    }

    pub(crate) fn poll(&self) -> Option<Answer> {
        match self.receiver.try_recv() {
            Ok(answer) => Some(answer),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("文件选择器已关闭，请再试一次。".into())),
        }
    }
}

impl Drop for FileDialog {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}

fn choose(kind: FileKind, title: &str, closed: &AtomicBool) -> Answer {
    let patterns: Vec<String> = kind
        .extensions()
        .iter()
        .flat_map(|s| [format!("*.{s}"), format!("*.{}", s.to_uppercase())])
        .collect();
    let filters = patterns.join(" ");
    let mut candidates: Vec<Command> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        let mut zenity = Command::new("zenity");
        zenity.args([
            "--file-selection",
            "--title",
            title,
            "--file-filter",
            &filters,
            "--file-filter",
            "*",
        ]);
        let mut kdialog = Command::new("kdialog");
        kdialog.args([
            "--title",
            title,
            "--getopenfilename",
            ".",
            &format!("{filters}|{filters}\n*|*"),
        ]);
        if std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("kde")
        {
            candidates.extend([kdialog, zenity]);
        } else {
            candidates.extend([zenity, kdialog]);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("osascript");
        cmd.args(["-e", "on run argv\ntry\nreturn POSIX path of (choose file with prompt (item 1 of argv))\non error number -128\nreturn \"\"\nend try\nend run", "--",title]);
        candidates.push(cmd);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new("powershell.exe");
        cmd.creation_flags(0x08000000).args(["-NoProfile","-STA","-Command",r#"Add-Type -AssemblyName System.Windows.Forms; $d=New-Object System.Windows.Forms.OpenFileDialog; $d.Title=$env:ZUSE_DIALOG_TITLE; $d.Filter=$env:ZUSE_DIALOG_FILTER; if ($d.ShowDialog() -eq 'OK') { [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); [Console]::Write($d.FileName) }"#])
            .env("ZUSE_DIALOG_TITLE",title).env("ZUSE_DIALOG_FILTER",format!("{filters}|{}|*|*.*",patterns.join(";")));
        candidates.push(cmd);
    }
    let mut tk = Command::new(if cfg!(windows) { "python" } else { "python3" });
    tk.args(["-c", "import sys, tkinter as tk\nfrom tkinter import filedialog\nr=tk.Tk(); r.withdraw()\ns=filedialog.askopenfilename(parent=r,title=sys.argv[1],filetypes=[(sys.argv[2],tuple(sys.argv[2].split())),('*','*')]); r.destroy()\nsys.stdout.buffer.write(s.encode('utf-8'))", title,&filters]);
    candidates.push(tk);
    for mut cmd in candidates {
        if closed.load(Ordering::Acquire) {
            return Ok(None);
        }
        let Ok(mut child) = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        use std::io::Read;
        let mut stdout = child.stdout.take().expect("piped");
        let output = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let status = loop {
            if closed.load(Ordering::Acquire) {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(None);
            }
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => std::thread::sleep(Duration::from_millis(40)),
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let Some(status) = status else {
            continue;
        };
        if status.success() {
            let bytes = output
                .join()
                .ok()
                .and_then(Result::ok)
                .ok_or("文件选择结果读取失败，请再试一次。")?;
            let name = String::from_utf8(bytes)
                .map_err(|_| "请选择使用 UTF-8 文件名的文件，或手工输入路径。")?;
            let name = if matches!(
                cmd.get_program().to_str(),
                Some("zenity" | "kdialog" | "osascript")
            ) {
                name.strip_suffix("\r\n")
                    .or_else(|| name.strip_suffix('\n'))
                    .unwrap_or(&name)
            } else {
                &name
            };
            if name.is_empty() {
                return Ok(None);
            }
            if !kind.accepts(name) {
                return Err(format!("请选择 {} 文件。", filters));
            }
            return Ok(Some(name.to_owned()));
        }
        if status.code() == Some(1)
            && matches!(cmd.get_program().to_str(), Some("zenity" | "kdialog"))
        {
            return Ok(None);
        }
    }
    Err("无法打开文件选择器。可手工输入字体路径或粘贴文章；Linux 也可安装 Zenity 或 KDialog 后重试。".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_accepts_the_selected_file_kind() {
        assert!(FileKind::Font.accepts("/tmp/我的字体.GFONT"));
        assert!(FileKind::Font.accepts("stroke.ZfOnT"));
        assert!(FileKind::Text.accepts("今天.TXT"));
        assert!(!FileKind::Text.accepts("a.txt.zip"));
        assert!(!FileKind::Font.accepts("a.txt"));
    }
}
