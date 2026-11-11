//! Schema migrations, tracked with `user_version`.
//!
//! Each migration is append-only and runs inside a transaction. Adding a
//! migration means pushing to `MIGRATIONS`; never edit an existing one,
//! because installed copies have already applied it.

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Index = target version. `MIGRATIONS[0]` takes the DB from 0 to 1.
const MIGRATIONS: &[&str] = &[
    include_str!("schema.sql"),
    include_str!("schema_v2.sql"),
    include_str!("schema_v3.sql"),
    include_str!("schema_v4.sql"),
    include_str!("schema_v5.sql"),
    include_str!("schema_v6.sql"),
    include_str!("schema_v7.sql"),
];

pub fn run(conn: &mut Connection) -> Result<()> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let target = MIGRATIONS.len() as u32;

    if current > target {
        anyhow::bail!(
            "store is at schema v{current}, this build understands v{target}; \
             a newer MunkiMail wrote this database"
        );
    }

    for v in current..target {
        let tx = conn.transaction()?;
        tx.execute_batch(MIGRATIONS[v as usize])
            .with_context(|| format!("applying migration to v{}", v + 1))?;
        // Not bindable as a parameter; v is a loop index, not user input.
        tx.pragma_update(None, "user_version", v + 1)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrating_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn).unwrap();
        // Re-running on an up-to-date DB must be a no-op, not an error:
        // every app start calls this.
        run(&mut conn).unwrap();
        let v: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, MIGRATIONS.len() as u32);
    }

    #[test]
    fn refuses_a_future_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 99u32).unwrap();
        let err = run(&mut conn).unwrap_err().to_string();
        assert!(err.contains("newer MunkiMail"), "{err}");
    }
}
