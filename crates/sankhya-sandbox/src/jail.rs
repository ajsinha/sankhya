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

/// The namespace user id the worker ends up as.
///
/// **Not zero**, and that is the change. The map used to read `0 <server uid> 1`, which made the
/// worker `root` inside its namespace with a full capability set --- and `execve` of a
/// non-setuid file only drops capabilities when the effective user id is *not* zero. So the
/// interpreter started with every capability the namespace had.
///
/// `65534` is `nobody` on every distribution that ships one, and any non-zero value would do:
/// what matters is that it is not zero, so the capabilities go at `exec` and the worker runs
/// with none.
///
/// # What this does not fix
///
/// **Outside** the namespace the worker is still the server's own user, and `ADR-0023`
/// Decision 2 promised *"a distinct unprivileged uid and gid"*. An unprivileged user namespace
/// cannot deliver that: the kernel permits exactly one map line, and its parent-side id must be
/// the writer's own. A distinct id needs a `newuidmap` helper installed setuid and a
/// `/etc/subuid` range allocated to the server's user, which is a deployment decision this
/// process cannot make for itself. `SEC-09`, and §13.7a now says so rather than dropping the
/// row. What stops that mattering is the PID namespace below --- a process that cannot see the
/// server cannot signal it, whatever id it shares.
const NOBODY: u32 = 65_534;

/// Which mechanism was being applied when one refused.
///
/// A number rather than a message because the value crosses a `fork`: the child cannot allocate
/// a sentence, and an exit code is eight bits. The parent turns it back into words.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Step {
    /// `unshare` of the five namespaces.
    Namespaces = 1,
    /// Refusing `setgroups`, which the kernel requires before an unprivileged `gid_map`.
    Groups,
    /// The user id map.
    UserMap,
    /// The group id map.
    GroupMap,
    /// `PR_SET_NO_NEW_PRIVS`.
    NoNewPrivileges,
    /// Detaching mount propagation.
    Propagation,
    /// The `tmpfs` the jail is built on.
    Root,
    /// A directory a bind lands on.
    Directory,
    /// A file a bind lands on.
    File,
    /// A bind mount.
    Bind,
    /// Making a bind read-only.
    ReadOnly,
    /// `pivot_root` and the detach that follows it.
    Pivot,
    /// `setrlimit`.
    Bounds,
    /// The fork that puts the worker inside the PID namespace.
    Fork,
    /// `PR_SET_PDEATHSIG`.
    Tether,
}

impl Step {
    /// What to tell an operator this machine would not do.
    pub(crate) const fn mechanism(self) -> &'static str {
        match self {
            Self::Namespaces => "enter a user, network, mount, IPC and PID namespace",
            Self::Groups => "refuse `setgroups`, which an unprivileged group map requires",
            Self::UserMap => "write a user id map for the new namespace",
            Self::GroupMap => "write a group id map for the new namespace",
            Self::NoNewPrivileges => "set the no-new-privileges flag",
            Self::Propagation => "detach mount propagation from the host",
            Self::Root => "mount a tmpfs to build the jail on",
            Self::Directory => "make a directory inside the jail",
            Self::File => "make a file inside the jail for a bind to land on",
            Self::Bind => "bind a readable path into the jail",
            Self::ReadOnly => "make a bind mount read-only",
            Self::Pivot => "pivot onto the jail and detach the old root",
            Self::Bounds => "apply the resource limits",
            Self::Fork => "fork into the new PID namespace",
            Self::Tether => "tie the worker's life to the process that started it",
        }
    }

    /// The step an exit code names, or `None` for one this version did not write.
    pub(crate) const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Namespaces),
            2 => Some(Self::Groups),
            3 => Some(Self::UserMap),
            4 => Some(Self::GroupMap),
            5 => Some(Self::NoNewPrivileges),
            6 => Some(Self::Propagation),
            7 => Some(Self::Root),
            8 => Some(Self::Directory),
            9 => Some(Self::File),
            10 => Some(Self::Bind),
            11 => Some(Self::ReadOnly),
            12 => Some(Self::Pivot),
            13 => Some(Self::Bounds),
            14 => Some(Self::Fork),
            15 => Some(Self::Tether),
            _ => None,
        }
    }
}

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

