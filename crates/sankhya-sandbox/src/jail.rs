//! The syscalls, and the only file in this crate that writes `unsafe`.
//!
//! # The shape of everything below
//!
//! Each function here does its work between `fork` and `exec`, in a child that must not
//! allocate --- `fork` in a threaded process leaves the child holding locks no thread will ever
//! release, and the allocator's is one of them. So **every string, every path and every list is
//! built by the parent** and the child only makes syscalls over what it was handed.
//!
//! That constraint is why [`Plan`] exists. It looks like premature structure and it is not: it
//! is the parent's half of the work, done where allocation is safe.

#![allow(unsafe_code)]

use super::{Bounds, Unavailable};
use std::ffi::CString;
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Namespaces entered together, in one call.
///
/// Together and not one at a time: the user namespace is what grants the capabilities the
/// others need, and the kernel applies it first within a single `unshare`. Split into two calls
/// the second fails with `EPERM` on any machine where this feature matters.
const NAMESPACES: libc::c_int = libc::CLONE_NEWUSER
    | libc::CLONE_NEWNET
    | libc::CLONE_NEWNS
    | libc::CLONE_NEWIPC
    | libc::CLONE_NEWPID;

/// Whether the namespaces can be entered on this machine.
///
/// Entered in a **child**, because `unshare(CLONE_NEWUSER)` is irreversible for the process
/// that calls it: probing in the server's own process would leave the server in a namespace
/// with no uid map, unable to read the files it serves.
pub(crate) fn probe() -> Result<(), Unavailable> {
    entered_in_a_child()
}

/// Fork, enter the namespaces in the child, and report what the child found.
///
/// `libc::fork` rather than a thread: `unshare` affects the calling *process*, and there is no
/// way to unshare a namespace and then return.
fn entered_in_a_child() -> Result<(), Unavailable> {
    // SAFETY: between `fork` and `_exit` the child calls only async-signal-safe functions ---
    // `unshare` and `_exit` --- and never returns to Rust. Nothing is allocated, no lock is
    // taken, and no destructor runs.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(Unavailable::Refused {
            mechanism: "this process could not fork to test the boundary",
            said: std::io::Error::last_os_error().to_string(),
        });
    }
    if pid == 0 {
        // SAFETY: as above. The exit code carries the answer, because the child cannot allocate
        // a message and the parent needs only one bit.
        unsafe {
            let entered = libc::unshare(NAMESPACES);
            libc::_exit(if entered == 0 { 0 } else { 1 });
        }
    }
    let mut status: libc::c_int = 0;
    // SAFETY: `pid` is this process's own child and `status` is a live local.
    let waited = unsafe { libc::waitpid(pid, &raw mut status, 0) };
    if waited < 0 {
        return Err(Unavailable::Refused {
            mechanism: "this process could not wait for the child that tested the boundary",
            said: std::io::Error::last_os_error().to_string(),
        });
    }
    let exited_cleanly = libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0;
    if exited_cleanly {
        Ok(())
    } else {
        Err(Unavailable::Refused {
            mechanism: "this kernel would not let an unprivileged process enter a user, \
                        network and mount namespace",
            said: "unshare was refused".to_owned(),
        })
    }
}

/// Everything the child will need, built where allocating is safe.
#[derive(Debug)]
pub(crate) struct Plan {
    /// The tree the child pivots onto, which the parent creates and owns.
    jail: CString,
    /// Directories the child must create inside the jail before it can bind onto them,
    /// shallowest first.
    make: Vec<CString>,
    /// Source and destination for each read-only bind mount.
    binds: Vec<(CString, CString)>,
    /// The bounds, as the values `setrlimit` takes.
    limits: Vec<(libc::__rlimit_resource_t, libc::rlim_t)>,
    /// What to write to `/proc/self/uid_map` and `/proc/self/gid_map`, built here because the
    /// child cannot format a number without allocating.
    identity: Vec<u8>,
    /// The group half of the same, which must follow a refusal of `setgroups`.
    groups: Vec<u8>,
    /// Kept alive so the jail directory outlives the child.
    _root: tempfile::TempDir,
}

