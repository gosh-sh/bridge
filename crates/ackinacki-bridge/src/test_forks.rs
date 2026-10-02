//! Keeps the tests that start subprocesses apart from the tests that take
//! a file lock again after releasing it.
//!
//! Starting a subprocess forks this process, and the child holds a copy of
//! every descriptor the process has open until its `exec` closes them. In
//! this test binary that takes milliseconds: the child first copies an
//! address space of more than a gigabyte, and `exec` tears it down again
//! before it closes anything. A `flock` belongs to the open file, not to
//! one descriptor of it, so a lock that one test releases while another
//! test's child is in that window stays held, and the test that takes it
//! again at once is told another process holds it. A tool a test is still
//! writing is held open for writing the same way, and the kernel refuses
//! to run it (ETXTBSY) until that child has gone through `exec`.
//!
//! So a test that starts subprocesses holds [`spawning`] and runs while no
//! other test holds either guard; a fake prover directory holds it for its
//! test (`testkit::fake_prover_dir`). A test that releases a lock and takes
//! it again holds [`relocking`] and runs alongside the other tests that do.
//! A test that does both takes [`spawning`]. The thread that holds
//! [`spawning`] may take it again, and its [`relocking`] is one more hold
//! on [`spawning`].
#![cfg(test)]

use std::{
    sync::{Condvar, Mutex, MutexGuard, PoisonError},
    thread::ThreadId,
};

/// Who holds the guards.
struct Holders {
    /// The thread of the test that may start subprocesses, and how many
    /// holds it has.
    spawning: Option<(ThreadId, usize)>,
    /// The threads of the tests that hold [`relocking`], once per hold.
    relocking: Vec<ThreadId>,
}

/// The holders, shared by every test thread.
static HOLDERS: Mutex<Holders> = Mutex::new(Holders {
    spawning: None,
    relocking: Vec::new(),
});

/// Signalled whenever a hold is released.
static RELEASED: Condvar = Condvar::new();

/// Locks [`HOLDERS`]. Nothing panics while it is locked, so a poisoned
/// lock only means a test failed elsewhere.
fn holders() -> MutexGuard<'static, Holders> {
    HOLDERS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Waits for [`RELEASED`] with `h` unlocked meanwhile.
fn wait(h: MutexGuard<'static, Holders>) -> MutexGuard<'static, Holders> {
    RELEASED.wait(h).unwrap_or_else(PoisonError::into_inner)
}

/// A hold on one of the two guards, released when it is dropped.
#[must_use = "the guard is released as soon as its hold is dropped"]
pub struct Hold(Kind);

/// What a [`Hold`] holds.
enum Kind {
    /// One hold on [`spawning`].
    Spawning,
    /// One hold on [`relocking`], taken on this thread.
    Relocking(ThreadId),
}

/// For a test that starts subprocesses: waits until no other test holds
/// either guard, and keeps them out until the hold is dropped.
pub fn spawning() -> Hold {
    let me = std::thread::current().id();
    let relocking = holders().relocking.contains(&me);
    assert!(
        !relocking,
        "this test holds test_forks::relocking and now asks for spawning, which would wait for \
         itself; a test that does both takes spawning alone"
    );
    let mut h = holders();
    loop {
        let s = &mut *h;
        match s.spawning {
            Some((id, ref mut n)) if id == me => *n += 1,
            None if s.relocking.is_empty() => s.spawning = Some((me, 1)),
            _ => {
                h = wait(h);
                continue;
            },
        }
        return Hold(Kind::Spawning);
    }
}

/// For a test that releases a lock and takes it again: waits until no
/// test holds [`spawning`] (except this one's own thread), and keeps such
/// tests out until the hold is dropped.
pub fn relocking() -> Hold {
    let me = std::thread::current().id();
    let mut h = holders();
    loop {
        match h.spawning {
            Some((id, ref mut n)) if id == me => {
                *n += 1;
                return Hold(Kind::Spawning);
            },
            None => {
                h.relocking.push(me);
                return Hold(Kind::Relocking(me));
            },
            Some(_) => h = wait(h),
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        let mut h = holders();
        match self.0 {
            Kind::Spawning => {
                if let Some((_, n)) = &mut h.spawning {
                    *n -= 1;
                    if *n == 0 {
                        h.spawning = None;
                    }
                }
            },
            Kind::Relocking(id) => {
                if let Some(i) = h.relocking.iter().position(|t| *t == id) {
                    h.relocking.swap_remove(i);
                }
            },
        }
        drop(h);
        RELEASED.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spawning_thread_may_take_both_guards_again() {
        let me = std::thread::current().id();
        let first = spawning();
        let covered = relocking();
        let again = spawning();
        assert_eq!(holders().spawning, Some((me, 3)));
        assert!(!holders().relocking.contains(&me));
        drop((first, covered));
        assert_eq!(holders().spawning, Some((me, 1)));
        drop(again);
        // Free, or already taken by a test that was waiting for it.
        assert_ne!(holders().spawning.map(|(id, _)| id), Some(me));
    }

    #[test]
    #[should_panic(expected = "takes spawning alone")]
    fn asking_for_spawning_while_relocking_fails_instead_of_waiting_for_itself() {
        let _relocking = relocking();
        let _spawning = spawning();
    }
}