/// Whether the boundary can be built on this machine, established by building one.
///
/// # Why this runs every step rather than one of them
///
/// `ADR-0023` Decision 3: *"The startup probe runs the mechanism, once, against a trivial
/// worker --- it does not read a capability flag and hope."* It used to fork, call `unshare`,
/// and stop. A machine where `unshare` succeeds and `pivot_root` fails --- which is every
/// machine with `/` mounted shared and no way to make it private, and every machine where the
/// jail's temporary directory is on a filesystem that cannot host a mount --- passed the probe
/// and failed at the first `CREATE AGGREGATION` in production. That is exactly the outcome the
/// decision exists to prevent. `SEC-14`.
///
/// It now calls [`apply`], the same function a real spawn calls, against an empty readable set.
/// There is no second implementation to drift.
///
/// Entered in a **child**, because `unshare(CLONE_NEWUSER)` is irreversible for the process that
/// calls it: probing in the server's own process would leave the server in a namespace with no
/// uid map, unable to read the files it serves.
pub(crate) fn probe() -> Result<(), Unavailable> {
    // Built in the parent, where allocating is safe --- the same rule the whole file follows.
    // Nothing readable: the boundary is what is being tested, not what can be seen through it.
    let plan = Plan::new(&[], &Bounds::modest()).map_err(|error| Unavailable::Refused {
        mechanism: "lay out a jail to test the boundary in",
        said: error.to_string(),
    })?;

    // A pipe carrying the errno back, because an exit code is eight bits and the step number
    // has already taken them. Written with one `write` in a child that may not allocate.
    let mut ends: [libc::c_int; 2] = [-1, -1];
    // SAFETY: `ends` is a live local of exactly the length `pipe` writes.
    if unsafe { libc::pipe(ends.as_mut_ptr()) } != 0 {
        return Err(Unavailable::Refused {
            mechanism: "open a pipe to hear how the boundary was refused",
            said: std::io::Error::last_os_error().to_string(),
        });
    }
    let (reader, writer) = (ends[0], ends[1]);

    // SAFETY: between `fork` and `_exit` the child calls only syscalls, allocates nothing, takes
    // no lock, runs no destructor, and never returns to Rust.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let said = std::io::Error::last_os_error().to_string();
        unsafe { libc::close(reader) };
        unsafe { libc::close(writer) };
        return Err(Unavailable::Refused {
            mechanism: "fork to test the boundary",
            said,
        });
    }
    if pid == 0 {
        // SAFETY: as above.
        unsafe {
            libc::close(reader);
            match apply(&plan.blueprint) {
                // Every mechanism applied, including the fork into the PID namespace --- this
                // process is the one inside it, and it has nothing to do but say so.
                Ok(()) => libc::_exit(0),
                Err((step, errno)) => {
                    let bytes = errno.to_ne_bytes();
                    libc::write(writer, bytes.as_ptr().cast::<libc::c_void>(), bytes.len());
                    libc::_exit(step as libc::c_int);
                }
            }
        }
    }

    // SAFETY: the write end is this process's own descriptor and is finished with.
    unsafe { libc::close(writer) };
    let mut errno = [0u8; 4];
    // SAFETY: `reader` is a live descriptor and `errno` is a live local of that length.
    let read = unsafe {
        libc::read(reader, errno.as_mut_ptr().cast::<libc::c_void>(), errno.len())
    };
    // SAFETY: finished with.
    unsafe { libc::close(reader) };

    let mut status: libc::c_int = 0;
    // SAFETY: `pid` is this process's own child and `status` is a live local.
    let waited = unsafe { libc::waitpid(pid, &raw mut status, 0) };
    if waited < 0 {
        return Err(Unavailable::Refused {
            mechanism: "wait for the child that tested the boundary",
            said: std::io::Error::last_os_error().to_string(),
        });
    }
    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
        return Ok(());
    }

    let code = u8::try_from(libc::WEXITSTATUS(status)).unwrap_or(0);
    let step = Step::from_code(code);
    let said = if read == 4 {
        std::io::Error::from_raw_os_error(i32::from_ne_bytes(errno)).to_string()
    } else {
        // The child died without saying why, which a signal does.
        "the child testing the boundary did not survive it".to_owned()
    };
    Err(Unavailable::Refused {
        mechanism: step.map_or(
            "apply the boundary --- and it did not say which part refused",
            Step::mechanism,
        ),
        said,
    })
}

