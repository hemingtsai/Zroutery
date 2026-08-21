//! Lock helpers that survive a poisoned lock.
//!
//! A panic while a `Mutex` is held would otherwise poison it and turn every
//! later request into a panic as well. None of the state behind these locks is
//! sensitive to a half-finished update: the request log, the health map and the
//! configuration snapshot are each replaced wholesale. Recovering the guard is
//! therefore strictly better than taking the whole proxy down.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("recovering a poisoned mutex");
        poisoned.into_inner()
    })
}

pub(crate) fn read<T>(l: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|poisoned| {
        tracing::warn!("recovering a poisoned lock for reading");
        poisoned.into_inner()
    })
}

pub(crate) fn write<T>(l: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(|poisoned| {
        tracing::warn!("recovering a poisoned lock for writing");
        poisoned.into_inner()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_poisoned_mutex_still_hands_out_the_value() {
        let m = Arc::new(Mutex::new(vec![1, 2, 3]));
        let clone = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let _guard = clone.lock().unwrap();
            panic!("poison it");
        })
        .join();
        assert!(m.lock().is_err(), "the lock really is poisoned");
        assert_eq!(lock(&m).len(), 3);
        lock(&m).push(4);
        assert_eq!(lock(&m).len(), 4);
    }

    #[test]
    fn a_poisoned_rwlock_still_reads_and_writes() {
        let l = Arc::new(RwLock::new(String::from("value")));
        let clone = Arc::clone(&l);
        let _ = std::thread::spawn(move || {
            let _guard = clone.write().unwrap();
            panic!("poison it");
        })
        .join();
        assert!(l.read().is_err());
        assert_eq!(*read(&l), "value");
        write(&l).push('!');
        assert_eq!(*read(&l), "value!");
    }
}
