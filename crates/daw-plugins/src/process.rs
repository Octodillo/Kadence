//! Child processes that run plugin code, and so may start helper processes of their own (a
//! bridge, a licence daemon, an external UI). Each gets its own process group, which is killed
//! as a whole, so no helper outlives it.

use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

/// Makes `command`'s process the leader of a new process group, for [`kill_group`].
pub fn in_own_group(command: &mut Command) -> &mut Command {
    command.process_group(0)
}

/// Kills (SIGKILL) every process in the group of a `child` started [`in_own_group`]. Call it
/// only before `child` has been waited for: until then its process id, and with it the group's,
/// cannot be reused. The child still has to be waited for.
pub fn kill_group(child: &Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: plain syscall; see above for why the group id is still the child's.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
}