/// Everything the child will need, built where allocating is safe.
///
/// Separate from [`Plan`] because the child's copy must not carry the temporary directory: the
/// directory has to be *dropped by the parent* once the run is over, and a handle to it in a
/// closure that crosses `fork` would be a second owner deciding when to remove it.
#[derive(Debug, Clone)]
pub(crate) struct Blueprint {
    /// The tree the child pivots onto, which the parent creates and owns.
    jail: CString,
    /// Directories the child must create inside the jail before it can bind onto them,
    /// shallowest first.
    make: Vec<CString>,
    /// Files the child must create inside the jail before it can bind onto them.
    ///
    /// A bind mount's target has to be the same kind of thing as its source, so a readable path
    /// that is a **file** needs an empty file to land on rather than a directory. That the
    /// sandbox could not bind a single file is why the interpreter used to be admitted by
    /// binding the directory it lives in --- which on this machine is `/usr/bin`. `SEC-11`.
    touch: Vec<CString>,
    /// Source and destination for each read-only bind mount.
    binds: Vec<(CString, CString)>,
    /// The bounds, as the values `setrlimit` takes.
    limits: Vec<(libc::__rlimit_resource_t, libc::rlim_t)>,
    /// What to write to `/proc/self/uid_map` and `/proc/self/gid_map`, built here because the
    /// child cannot format a number without allocating.
    identity: Vec<u8>,
    /// The group half of the same, which must follow a refusal of `setgroups`.
    groups: Vec<u8>,
}

/// A blueprint, and the directory it is laid out in.
#[derive(Debug)]
pub(crate) struct Plan {
    /// What the child will do.
    pub(crate) blueprint: Blueprint,
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
        let mut touch = Vec::new();
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
            // A file lands on a file and a directory on a directory. The kernel refuses the
            // mismatch, so getting this wrong is a spawn that fails rather than a jail that
            // silently holds the wrong thing --- but it is the difference between admitting one
            // interpreter and admitting every binary beside it.
            if canonical.is_file() {
                touch.push(c_string(&inside)?);
            } else {
                make.push(c_string(&inside)?);
            }
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
        // Mapped to `NOBODY` rather than to zero. An unprivileged user namespace can only
        // name the caller's own id on the parent side --- that part is the kernel's rule and
        // is why the worker is still the server's user outside --- but the id it takes
        // *inside* is ours to choose, and choosing zero made the worker namespace-root with a
        // full capability set that `execve` then had no reason to drop.
        let identity = format!("{NOBODY} {} 1\n", real_uid()).into_bytes();
        let groups = format!("{NOBODY} {} 1\n", real_gid()).into_bytes();

        Ok(Self {
            blueprint: Blueprint { jail, make, touch, binds, limits, identity, groups },
            _root: root,
        })
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

    let blueprint = plan.blueprint.clone();

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
            // The step is dropped here and the `errno` kept. Naming it would mean formatting a
            // sentence, and this is the one place in the crate that may not allocate --- the
            // probe is where the name is wanted, and the probe has a pipe to carry it.
            apply(&blueprint).map_err(|(_, errno)| std::io::Error::from_raw_os_error(errno))
        });
    }
    command.spawn()
}

/// The child's half: namespaces, mounts, limits, privileges, and the PID namespace.
///
/// Takes the whole [`Plan`] rather than six slices so that the probe and the spawn cannot pass
/// different things --- the probe exists to run *this*, and a probe that ran a subset would be
/// the thing `SEC-14` was.
///
/// Returns the step that refused and its `errno`, rather than an `std::io::Error`: building one
/// with a message allocates, and this runs in a forked child that must not.
///
/// # Safety
///
/// Called only between `fork` and `exec`. Allocates nothing and calls only syscalls.
unsafe fn apply(plan: &Blueprint) -> Result<(), (Step, i32)> {
    unsafe { establish(plan) }?;
    unsafe { enter_pid_namespace() }
}

