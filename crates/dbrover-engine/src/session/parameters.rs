//! A console's statements with `:name` parameters, made ready to send: bound where the driver
//! binds, written as literals where it doesn't, and as literals for the checks that must see
//! what runs.

use dbrover_core::{Connection, DriverError, ResultCursor};
use dbrover_sql::{Dialect, MissingValue, ParameterValues, Prepared};

use crate::RunPurpose;

/// One statement as it goes to the database.
#[derive(Debug, Clone)]
pub(super) struct Sendable {
    /// The statement with its values as literals, for the read-only check and the transaction
    /// rules, which must see what runs whichever way the values travel.
    pub checked: String,
    pub prepared: Prepared,
    /// The statement as the history keeps it: as typed, followed by a comment naming its
    /// values.
    pub recorded: String,
}

impl Sendable {
    /// `sql` ready to send with `values`. With no values at all, or no SQL dialect to read
    /// parameters in, the statement goes as typed, so a run the console didn't ask values for
    /// is never changed. Explaining always uses literals: SQL Server explains with a session
    /// setting that doesn't reach into `sp_executesql`.
    pub(super) fn new(
        sql: &str,
        dialect: Option<Dialect>,
        values: &ParameterValues,
        purpose: RunPurpose,
    ) -> Result<Self, MissingValue> {
        let Some(dialect) = dialect.filter(|_| !values.is_empty()) else {
            return Ok(Self::plain(sql));
        };
        let inlined = dbrover_sql::inline(sql, dialect, values)?;
        let prepared = match purpose {
            RunPurpose::Run => dbrover_sql::bind(sql, dialect, values)?,
            RunPurpose::Explain | RunPurpose::ExplainAnalyse => inlined.clone(),
        };
        let recorded = match dbrover_sql::values_comment(sql, dialect, values) {
            Some(comment) => format!("{sql}\n{comment}"),
            None => sql.to_owned(),
        };
        Ok(Self {
            checked: inlined.sql,
            prepared,
            recorded,
        })
    }

    /// A statement DbRover wrote itself, which has no parameters.
    pub(super) fn plain(sql: &str) -> Self {
        Self {
            checked: sql.to_owned(),
            prepared: Prepared::unchanged(sql),
            recorded: sql.to_owned(),
        }
    }

    /// Starts the statement on `connection`, binding its values if it has any.
    pub(super) async fn execute(
        &self,
        connection: &mut dyn Connection,
    ) -> Result<Box<dyn ResultCursor>, DriverError> {
        let Prepared { sql, values, .. } = &self.prepared;
        if values.is_empty() {
            connection.execute(sql).await
        } else {
            connection.execute_bound(sql, values).await
        }
    }

    /// Where in the statement as typed an error the database reported at `position` is.
    pub(super) fn typed_position(&self, position: Option<usize>) -> Option<usize> {
        position.map(|position| self.prepared.typed_offset(position))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_runs_explains_with_literals_and_leaves_runs_without_values_alone() {
        let values: ParameterValues = [("id", Some("7".to_owned()))].into_iter().collect();
        let sql = "SELECT * FROM t WHERE id = :id";

        let run = Sendable::new(sql, Some(Dialect::Sqlite), &values, RunPurpose::Run).unwrap();
        assert_eq!(run.prepared.sql, "SELECT * FROM t WHERE id = ?1");
        assert_eq!(run.checked, "SELECT * FROM t WHERE id = '7'");
        assert_eq!(run.recorded, format!("{sql}\n-- :id = '7'"));

        let explain =
            Sendable::new(sql, Some(Dialect::Sqlite), &values, RunPurpose::Explain).unwrap();
        assert_eq!(explain.prepared.sql, "SELECT * FROM t WHERE id = '7'");
        assert!(explain.prepared.values.is_empty());

        let untouched = Sendable::new(
            sql,
            Some(Dialect::Sqlite),
            &ParameterValues::default(),
            RunPurpose::Run,
        )
        .unwrap();
        assert_eq!(untouched.prepared.sql, sql);
        assert_eq!(untouched.recorded, sql);

        assert!(
            Sendable::new(
                "SELECT :other",
                Some(Dialect::Sqlite),
                &values,
                RunPurpose::Run
            )
            .is_err()
        );
    }
}
