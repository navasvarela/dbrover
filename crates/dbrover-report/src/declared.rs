//! A source's declared columns (`docs/reporting.md`, "Declared columns"): its rows, or its
//! documents, turned into the columns the file declares, each value checked against its type
//! and written in the type's plain form, as an output's declared column is.

use dbrover_core::{ResultShape, Row};
use dbrover_document::{Path, Step, Syntax, Value};
use dbrover_export::{ColumnType, ExportColumn};

use crate::{QuerySource, SourceColumn, SourceError, SourceOpener, SourceReader, SourceStatement};

/// A source's reader, its pages in the source's declared columns when it declares them, and
/// as they come when it doesn't.
pub(crate) struct Shaped<R> {
    inner: R,
    declared: Option<Declared>,
}

impl<R: SourceReader> Shaped<R> {
    /// Opens `statement` with `opener`, and finds where its declared columns are in what it
    /// returns.
    ///
    /// # Errors
    ///
    /// [`SourceError`] when the statement fails, a declared column comes from a result column
    /// the result doesn't have or from a path that isn't one, or the result is documents and
    /// the source declares no columns to take from them; the statement is closed first.
    pub(crate) async fn open<O: SourceOpener<Reader = R>>(
        opener: &O,
        statement: &SourceStatement<'_>,
    ) -> Result<Self, SourceError> {
        let inner = opener.open(statement).await?;
        match declared(&inner, statement.source) {
            Ok(declared) => Ok(Self { inner, declared }),
            Err(error) => {
                inner.close().await;
                Err(error)
            }
        }
    }
}

fn declared<R: SourceReader>(
    inner: &R,
    source: &QuerySource,
) -> Result<Option<Declared>, SourceError> {
    match (&source.columns, inner.shape()) {
        (Some(columns), shape) => Declared::new(columns, inner.columns(), shape).map(Some),
        (None, ResultShape::Rows) => Ok(None),
        (None, ResultShape::Documents) => Err(SourceError::new(
            "the query returns documents; declare the columns the source makes, as columns = \
             [{ name = \"symbol\", type = \"text\" }]",
        )),
    }
}

impl<R: SourceReader> SourceReader for Shaped<R> {
    fn columns(&self) -> &[ExportColumn] {
        match &self.declared {
            Some(declared) => &declared.columns,
            None => self.inner.columns(),
        }
    }

    async fn next_page(&mut self) -> Result<Option<Vec<Row>>, SourceError> {
        let page = self.inner.next_page().await?;
        match (&mut self.declared, page) {
            (Some(declared), Some(page)) => declared.convert(page).map(Some),
            (_, page) => Ok(page),
        }
    }

    async fn close(self) {
        self.inner.close().await;
    }
}

/// Where each declared column's values are, and how many items have been read.
struct Declared {
    declared: Vec<SourceColumn>,
    columns: Vec<ExportColumn>,
    places: Places,
    items: u64,
}

/// Where the values are in what the source returns.
enum Places {
    /// The index of each declared column's result column.
    Rows(Vec<usize>),
    /// Each declared column's path in a document.
    Documents(Vec<Path>),
}

/// A value as found, before it is checked against its column's type.
enum Found {
    /// Missing, or `null`: the column's default, if it has one.
    Nothing,
    Plain(String),
    /// An object or array, which only a text column takes, as its JSON.
    Structured(String),
}

impl Declared {
    fn new(
        declared: &[SourceColumn],
        result: &[ExportColumn],
        shape: ResultShape,
    ) -> Result<Self, SourceError> {
        let places = match shape {
            ResultShape::Rows => Places::Rows(
                declared
                    .iter()
                    .map(|column| result_column(column, result))
                    .collect::<Result<_, _>>()?,
            ),
            ResultShape::Documents => Places::Documents(
                declared
                    .iter()
                    .map(|column| {
                        column.from.parse::<Path>().map_err(|_| {
                            SourceError::new(format!(
                                "column {:?}: from = {:?} is not a path to a document's field, \
                                 as address.city or items[0].sku",
                                column.name, column.from
                            ))
                        })
                    })
                    .collect::<Result<_, _>>()?,
            ),
        };
        let columns = declared
            .iter()
            .map(|column| ExportColumn {
                name: column.name.clone(),
                type_name: Some(column.column_type.to_string()),
                kind: column.column_type.kind(),
                excel_format: None,
            })
            .collect();
        Ok(Self {
            declared: declared.to_vec(),
            columns,
            places,
            items: 0,
        })
    }

