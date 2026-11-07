//! Request-scoped import cancellation, independent of the runtime database lock.

use std::collections::BTreeMap;
use std::sync::Mutex;

use zroutery_core::history_import::ImportCancel;

#[derive(Default)]
pub(crate) struct HistoryImports(Mutex<BTreeMap<String, Job>>);

struct Job {
    cancel: ImportCancel,
    running: bool,
    touched: std::time::Instant,
}

pub(crate) struct Operation<'a> {
    jobs: &'a HistoryImports,
    id: String,
    pub cancel: ImportCancel,
}

impl HistoryImports {
    pub fn begin(&self, id: &str) -> Result<Operation<'_>, String> {
        uuid::Uuid::parse_str(id).map_err(|_| "invalid import run id")?;
        let mut jobs = self.0.lock().unwrap_or_else(|error| error.into_inner());
        jobs.retain(|_, job| {
            job.running || job.touched.elapsed() < std::time::Duration::from_secs(60)
        });
        if jobs.get(id).is_some_and(|job| job.running) {
            return Err("this import is already running".into());
        }
        if !jobs.contains_key(id) && jobs.len() >= 128 {
            return Err("too many pending history import operations".into());
        }
        let job = jobs.entry(id.into()).or_insert_with(|| Job {
            cancel: ImportCancel::new(),
            running: false,
            touched: std::time::Instant::now(),
        });
        job.running = true;
        Ok(Operation {
            jobs: self,
            id: id.into(),
            cancel: job.cancel.clone(),
        })
    }

    /// Remember cancellation even when IPC scheduling delivers it before begin.
    pub fn cancel(&self, id: &str) -> Result<bool, String> {
        uuid::Uuid::parse_str(id).map_err(|_| "invalid import run id")?;
        let mut jobs = self.0.lock().unwrap_or_else(|error| error.into_inner());
        jobs.retain(|_, job| {
            job.running || job.touched.elapsed() < std::time::Duration::from_secs(60)
        });
        if !jobs.contains_key(id) && jobs.len() >= 128 {
            return Err("too many pending history import operations".into());
        }
        let job = jobs.entry(id.into()).or_insert_with(|| Job {
            cancel: ImportCancel::new(),
            running: false,
            touched: std::time::Instant::now(),
        });
        Ok(job.cancel.cancel())
    }
}

impl Drop for Operation<'_> {
    fn drop(&mut self) {
        self.jobs
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_before_begin_is_not_lost_and_jobs_do_not_overlap() {
        let jobs = HistoryImports::default();
        let id = uuid::Uuid::new_v4().to_string();
        assert!(jobs.cancel(&id).unwrap());
        let operation = jobs.begin(&id).unwrap();
        assert!(operation.cancel.is_cancelled());
        assert!(jobs.begin(&id).is_err());
        drop(operation);
        assert!(!jobs.begin(&id).unwrap().cancel.is_cancelled());
    }
}
