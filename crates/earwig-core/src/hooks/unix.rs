//! Unix: the filter leads its own process group.

use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command};

use rustix::process::{Pid, Signal, kill_process_group};

pub(super) struct Tree(Pid);

pub(super) fn spawn(mut command: Command) -> io::Result<(Child, Tree)> {
    let child = command.process_group(0).spawn()?;
    let group = Pid::from_child(&child);
    Ok((child, Tree(group)))
}

impl Tree {
    pub(super) fn kill(&self) {
        let _ = kill_process_group(self.0, Signal::KILL);
    }
}