/// Everything except the PID namespace, which needs a fork of its own.
///
/// # Safety
///
/// As [`apply`].
#[allow(clippy::too_many_lines)]
unsafe fn establish(plan: &Blueprint) -> Result<(), (Step, i32)> {
    let failed = |step: Step| -> (Step, i32) {
        (step, std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    };

    // 1. The namespaces, together. After this the process is unprivileged everywhere outside
    //    them and fully capable inside, which is what the mounts below need.
    if unsafe { libc::unshare(NAMESPACES) } != 0 {
        return Err(failed(Step::Namespaces));
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
        return Err((Step::Groups, errno));
    }
    if let Err(errno) = unsafe { write_file(c"/proc/self/uid_map", &plan.identity) } {
        return Err((Step::UserMap, errno));
    }
    if let Err(errno) = unsafe { write_file(c"/proc/self/gid_map", &plan.groups) } {
        return Err((Step::GroupMap, errno));
    }

    // 3. No new privileges, before anything else can grant one. A `setuid` binary reached
    //    later cannot raise this process, whatever its bits say.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(failed(Step::NoNewPrivileges));
    }

    // 4. Detach mount propagation. Without this every mount below would travel back out to the
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
        return Err(failed(Step::Propagation));
    }

    // 5. The jail becomes a mount point in its own right, which `pivot_root` requires.
    let tmpfs = c"tmpfs";
    if unsafe {
        libc::mount(
            tmpfs.as_ptr(),
            plan.jail.as_ptr(),
            tmpfs.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(failed(Step::Root));
    }

    // 6. The directories the binds land on. `EEXIST` is not a failure: several allowed paths
    //    share ancestors, and the list is deliberately not deduplicated in the parent.
    for directory in &plan.make {
        if unsafe { libc::mkdir(directory.as_ptr(), 0o755) } != 0 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if errno != libc::EEXIST {
                return Err((Step::Directory, errno));
            }
        }
    }

    // 7. And the files, for the readable paths that are files rather than directories. Created
    //    empty and immediately closed: the bind replaces the contents, and what matters is that
    //    the name exists and is of the right kind.
    for file in &plan.touch {
        let fd = unsafe { libc::open(file.as_ptr(), libc::O_CREAT | libc::O_WRONLY, 0o644) };
        if fd < 0 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if errno != libc::EEXIST {
                return Err((Step::File, errno));
            }
        } else {
            unsafe { libc::close(fd) };
        }
    }

    // 8. The allowed tree, read-only. Two calls: a bind cannot be made read-only in the same
    //    `mount` that creates it, and a bind that is only *created* is as writable as its
    //    source.
    for (source, destination) in &plan.binds {
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
            return Err(failed(Step::Bind));
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
            return Err(failed(Step::ReadOnly));
        }
    }

    // 9. Pivot onto the jail, and detach what was there. `pivot_root(".", ".")` rather than a
    //    second directory: the old root is stacked over the new one and immediately detached,
    //    which leaves nothing to unmount later and no directory an escape could walk into.
    if unsafe { libc::chdir(plan.jail.as_ptr()) } != 0 {
        return Err(failed(Step::Pivot));
    }
    let here = c".";
    if unsafe { libc::syscall(libc::SYS_pivot_root, here.as_ptr(), here.as_ptr()) } != 0 {
        return Err(failed(Step::Pivot));
    }
    if unsafe { libc::umount2(here.as_ptr(), libc::MNT_DETACH) } != 0 {
        return Err(failed(Step::Pivot));
    }
    if unsafe { libc::chdir(slash.as_ptr()) } != 0 {
        return Err(failed(Step::Pivot));
    }

    // 10. The bounds. Last, because everything above needs to allocate and map, and a limit set
    //     first would be a limit the setup itself trips over.
    for (resource, value) in &plan.limits {
        let limit = libc::rlimit { rlim_cur: *value, rlim_max: *value };
        if unsafe { libc::setrlimit(*resource, &raw const limit) } != 0 {
            return Err(failed(Step::Bounds));
        }
    }

    Ok(())
}