impl Plan {
    /// Work out the mounts and limits for one run.
    ///
    /// # Errors
    ///
    /// A path that cannot be represented as a C string, or a temporary directory that cannot
    /// be made.
    pub(crate) fn new(readable: &[&Path], bounds: &Bounds) -> std::io::Result<Self> {
        let root = tempfile::tempdir()?;
        let jail = c_string(root.path())?;

        let mut make = Vec::new();
        let mut binds = Vec::new();
        for path in readable {
            // The source is where the tree really is; the destination is where the program
            // expects to find it. They differ whenever a path is a symbolic link --- `/bin` is
            // `/usr/bin` on most modern systems --- and mounting at the canonical name only
            // produces a jail where `/bin/sh` does not exist, which the program discovers by
            // failing to start.
            let canonical = path.canonicalize()?;
            let inside = root.path().join(path.strip_prefix("/").unwrap_or(path));
            // Every ancestor, shallowest first. The child cannot walk a path and allocate the
            // pieces, so the pieces are cut here.
            let mut ancestors: Vec<&Path> = inside.ancestors().skip(1).collect();
            ancestors.reverse();
            for ancestor in ancestors {
                if ancestor.starts_with(root.path()) || ancestor == root.path() {
                    make.push(c_string(ancestor)?);
                }
            }
            make.push(c_string(&inside)?);
            binds.push((c_string(&canonical)?, c_string(&inside)?));
        }

        let limits = vec![
            (libc::RLIMIT_AS, bounds.memory),
            (libc::RLIMIT_CPU, bounds.cpu),
            // No file may be created at any size. `ADR-0022` Decision 3: a user function reads
            // and returns, and a function that could write would be a second writer --- which
            // every guarantee resting on one authoritative writer would then be conditional on.
            // Stated as a limit as well as by the read-only mounts, because two mechanisms that
            // must both fail is the point of defence in depth.
            (libc::RLIMIT_FSIZE, 0),
            // A bound on fan-out rather than a prohibition on forking, and the difference is
            // worth stating because it is not the one `ADR-0010` first reached for.
            //
            // *No subprocess* cannot be enforced with a `seccomp` filter installed here: this
            // code runs before `exec`, and the child must `exec` exactly once to become the
            // worker at all. A filter denying `execve` would deny that one. What actually
            // delivers the prohibition is the mount namespace above --- inside the jail there
            // is nothing to exec but the interpreter it was given, because nothing else
            // exists. This limit is what stops the interpreter forking a thousand copies of
            // itself, which is a resource question rather than an escape.
            (libc::RLIMIT_NPROC, 16),
        ];

        // The identity maps. Without them the process's user is *unmapped*, and the kernel
        // answers every operation that must translate one with `EOVERFLOW` --- which arrives
        // as `mkdir` failing with "value too large for defined data type" on a path fourteen
        // characters long, and is how the first version of this file spent an afternoon.
        //
        // Mapping the namespace's root to the caller's own id is what unprivileged user
        // namespaces offer: full capability *inside* the namespace, none outside it, and the
        // same unprivileged user as far as the rest of the machine is concerned.
        let identity = format!("0 {} 1\n", real_uid()).into_bytes();
        let groups = format!("0 {} 1\n", real_gid()).into_bytes();

        Ok(Self { jail, make, binds, limits, identity, groups, _root: root })
    }
}

/// Start the program behind the boundary.
///
/// # Errors
///
/// The process could not be started, which includes every way the boundary could not be applied
/// --- `pre_exec` returning an error makes the spawn fail rather than producing an unsandboxed
/// child.
pub(crate) fn spawn(program: &Path, arguments: &[&str], plan: &Plan) -> std::io::Result<Child> {
    use std::os::unix::process::CommandExt;

    let jail = plan.jail.clone();
    let make = plan.make.clone();
    let binds = plan.binds.clone();
    let limits = plan.limits.clone();
    let identity = plan.identity.clone();
    let groups = plan.groups.clone();

    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Nothing of this server's environment crosses. A worker inheriting the server's
        // environment inherits every credential anybody put in one.
        .env_clear();

    // SAFETY: the closure runs in the forked child before `exec`. It allocates nothing --- every
    // string it uses was built by `Plan::new` in the parent --- takes no lock, and calls only
    // syscalls. It returns rather than panicking, so a failure aborts the spawn instead of
    // producing a child outside the boundary.
    unsafe {
        command.pre_exec(move || {
            apply(&jail, &make, &binds, &limits, &identity, &groups)?;
            Ok(())
        });
    }
    command.spawn()
}

