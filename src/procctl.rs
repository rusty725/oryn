//! Actions on processes from the process panel: terminate, kill, renice,
//! start a command, and the search filter. Errors come back as short,
//! user-facing messages for the panel's status line.

use crate::sysstats::ProcInfo;

/// Does `p` match the search text? Case-insensitive on name, command and
/// user; a number also matches the PID exactly.
pub fn matches(p: &ProcInfo, filter: &str) -> bool {
    let f = filter.trim();
    if f.is_empty() {
        return true;
    }
    if let Ok(pid) = f.parse::<u32>()
        && p.pid == pid {
            return true;
        }
    let f = f.to_lowercase();
    p.name.to_lowercase().contains(&f) || p.cmd.to_lowercase().contains(&f) || p.user.to_lowercase().contains(&f)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sig {
    /// SIGTERM: ask the process to exit.
    Term,
    /// SIGKILL: stop it immediately.
    Kill,
}

/// Send `sig` to `pid`.
pub fn signal(pid: u32, sig: Sig) -> Result<(), String> {
    if pid == std::process::id() {
        return Err("that's Oryn itself — press q to quit".into());
    }
    // kill(2) treats 0 and negative numbers as whole process groups.
    if pid == 0 || pid > i32::MAX as u32 {
        return Err("not a valid process ID".into());
    }
    #[cfg(unix)]
    {
        let s = match sig {
            Sig::Term => libc::SIGTERM,
            Sig::Kill => libc::SIGKILL,
        };
        // SAFETY: plain syscall with a pid and signal number.
        if unsafe { libc::kill(pid as libc::pid_t, s) } == 0 {
            Ok(())
        } else {
            Err(errno_message("signal"))
        }
    }
    #[cfg(not(unix))]
    {
        let sys = sysinfo::System::new_with_specifics(
            sysinfo::RefreshKind::nothing().with_processes(sysinfo::ProcessRefreshKind::nothing()),
        );
        let proc_ = sys.process(sysinfo::Pid::from_u32(pid)).ok_or("process no longer exists")?;
        let ok = match sig {
            Sig::Term => proc_.kill_with(sysinfo::Signal::Term).unwrap_or(false) || proc_.kill(),
            Sig::Kill => proc_.kill(),
        };
        if ok { Ok(()) } else { Err("could not stop the process (permission?)".into()) }
    }
}

/// Set the nice value (-20 = highest priority … 19 = lowest).
pub fn renice(pid: u32, nice: i32) -> Result<(), String> {
    if !(-20..=19).contains(&nice) {
        return Err("nice must be between -20 and 19".into());
    }
    // setpriority(2) treats 0 as "this process", i.e. Oryn.
    if pid == 0 || pid > i32::MAX as u32 {
        return Err("not a valid process ID".into());
    }
    #[cfg(unix)]
    {
        // SAFETY: plain syscall.
        let r = unsafe { libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, nice) };
        if r == 0 { Ok(()) } else { Err(errno_message("renice")) }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Err("changing nice isn't supported on this OS".into())
    }
}

/// Is the process still running? A zombie (exited, not yet reaped by its
/// parent) counts as gone.
pub fn alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => {
                let state = stat.rfind(')').and_then(|i| stat.get(i + 2..)).and_then(|r| r.chars().next());
                !matches!(state, Some('Z') | Some('X'))
            }
            Err(_) => false,
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // Signal 0 only checks that the process exists.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Current nice value, or None if unknown.
pub fn nice_of(pid: u32) -> Option<i32> {
    #[cfg(target_os = "linux")]
    {
        // /proc/<pid>/stat: field 19 is nice. The command name (field 2) is in
        // parentheses and may contain spaces, so split after the last ')'.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = stat.get(stat.rfind(')')? + 2..)?;
        rest.split_whitespace().nth(16)?.parse().ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// Start `command` in the background, detached from Oryn: its own process
/// group, no terminal input/output (so it can't draw over the UI), and it
/// keeps running after Oryn exits. Returns the new PID.
pub fn exec(command: &str) -> Result<u32, String> {
    let command = command.trim();
    if command.is_empty() {
        return Err("no command given".into());
    }
    // A plain "program arg arg" runs directly, so the PID shown is the
    // program's own; anything using shell syntax goes through sh -c.
    let plain = !command.chars().any(|c| ";&|<>()$`*?~\"'\\\n{}[]=#!".contains(c));
    #[cfg(unix)]
    let mut cmd = {
        use std::os::unix::process::CommandExt;
        let mut c = if plain {
            let mut words = command.split_whitespace();
            let mut c = std::process::Command::new(words.next().unwrap_or_default());
            c.args(words);
            c
        } else {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg(command);
            c
        };
        c.process_group(0);
        c
    };
    #[cfg(not(unix))]
    let mut cmd = {
        let _ = plain;
        let mut c = std::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    };
    let child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start: {e}"))?;
    let pid = child.id();
    // Reap it when it exits so it doesn't linger as a zombie. The thread
    // only waits, so it gets a small stack instead of the default 8 MB.
    let _ = std::thread::Builder::new().name("oryn-reap".into()).stack_size(32 * 1024).spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(pid)
}

#[cfg(unix)]
fn errno_message(what: &str) -> String {
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EPERM) | Some(libc::EACCES) => match what {
            "renice" => "permission denied — run with sudo for this".into(),
            _ => "permission denied — another user's process (run with sudo)".into(),
        },
        Some(libc::ESRCH) => "process no longer exists".into(),
        _ => format!("{what} failed: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, name: &str, cmd: &str, user: &str) -> ProcInfo {
        ProcInfo { pid, name: name.into(), cmd: cmd.into(), user: user.into(), mem: 0, cpu: 0.0, nice: None }
    }

    #[test]
    fn filter() {
        let fx = p(9979, "Isolated Web Co", "/usr/lib/firefox/firefox -contentproc", "rusty");
        assert!(matches(&fx, ""));
        assert!(matches(&fx, "FIREFOX"));
        assert!(matches(&fx, "isolated"));
        assert!(matches(&fx, "rusty"));
        assert!(matches(&fx, "9979"));
        assert!(!matches(&fx, "chrome"));
    }

    #[test]
    fn start_nice_terminate_kill() {
        // A real child process: start, read and change its nice, stop it.
        let pid = exec("sleep 30").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        #[cfg(target_os = "linux")]
        assert_eq!(nice_of(pid), Some(0));
        renice(pid, 10).unwrap();
        #[cfg(target_os = "linux")]
        assert_eq!(nice_of(pid), Some(10));
        assert!(renice(pid, 25).is_err());
        signal(pid, Sig::Term).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(signal(pid, Sig::Kill).is_err()); // already gone
        let pid2 = exec("sleep 30").unwrap();
        // The kernel renames the child a moment after spawn returns.
        std::thread::sleep(std::time::Duration::from_millis(100));
        #[cfg(target_os = "linux")]
        assert_eq!(std::fs::read_to_string(format!("/proc/{pid2}/comm")).unwrap().trim(), "sleep");
        assert!(exec("definitely-not-a-program-xyz").is_err());
        assert!(exec("true && sleep 0").is_ok());
        signal(pid2, Sig::Kill).unwrap();
        assert!(signal(std::process::id(), Sig::Term).is_err()); // never ourselves
        assert!(signal(0, Sig::Term).is_err()); // never a whole process group
        assert!(renice(0, 5).is_err());
        #[cfg(target_os = "linux")]
        {
            assert!(alive(std::process::id()));
            assert!(!alive(pid2) || { std::thread::sleep(std::time::Duration::from_millis(200)); !alive(pid2) });
        }
    }
}
