//! Running helper programs (smartctl, xrandr, desktop `--version` probes)
//! without ever blocking past a deadline.
//!
//! `Command::output()` reads the pipes only after the child exits and waits
//! for EOF on them: a child that prints more than the pipe holds (64 KiB on
//! Linux, less elsewhere) blocks until it is killed, and a grandchild that
//! keeps stdout open (a daemon started in the background) blocks the caller
//! forever. Here both pipes are drained on their own threads while waiting.

use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// What a finished command printed.
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Run `cmd` (stdin closed, stdout and stderr captured) for at most
/// `timeout`. A command still running then is killed and reaped, and None
/// is returned. If the command exits in time but something it started
/// keeps a pipe open, whatever arrived before the deadline is returned and
/// the reader thread is left behind to finish on its own.
pub fn output_within(cmd: &mut Command, timeout: Duration) -> Option<Output> {
    let deadline = Instant::now() + timeout;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    Some(Output { status, stdout: collect(stdout, deadline), stderr: collect(stderr, deadline) })
}

/// A pipe being read on its own thread: what has arrived so far, and a
/// channel that closes at EOF.
type Drain = (Arc<Mutex<Vec<u8>>>, mpsc::Receiver<()>);

fn drain(pipe: Option<impl Read + Send + 'static>) -> Drain {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = mpsc::channel::<()>();
    if let Some(mut p) = pipe {
        let shared = Arc::clone(&buf);
        thread::spawn(move || {
            let _eof = tx;
            let mut chunk = [0u8; 16384];
            loop {
                match p.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => shared.lock().unwrap().extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
    }
    (buf, rx)
}

/// What the pipe delivered, waiting for EOF no later than `deadline`.
fn collect((buf, eof): Drain, deadline: Instant) -> Vec<u8> {
    // Err either way: Disconnected at EOF, Timeout if still open.
    let _ = eof.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    std::mem::take(&mut *buf.lock().unwrap())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(script: &str, timeout: Duration) -> (Option<Output>, Duration) {
        let start = Instant::now();
        let out = output_within(Command::new("/bin/sh").args(["-c", script]), timeout);
        (out, start.elapsed())
    }

    #[test]
    fn big_output_is_read_while_running() {
        // ~1 MiB, far more than any pipe buffer.
        let (out, _) = sh("i=0; while [ $i -lt 16 ]; do dd if=/dev/zero bs=65536 count=1 2>/dev/null; i=$((i+1)); done; echo done >&2", Duration::from_secs(10));
        let out = out.expect("finished in time");
        assert!(out.status.success());
        assert_eq!(out.stdout.len(), 16 * 65536);
        assert_eq!(out.stderr, b"done\n");
    }

    #[test]
    fn slow_child_is_killed_at_the_deadline() {
        let (out, took) = sh("sleep 5", Duration::from_millis(300));
        assert!(out.is_none());
        assert!(took < Duration::from_secs(5), "took {took:?}");
    }

    #[test]
    fn grandchild_holding_the_pipe_does_not_block() {
        // The shell exits at once; the background sleep keeps stdout open.
        let (out, took) = sh("echo hello; sleep 5 &", Duration::from_millis(500));
        let out = out.expect("the shell itself exited");
        assert!(out.status.success());
        assert_eq!(out.stdout, b"hello\n");
        assert!(took < Duration::from_secs(5), "took {took:?}");
    }

    #[test]
    fn missing_program() {
        assert!(output_within(&mut Command::new("/nonexistent/oryn-test"), Duration::from_secs(1)).is_none());
    }
}
