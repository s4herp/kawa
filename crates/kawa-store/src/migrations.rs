//! Embedded, numbered schema migrations tracked with `PRAGMA user_version`.

use rusqlite::Connection;

use crate::{Result, StoreError};

/// Applied in order; never edit an entry once released, add a new one.
pub(crate) const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_initial.sql"),
    include_str!("migrations/0002_developers.sql"),
    include_str!("migrations/0003_pr_window_start.sql"),
];

pub(crate) fn current_version(conn: &Connection) -> Result<usize> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    Ok(usize::try_from(version).unwrap_or(0))
}

pub(crate) fn apply(conn: &mut Connection) -> Result<()> {
    let current = current_version(conn)?;
    if current > MIGRATIONS.len() {
        return Err(StoreError::SchemaTooNew {
            found: current,
            supported: MIGRATIONS.len(),
        });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index + 1)?;
        tx.commit()?;
    }
    Ok(())
}
