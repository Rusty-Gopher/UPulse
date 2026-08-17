//! Small shared helpers for safely shelling out to external tools.

use std::process::{Command, Output};
use std::sync::mpsc;
use std::time::Duration;

/// Run a command to completion but give up after `secs`. The command runs on a
/// throwaway thread; on timeout we return an error and let that thread finish
/// on its own (all our timed commands are read-only — `dpkg-query`, `apt-cache`,
/// `apt list`, `lspci` — so an orphaned one is harmless and simply exits).
///
/// This keeps a stuck tool (dead NFS mount, held dpkg lock, wedged process) from
/// spinning a UI spinner forever.
pub fn output_timeout(mut cmd: Command, secs: u64) -> std::io::Result<Output> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(cmd.output());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(res) => res,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "command timed out",
        )),
    }
}

/// Whether an executable is found on `PATH` (a cheap `which`, no process spawn).
pub fn has_bin(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

/// True if the system has the tools this app needs to manage packages
/// (browse/install/remove and upgrades). Ubuntu/Debian desktops have all of
/// these; other systems generally don't, so we gate those features on it.
pub fn package_tools_available() -> bool {
    has_bin("pkexec") && has_bin("apt-get") && has_bin("dpkg-query")
}
