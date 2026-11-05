use std::{
    collections::{HashMap, HashSet},
    ffi::CStr,
    os::unix::fs::MetadataExt,
    path::Path,
    process::Command,
    sync::Mutex,
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Clone)]
pub(crate) struct Process {
    pub pid: u32,
    pub parent: Option<u32>,
    pub start: u64,
    pub argv: Vec<String>,
}

impl Process {
    pub fn named(&self, name: &str) -> bool {
        self.argv
            .iter()
            .take(2)
            .any(|arg| Path::new(arg).file_name().is_some_and(|n| n == name))
    }
}

#[derive(Default)]
pub(crate) struct Inspector {
    system: System,
    cache: HashMap<u32, Option<Process>>,
}

impl Inspector {
    pub fn get(&mut self, pid: u32) -> Option<Process> {
        self.cache
            .entry(pid)
            .or_insert_with(|| {
                let id = Pid::from_u32(pid);
                self.system.refresh_processes_specifics(
                    ProcessesToUpdate::Some(&[id]),
                    true,
                    ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
                );
                self.system.process(id).map(|p| Process {
                    pid,
                    parent: p.parent().map(|p| p.as_u32()),
                    start: p.start_time(),
                    argv: p
                        .cmd()
                        .iter()
                        .map(|s| s.to_string_lossy().into_owned())
                        .collect(),
                })
            })
            .clone()
    }

    pub fn refresh(&mut self, pid: u32) -> Option<Process> {
        self.cache.remove(&pid);
        self.get(pid)
    }

    pub fn chain(&mut self, pid: u32) -> Vec<Process> {
        let mut chain = Vec::new();
        let mut next = Some(pid);
        let mut seen = HashSet::new();
        while let Some(pid) = next {
            if pid == 0 || !seen.insert(pid) {
                break;
            }
            let Some(p) = self.get(pid) else {
                break;
            };
            next = p.parent;
            chain.push(p);
        }
        chain
    }
}

pub(crate) struct Usage {
    parent: u32,
    cpu: f64,
}

pub(crate) fn usage() -> anyhow::Result<HashMap<u32, Usage>> {
    let out = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,%cpu="])
        .env("LC_ALL", "C")
        .output()?;
    anyhow::ensure!(out.status.success(), "ps failed");
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let parent = fields.next()?.parse().ok()?;
            let cpu = fields.next()?.parse().ok()?;
            Some((pid, Usage { parent, cpu }))
        })
        .collect())
}

pub(crate) fn tree_cpu(pid: u32, usage: &HashMap<u32, Usage>) -> f64 {
    let mut descendants = HashSet::from([pid]);
    loop {
        let before = descendants.len();
        for (&child, u) in usage {
            if descendants.contains(&u.parent) {
                descendants.insert(child);
            }
        }
        if descendants.len() == before {
            break;
        }
    }
    descendants
        .iter()
        .filter_map(|p| usage.get(p))
        .map(|u| u.cpu)
        .sum()
}

pub(crate) fn tty(pid: u32) -> Option<String> {
    // devname returns a shared buffer; copy it while holding the lock.
    static DEVNAME: Mutex<()> = Mutex::new(());
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // The kernel initializes the entire struct on a full-sized successful read.
    let info = unsafe {
        if libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        ) != size
        {
            return None;
        }
        info.assume_init()
    };
    if info.e_tdev == u32::MAX {
        return None;
    }
    // devname scans /dev (an lstat per entry), most of an idle app tick. A pty's name follows from its minor number,
    // so check that one path first.
    let pty = format!("/dev/ttys{:03}", info.e_tdev & 0xff_ffff);
    if std::fs::metadata(&pty).is_ok_and(|m| m.rdev() == u64::from(info.e_tdev)) {
        return Some(pty);
    }
    let _guard = DEVNAME.lock().ok()?;
    // A non-null devname result is a NUL-terminated string valid until the next call.
    unsafe {
        let name = libc::devname(info.e_tdev as libc::dev_t, libc::S_IFCHR);
        (!name.is_null()).then(|| format!("/dev/{}", CStr::from_ptr(name).to_string_lossy()))
    }
}

pub(crate) fn parse_utc_start(start: &str) -> Option<u64> {
    chrono::NaiveDateTime::parse_from_str(start.trim(), "%a %b %e %T %Y")
        .ok()?
        .and_utc()
        .timestamp()
        .try_into()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_replaces_cached_dead_or_reused_identity() {
        let pid = std::process::id();
        let mut inspector = Inspector::default();
        let current = inspector.get(pid).unwrap();
        let mut reused = current.clone();
        reused.start -= 1;
        for stale in [None, Some(reused)] {
            inspector.cache.insert(pid, stale.clone());
            assert_eq!(inspector.get(pid).map(|p| p.start), stale.map(|p| p.start));
            assert_eq!(inspector.refresh(pid).unwrap().start, current.start);
            assert_eq!(inspector.get(pid).unwrap().start, current.start);
        }
    }

    #[test]
    fn registry_start_is_epoch_seconds_in_utc() {
        for start in ["Tue Nov  3 17:00:00 2026", " Tue Nov 3 17:00:00 2026 "] {
            assert_eq!(parse_utc_start(start), Some(1_793_725_200));
        }
        assert_eq!(parse_utc_start("not a date"), None);
    }
}
