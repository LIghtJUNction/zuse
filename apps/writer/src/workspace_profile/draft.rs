//! An unfinished XY measurement is scoped to one running controller session.
//! Reading a draft never installs workspace bounds or authorizes a move.
use super::{Binding, MAX_FILE, Result, config_root, write_atomic};
use std::{
    fs,
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub pid: u64,
    pub revision: u64,
    pub job: u64,
    pub generation: u64,
    pub incarnation: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Draft {
    pub stamp: Stamp,
    pub top: [f64; 2],
    pub bottom: Option<[f64; 2]>,
    identity: String,
}

impl Draft {
    pub fn new(stamp: Stamp, binding: &Binding, top: [f64; 2], bottom: Option<[f64; 2]>) -> Self {
        Self {
            stamp,
            top,
            bottom,
            identity: binding.header(),
        }
    }

    pub fn matches(&self, binding: &Binding) -> bool {
        self.identity == binding.header()
    }

    pub fn offset(&self) -> Result<[f64; 3]> {
        let fields = self
            .identity
            .lines()
            .last()
            .and_then(|line| line.strip_prefix("wco="))
            .ok_or("角点草稿缺少工作偏移")?;
        let values = fields
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| "草稿工作偏移无效")?;
        match values.as_slice() {
            &[x, y, z] if [x, y, z].iter().all(|v| v.is_finite()) => Ok([x, y, z]),
            _ => Err("草稿工作偏移无效".into()),
        }
    }

    fn path(root: &Path, session: &str) -> Result<PathBuf> {
        if session.is_empty()
            || session.len() > 100
            || !session
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err("会话名不能关联角点草稿".into());
        }
        Ok(root
            .with_file_name("corner-drafts")
            .join(format!("{session}.draft")))
    }

    pub fn save(&self, session: &str) -> Result<()> {
        if self.stamp.incarnation.is_empty() {
            return Err("无法确认后台进程的启动身份，角点仅在当前窗口保留".into());
        }
        write_atomic(&Self::path(&config_root()?, session)?, &self.encode())
    }

    fn encode(&self) -> String {
        let Stamp {
            pid,
            revision,
            job,
            generation,
            incarnation,
        } = &self.stamp;
        let [x, y] = self.top;
        let bottom = self
            .bottom
            .map(|[x, y]| format!(" {x} {y}"))
            .unwrap_or_default();
        format!(
            "writer-xy-draft-v1\n{pid} {revision} {job} {generation}\n{incarnation}\n{x} {y}{bottom}\n{}",
            self.identity
        )
    }

    fn decode(raw: &str) -> Result<Self> {
        let err = "角点草稿不完整，请重新记录";
        let mut parts = raw.splitn(5, '\n');
        if parts.next() != Some("writer-xy-draft-v1") {
            return Err(err.into());
        }
        let nums = parts
            .next()
            .ok_or(err)?
            .split_whitespace()
            .map(str::parse::<u64>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| err)?;
        let &[pid, revision, job, generation] = nums.as_slice() else {
            return Err(err.into());
        };
        let incarnation = parts
            .next()
            .filter(|s| !s.is_empty() && s.len() < 160 && !s.chars().any(char::is_control))
            .ok_or(err)?
            .into();
        let coords = parts
            .next()
            .ok_or(err)?
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| err)?;
        if !coords.iter().all(|n| n.is_finite()) {
            return Err(err.into());
        }
        let (top, bottom) = match *coords.as_slice() {
            [x, y] => ([x, y], None),
            [x, y, a, b] if x != a && y != b => ([x, y], Some([a, b])),
            _ => return Err(err.into()),
        };
        Ok(Self {
            stamp: Stamp {
                pid,
                revision,
                job,
                generation,
                incarnation,
            },
            top,
            bottom,
            identity: parts.next().ok_or(err)?.into(),
        })
    }

    pub fn load(session: &str) -> Result<Option<Self>> {
        let f = match fs::File::open(Self::path(&config_root()?, session)?) {
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            res => res.map_err(|e| e.to_string())?,
        };
        let mut raw = String::new();
        f.take(MAX_FILE + 1)
            .read_to_string(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() as u64 > MAX_FILE {
            return Err("角点草稿超出大小限制".into());
        }
        Self::decode(&raw).map(Some)
    }

    pub fn clear(session: &str) -> Result<()> {
        match fs::remove_file(Self::path(&config_root()?, session)?) {
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            res => res.map_err(|e| e.to_string()),
        }
    }
}

/// Linux Writer talks to a local Unix session. A reused PID after process exit
/// or a system restart must not revive a previous session's reference counter.
pub(crate) fn incarnation(pid: u64) -> Option<String> {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let ticks = stat
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)?
        .parse::<u64>()
        .ok()?;
    Some(format!("{}:{ticks}", boot.trim()))
}