/// The child's half: namespaces, mounts, limits, privileges.
///
/// # Safety
///
/// Called only from `pre_exec`. Allocates nothing and calls only syscalls.
unsafe fn apply(
    jail: &CString,
    make: &[CString],
    binds: &[(CString, CString)],
    limits: &[(libc::__rlimit_resource_t, libc::rlim_t)],
    identity: &[u8],
    groups: &[u8],
) -> std::io::Result<()> {
    // 1. The namespaces, together. After this the process is unprivileged everywhere outside
    //    them and fully capable inside, which is what the mounts below need.
    if unsafe { libc::unshare(NAMESPACES) } != 0 {
        return Err(std::io::Error::last_os_error());
    }

    // 2. The identity maps, immediately, because until they are written this process's user
    //    is unmapped and **every operation that must translate a user id fails** --- with
    //    `EOVERFLOW`, which reads as "value too large for defined data type" and names nothing
    //    a reader would connect to a missing map.
    //
    //    `setgroups` is refused first. The kernel requires that of an unprivileged writer, and
    //    for a good reason: without it, mapping a group would be a way to *drop* a group whose
    //    membership denies access to something.
    if let Err(errno) = unsafe { write_file(c"/proc/self/setgroups", b"deny") } {
        return Err(std::io::Error::from_raw_os_error(errno));
    }
    if let Err(errno) = unsafe { write_file(c"/proc/self/uid_map", identity) } {
        return Err(std::io::Error::from_raw_os_error(errno));
    }
    if let Err(errno) = unsafe { write_file(c"/proc/self/gid_map", groups) } {
        return Err(std::io::Error::from_raw_os_error(errno));
    }

    // 3. No new privileges, before anything else can grant one. A `setuid` binary reached
    //    later cannot raise this process, whatever its bits say.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }

    // 3. Detach mount propagation. Without this every mount below would travel back out to the
    //    host, which is the opposite of the intent and is silent.
    let slash = c"/";
    if unsafe {
        libc::mount(
            std::ptr::null(),
            slash.as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }

    // 4. The jail becomes a mount point in its own right, which `pivot_root` requires.
    let tmpfs = c"tmpfs";
    if unsafe {
        libc::mount(
            tmpfs.as_ptr(),
            jail.as_ptr(),
            tmpfs.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }

    // 5. The directories the binds land on. `EEXIST` is not a failure: several allowed paths
    //    share ancestors, and the list is deliberately not deduplicated in the parent.
    for directory in make {
        let made = unsafe { libc::mkdir(directory.as_ptr(), 0o755) };
        if made != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(error);
            }
        }
    }

    // 6. The allowed tree, read-only. Two calls: a bind cannot be made read-only in the same
    //    `mount` that creates it, and a bind that is only *created* is as writable as its
    //    source.
    for (source, destination) in binds {
        if unsafe {
            libc::mount(
                source.as_ptr(),
                destination.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REC,
                std::ptr::null(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe {
            libc::mount(
                std::ptr::null(),
                destination.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_REC,
                std::ptr::null(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
    }

    // 7. Pivot onto the jail, and detach what was there. `pivot_root(".", ".")` rather than a
    //    second directory: the old root is stacked over the new one and immediately detached,
    //    which leaves nothing to unmount later and no directory an escape could walk into.
    if unsafe { libc::chdir(jail.as_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let here = c".";
    if unsafe {
        libc::syscall(libc::SYS_pivot_root, here.as_ptr(), here.as_ptr())
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::umount2(here.as_ptr(), libc::MNT_DETACH) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::chdir(slash.as_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }

    // 8. The bounds. Last, because everything above needs to allocate and map, and a limit set
    //    first would be a limit the setup itself trips over.
    for (resource, value) in limits {
        let limit = libc::rlimit { rlim_cur: *value, rlim_max: *value };
        if unsafe { libc::setrlimit(*resource, &raw const limit) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }

    Ok(())
}

/// This process's real user id.
fn real_uid() -> u32 {
    // SAFETY: `getuid` cannot fail and touches nothing.
    unsafe { libc::getuid() }
}

/// This process's real group id.
fn real_gid() -> u32 {
    // SAFETY: `getgid` cannot fail and touches nothing.
    unsafe { libc::getgid() }
}

/// Write one of the identity maps.
///
/// # Safety
///
/// Called only from `pre_exec`. Allocates nothing.
unsafe fn write_file(path: &std::ffi::CStr, contents: &[u8]) -> Result<(), i32> {
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
    }
    let written = unsafe {
        libc::write(fd, contents.as_ptr().cast::<libc::c_void>(), contents.len())
    };
    let failed = written < 0;
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    unsafe { libc::close(fd) };
    if failed {
        return Err(errno);
    }
    Ok(())
}

/// A path as a C string, or an error naming it.
fn c_string(path: &Path) -> std::io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("`{}` contains a zero byte and cannot be a path", path.display()),
        )
    })
}