    fn convert(&mut self, page: Vec<Row>) -> Result<Vec<Row>, SourceError> {
        page.into_iter()
            .map(|row| {
                self.items += 1;
                self.convert_row(&row)
            })
            .collect()
    }

    fn convert_row(&self, row: &[Option<String>]) -> Result<Row, SourceError> {
        let document = match &self.places {
            Places::Rows(_) => None,
            Places::Documents(_) => {
                Some(document(row).map_err(|reason| self.item_error(None, &reason))?)
            }
        };
        self.declared
            .iter()
            .enumerate()
            .map(|(index, column)| {
                let found = match (&self.places, &document) {
                    (Places::Rows(indexes), _) => match row.get(indexes[index]) {
                        Some(Some(text)) => Found::Plain(text.clone()),
                        _ => Found::Nothing,
                    },
                    (Places::Documents(paths), Some(document)) => {
                        in_document(document, &paths[index])
                    }
                    (Places::Documents(_), None) => Found::Nothing,
                };
                cell(column, found).map_err(|reason| {
                    self.item_error(
                        document.as_ref(),
                        &format!("column {:?}: {reason}", column.name),
                    )
                })
            })
            .collect()
    }

    /// A problem with the item just read, naming it by its number in the source and, for a
    /// document with one, its `_id`.
    fn item_error(&self, document: Option<&Value>, reason: &str) -> SourceError {
        let id_path = Path::from(vec![Step::Field("_id".to_owned())]);
        let id = document
            .and_then(|document| document.at(&id_path))
            .and_then(|id| id.plain(Syntax::ExtendedJson))
            .map(|id| format!(" (_id {id:?})"))
            .unwrap_or_default();
        SourceError::new(format!("item {}{id}: {reason}", self.items))
    }
}

/// The index of the result's column `column` comes from: by its exact name, else ignoring
/// case, as SQL engines fold unquoted names differently.
fn result_column(column: &SourceColumn, result: &[ExportColumn]) -> Result<usize, SourceError> {
    result
        .iter()
        .position(|candidate| candidate.name == column.from)
        .or_else(|| {
            result
                .iter()
                .position(|candidate| candidate.name.eq_ignore_ascii_case(&column.from))
        })
        .ok_or_else(|| {
            let names: Vec<&str> = result.iter().map(|column| column.name.as_str()).collect();
            SourceError::new(format!(
                "column {:?}: the result has no column {:?}; it has {}",
                column.name,
                column.from,
                names.join(", ")
            ))
        })
}

/// A row of documents' one cell, read as Extended JSON.
fn document(row: &[Option<String>]) -> Result<Value, String> {
    let text = row.first().and_then(Option::as_deref).unwrap_or("null");
    Value::parse(text).map_err(|error| format!("the document can't be read: {error}"))
}

fn in_document(document: &Value, path: &Path) -> Found {
    match document.at(path) {
        None | Some(Value::Null) => Found::Nothing,
        Some(value) => match value.plain(Syntax::ExtendedJson) {
            Some(text) => Found::Plain(text),
            None => Found::Structured(value.to_json()),
        },
    }
}

/// The value `found` as `column` loads it: in its type's plain form, its default when it is
/// missing, or why it doesn't fit.
fn cell(column: &SourceColumn, found: Found) -> Result<Option<String>, String> {
    let text = match found {
        Found::Nothing => return Ok(column.default.clone()),
        Found::Structured(json) if column.column_type == ColumnType::Text => return Ok(Some(json)),
        Found::Structured(json) => {
            return Err(format!(
                "{json} is an object or an array, which only a text column takes"
            ));
        }
        Found::Plain(text) => text,
    };
    let text = match column.column_type {
        ColumnType::Timestamp => in_utc(&text).unwrap_or(text),
        _ => text,
    };
    let plain = column
        .column_type
        .plain_form(&text)
        .map_err(|reason| format!("{text:?} {reason}"))?;
    if column.column_type == ColumnType::Integer && plain.parse::<i64>().is_err() {
        return Err(format!(
            "{text:?} has more digits than an integer column holds (18); declare it \
             decimal(38,0)"
        ));
    }
    Ok(Some(plain))
}

/// A time with an offset, as `2026-10-01T12:00:00.000Z` or `…+02:00`, as the same time in
/// UTC without one; `None` for a time without an offset, or text that isn't a time.
fn in_utc(text: &str) -> Option<String> {
    let instant = text.replacen(' ', "T", 1).parse::<jiff::Timestamp>().ok()?;
    Some(
        instant
            .to_zoned(jiff::tz::TimeZone::UTC)
            .datetime()
            .to_string(),
    )
}
