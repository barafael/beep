//! The deadlock fixed in `signal::receive` (ad5f40c), in small: a `watch::Ref` from `borrow()`
//! held across an `.await` while another task on the same thread calls `send_replace`. The send
//! waits for the watch's lock synchronously, so the thread never gets back to the task holding
//! the guard. b33p's session runs all its tasks on one thread (a `LocalSet`), as here.
#![cfg(feature = "signal")]

use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use tokio::sync::watch;

/// Stands in for b33p's `accepted`: anything that awaits.
async fn accepted(storage_read: bool) -> bool {
    tokio::task::yield_now().await;
    storage_read
}

/// Whether a reader and a writer of one watch, on one thread, finish within a second.
fn finishes(read_before_the_await: bool) -> bool {
    let (done, finished) = mpsc::channel();
    // On a thread of its own: a deadlocked thread can't time itself out.
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime, async move {
            let storage_read = Rc::new(watch::Sender::new(false));
            let writer = tokio::task::spawn_local({
                let storage_read = storage_read.clone();
                async move {
                    storage_read.send_replace(true);
                }
            });
            if read_before_the_await {
                let read = *storage_read.borrow();
                if accepted(read).await {}
            } else if accepted(*storage_read.borrow()).await {
                // The guard lives to the end of the condition, past the await.
            }
            writer.await.unwrap();
        });
        let _ = done.send(());
    });
    finished.recv_timeout(Duration::from_secs(1)).is_ok()
}

#[test]
fn a_guard_held_across_an_await_deadlocks() {
    assert!(!finishes(false));
}

#[test]
fn the_value_read_before_the_await_does_not() {
    assert!(finishes(true));
}
