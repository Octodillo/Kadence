//! Child processes that run plugin code, and so may start helper processes of their own (a
//! bridge, a licence daemon, an external UI). Each gets its own process group, which is killed
//! as a whole, so its helpers go with it. A helper that leaves the group (a daemon that starts
//! a session of its own) is out of reach.

use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

/// Makes `command`'s process the leader of a new process group, for [`kill_group`].
pub fn in_own_group(command: &mut Command) -> &mut Command {
    command.process_group(0)
}

/// Kills (SIGKILL) every process in the group of a `child` started [`in_own_group`]. Call it
/// only before `child` has been waited for: until then its process id, and with it the group's,
/// cannot be reused. The child still has to be waited for.
///
/// The child is signalled by itself as well: it may have moved to another group, which would
/// leave nothing under its id.
pub fn kill_group(child: &Child) {
    let Ok(pid) = libc::pid_t::try_from(child.id()) else {
        return;
    };
    for target in [-pid, pid] {
        // SAFETY: plain syscall; see above for why both ids are still the child's.
        if unsafe { libc::kill(target, libc::SIGKILL) } != 0 {
            let e = std::io::Error::last_os_error();
            // No such process: the group is empty, or the child is a zombie already.
            if e.raw_os_error() != Some(libc::ESRCH) {
                log::warn!("cannot kill plugin process {target}: {e}");
            }
        }
    }
}

/// Whether `child` has exited, without waiting for it: [`kill_group`] is still safe after a
/// true answer, and `child.wait()` then returns at once.
pub fn has_exited(child: &Child) -> std::io::Result<bool> {
    // SAFETY: an all-zero `siginfo_t` is valid, and `waitid` writes only into it. WNOWAIT
    // leaves the child a zombie, so its ids stay ours.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        let options = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
        if libc::waitid(libc::P_PID, child.id(), &mut info, options) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(info.si_pid() != 0)
    }
}
