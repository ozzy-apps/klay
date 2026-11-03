//! Long geometry operations on a worker thread (ZRemesher, Decimation, UV Unwrap, Remesh,
//! Make Boolean Mesh, ProjectAll): the thread they run on, and the Cancel flag an algorithm
//! reads between its phases with [`cancelled`].
//!
//! The app copies what the operation reads out of the active subtool, hands a closure to
//! [`spawn`], polls the receiver each frame and applies the result through the operation's
//! undo path. Cancel sets the flag and drops the receiver: whatever the thread still
//! produces is discarded, and an algorithm that checks the flag stops early.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::Arc;

thread_local! {
    /// The Cancel flag of the job this thread runs, if it runs one.
    static CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Whether the job running on this thread was cancelled. Always false off a job thread
/// (the algorithms called directly, in tests or at start-up), and on rayon's threads: check
/// it between phases, not inside a parallel loop.
pub fn cancelled() -> bool {
    CANCEL.with(|c| c.borrow().as_ref().is_some_and(|f| f.load(Ordering::Relaxed)))
}

/// What a job thread sends back: the result, or why it failed ("cancelled" for a job that
/// saw its flag).
pub type Done<T> = Result<T, String>;

/// Run `f` on a thread of its own with `cancel` as its flag. A panic in `f` comes back as
/// an error instead of taking the app down.
pub fn spawn<T: Send + 'static>(cancel: Arc<AtomicBool>, f: impl FnOnce() -> Done<T> + Send + 'static) -> Receiver<Done<T>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        CANCEL.with(|c| *c.borrow_mut() = Some(cancel));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|p| {
            let why = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            Err(if why.is_empty() { "internal error".to_string() } else { format!("internal error: {why}") })
        });
        let _ = tx.send(r);
    });
    rx
}

/// `Err("cancelled")` when the job was cancelled, to return from a phase boundary.
pub fn check() -> Result<(), String> {
    if cancelled() { Err("cancelled".into()) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_job_sees_its_flag_and_only_its_own_thread_does() {
        assert!(!cancelled(), "no job on the test thread");
        let flag = Arc::new(AtomicBool::new(false));
        let rx = spawn(flag.clone(), || {
            for _ in 0..200 {
                check()?;
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(1)
        });
        std::thread::sleep(Duration::from_millis(20));
        flag.store(true, Ordering::Relaxed);
        assert_eq!(rx.recv().unwrap(), Err("cancelled".to_string()));
        assert!(!cancelled(), "the flag is the job thread's alone");
        // A job that is not cancelled finishes with its value.
        let rx = spawn(Arc::new(AtomicBool::new(false)), || { check()?; Ok(2) });
        assert_eq!(rx.recv().unwrap(), Ok(2));
    }

    #[test]
    fn a_panic_on_the_job_thread_comes_back_as_an_error() {
        let rx = spawn::<u32>(Arc::new(AtomicBool::new(false)), || panic!("the mesh is odd"));
        assert_eq!(rx.recv().unwrap(), Err("internal error: the mesh is odd".to_string()));
        // Dropping the receiver before the job ends loses nothing but the result.
        let flag = Arc::new(AtomicBool::new(false));
        let rx = spawn(flag.clone(), || { std::thread::sleep(Duration::from_millis(10)); Ok(3) });
        drop(rx);
        std::thread::sleep(Duration::from_millis(30));
    }
}
