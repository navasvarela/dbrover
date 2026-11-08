//! **Open data** on a table for free (`docs/cloud-data.md`, C4f). BigQuery bills a
//! `SELECT * … LIMIT 1000` for every column of the table, whole, whatever the `LIMIT`; its
//! `tabledata.list` reads the same rows for nothing, as its console's **Preview** does. So the
//! driver answers the statement the explorer writes, ``SELECT * FROM `p.d.t` LIMIT n``, in
//! exactly that shape and on a table, from `tabledata.list`; anything else, a view's rows
//! included, is a query.

use async_trait::async_trait;
use dbrover_core::{ByteCount, ColumnInfo, DriverError, JobProgress, ResultCursor, Row};
use serde::Deserialize;

use crate::api::{Api, segment};
use crate::errors;
use crate::results::{self, ApiRow, Schema};

/// A table the explorer's statement names, and how many of its rows it asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Preview {
    pub project: String,
    pub dataset: String,
    pub table: String,
    pub rows: usize,
}

impl Preview {
    /// The table and row count of ``SELECT * FROM `project.dataset.table` LIMIT n``, written as
    /// `dbrover-language` writes it for **Open data** (any case, a semicolon or not); `None`
    /// for any other statement.
    pub(crate) fn recognise(statement: &str) -> Option<Self> {
        let statement = statement.trim().trim_end_matches(';').trim_end();
        let rest = strip_prefix_ignoring_case(statement, "SELECT * FROM `")?;
        let (path, rest) = quoted_path(rest)?;
        let rows = strip_prefix_ignoring_case(rest, " LIMIT ")?;
        if rows.is_empty() || !rows.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let mut parts = path.rsplitn(3, '.');
        let table = parts.next()?;
        let dataset = parts.next()?;
        let project = parts.next()?;
        Some(Self {
            project: project.to_owned(),
            dataset: dataset.to_owned(),
            table: table.to_owned(),
            rows: rows.parse().ok()?,
        })
    }
}

fn strip_prefix_ignoring_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// The path inside backquotes, its backslash escapes undone, and what follows the closing one.
fn quoted_path(text: &str) -> Option<(String, &str)> {
    let mut path = String::new();
    let mut chars = text.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => path.push(chars.next()?.1),
            '`' => return Some((path, &text[at + 1..])),
            c => path.push(c),
        }
    }
    None
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TableData {
    #[serde(default)]
    rows: Vec<ApiRow>,
    page_token: Option<String>,
}

/// A table's rows read from `tabledata.list`, a page as the console asks for one, up to the
/// statement's `LIMIT`.
pub(crate) struct PreviewCursor {
    api: Api,
    path: String,
    schema: Schema,
    columns: Vec<ColumnInfo>,
    /// Rows still to be handed out before the `LIMIT` is reached.
    left: usize,
    /// Where the next page starts; `None` once the table has no more, or before the first.
    next: Option<String>,
    started: bool,
}

impl PreviewCursor {
    pub(crate) fn new(api: Api, preview: &Preview, schema: Schema) -> Self {
        Self {
            api,
            path: format!(
                "/projects/{}/datasets/{}/tables/{}/data",
                segment(&preview.project),
                segment(&preview.dataset),
                segment(&preview.table)
            ),
            columns: schema.fields.iter().map(results::Field::column).collect(),
            schema,
            left: preview.rows,
            next: None,
            started: false,
        }
    }
}

#[async_trait]
impl ResultCursor for PreviewCursor {
    fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    async fn next_page(&mut self, max_rows: usize) -> Result<Vec<Row>, DriverError> {
        let mut rows = Vec::new();
        while !self.is_exhausted() && rows.len() < max_rows {
            let wanted = (max_rows - rows.len()).min(self.left).to_string();
            let mut query = vec![
                ("maxResults", wanted.as_str()),
                ("formatOptions.useInt64Timestamp", "true"),
            ];
            if let Some(token) = &self.next {
                query.push(("pageToken", token));
            }
            let data: TableData = self
                .api
                .get(&self.path, &query)
                .await
                .map_err(errors::connection)?;
            self.started = true;
            let mut page = results::rows(&self.schema, &data.rows);
            page.truncate(self.left);
            self.left -= page.len();
            self.next = data.page_token.filter(|_| !page.is_empty());
            rows.extend(page);
        }
        Ok(rows)
    }

    fn is_exhausted(&self) -> bool {
        self.left == 0 || (self.started && self.next.is_none())
    }

    /// Nothing scanned: `tabledata.list` isn't billed.
    fn progress(&self) -> Option<JobProgress> {
        Some(JobProgress {
            rows_read: None,
            bytes_read: Some(ByteCount(0)),
        })
    }

    async fn finish(self: Box<Self>) -> Result<Option<u64>, DriverError> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_explorers_own_statement_is_a_preview() {
        let orders = |project: &str, table: &str, rows| {
            Some(Preview {
                project: project.to_owned(),
                dataset: "sales".to_owned(),
                table: table.to_owned(),
                rows,
            })
        };
        for (statement, preview) in [
            (
                "SELECT * FROM `my-project.sales.orders` LIMIT 1000",
                orders("my-project", "orders", 1000),
            ),
            (
                "  select * from `my-project.sales.orders` limit 10;\n",
                orders("my-project", "orders", 10),
            ),
            (
                "SELECT * FROM `google.com:analytics.sales.it\\`s` LIMIT 5",
                orders("google.com:analytics", "it`s", 5),
            ),
            ("SELECT * FROM `my-project.sales.orders`", None),
            (
                "SELECT * FROM `my-project.sales.orders` WHERE id = 1 LIMIT 5",
                None,
            ),
            (
                "SELECT * FROM `my-project.sales.orders` ORDER BY id LIMIT 5",
                None,
            ),
            ("SELECT id FROM `my-project.sales.orders` LIMIT 5", None),
            ("SELECT * FROM `sales.orders` LIMIT 5", None),
            (
                "SELECT * FROM `my-project.sales.orders` LIMIT 5 OFFSET 2",
                None,
            ),
            ("SELECT * FROM `my-project.sales.orders` LIMIT -5", None),
        ] {
            assert_eq!(Preview::recognise(statement), preview, "{statement}");
        }
    }
}
