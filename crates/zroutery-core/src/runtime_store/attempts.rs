//! Read immutable attempt identities for request diagnostics.

use crate::accounting::AttemptFact;

use super::RuntimeStore;

impl RuntimeStore {
    pub fn usage_attempts(&self, request_id: &str) -> Result<Vec<AttemptFact>, String> {
        if request_id.is_empty() || request_id.len() > 256 {
            return Err("invalid usage request id".into());
        }
        let db = crate::sync::lock(&self.db);
        let mut statement = db
            .prepare(
                "SELECT record FROM attempt_facts WHERE request_id = ?1 ORDER BY id LIMIT 1001",
            )
            .map_err(|error| error.to_string())?;
        let mut attempts = statement
            .query_map([request_id], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .map(|row| {
                serde_json::from_str::<AttemptFact>(&row.map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        if attempts.len() > 1000 {
            return Err("request attempt history exceeds the diagnostic limit".into());
        }
        attempts.sort_by(|a, b| {
            a.started_at
                .cmp(&b.started_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(attempts)
    }
}