/// Become a process **inside** the PID namespace, rather than the one that made it.
///
/// # The failure this exists for
///
/// `unshare(CLONE_NEWPID)` places the caller's *children* in the new namespace and leaves the
/// caller behind. The caller here is the process that then `exec`s into the worker --- so the
/// worker was in the host PID namespace, could see every process on the machine, and, because
/// its credentials map to the server's own user, `os.kill(os.getppid(), 9)` worked. A user
/// function could stop the server. `SEC-10`.
///
/// So this forks once more. The child of that fork is PID 1 in the new namespace and becomes
/// the worker; the process that forked it stays outside, waits, and exits the way the worker
/// exited so the caller upstream sees the worker's own status and not this one's.
///
/// # Why the worker is tethered
///
/// Killing the intermediate --- which is what the deadline upstream does --- would otherwise
/// leave the worker running: it is PID 1 of its own namespace, so nothing reaps it and its
/// parent's death is not its own. `PR_SET_PDEATHSIG` makes it die with the process that started
/// it. There is a window between the fork and that call in which the intermediate could be
/// killed and the signal never armed; a worker orphaned in that window still ends at
/// `RLIMIT_CPU`, so the leak is bounded rather than absent.
///
/// # Safety
///
/// As [`apply`].
unsafe fn enter_pid_namespace() -> Result<(), (Step, i32)> {
    // SAFETY: between this `fork` and `_exit` neither side allocates, takes a lock, or runs a
    // destructor. The child returns to `pre_exec`, which is what it is for.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err((
            Step::Fork,
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        ));
    }
    if pid > 0 {
        // The intermediate. It holds the standard streams open, so it must not outlive the
        // worker by long --- and it does not: it exits the moment the worker does.
        //
        // **Everything above the standard streams is closed first**, and the reason is not
        // tidiness. `Command::spawn` reports a failed `exec` to the parent through a
        // close-on-exec pipe, and the parent's `spawn` call does not return until every copy
        // of that pipe's write end is closed. The worker's copy closes when it `exec`s; this
        // process's copy would not close until it exited --- which is when the worker exits.
        // So `spawn` blocked for the whole run, the deadline clock started after the worker
        // had already finished, and a function that never returned was reported as one that
        // failed. Closing them here is what makes the fork above invisible from outside.
        //
        // `close_range` where the kernel has it, and a bounded loop where it does not. Both
        // are async-signal-safe and neither allocates.
        // SAFETY: closing descriptors this process will not use again. It does nothing but
        // wait and exit.
        let ranged = unsafe { libc::syscall(libc::SYS_close_range, 3, libc::c_uint::MAX, 0) };
        if ranged != 0 {
            for descriptor in 3..1024 {
                // SAFETY: as above. Closing a descriptor that is not open is a no-op returning
                // `EBADF`, which is why the result is not checked.
                unsafe { libc::close(descriptor) };
            }
        }
        let mut status: libc::c_int = 0;
        loop {
            // SAFETY: `pid` is this process's own child and `status` is a live local.
            if unsafe { libc::waitpid(pid, &raw mut status, 0) } >= 0 {
                break;
            }
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                break;
            }
        }
        if libc::WIFSIGNALED(status) {
            // Re-raised rather than translated, so the caller upstream sees the signal that
            // ended the worker. Mapping it to an exit code would turn "killed by `SIGKILL`"
            // into a number, and the difference between a killed function and a failed one is
            // the whole of `Outcome`.
            let signal = libc::WTERMSIG(status);
            // SAFETY: restoring the default disposition and raising it on this process. This
            // process is about to end either way.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                libc::raise(signal);
            }
        }
        // SAFETY: `_exit` runs no destructor, which is the requirement here.
        unsafe { libc::_exit(libc::WEXITSTATUS(status)) };
    }

    // The worker. PID 1 in a namespace holding nothing else, so `getppid()` is 0 and there is
    // no process on this machine it can name.
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) } != 0 {
        return Err((
            Step::Tether,
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        ));
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
