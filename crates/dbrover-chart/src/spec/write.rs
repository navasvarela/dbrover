//! Writing the chart tab's choice back into a spec's text.

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value};

use super::{Key, SPEC_TABLE};
use crate::ChartKind;

/// What the chart tab's toolbar sets, by the columns' names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedChoice {
    pub kind: ChartKind,
    pub x: String,
    pub y: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WriteError {
    /// The spec's text isn't TOML, so there is nothing to keep around the keys.
    #[error("the chart's text isn't TOML: {0}")]
    Unreadable(String),
    /// `[chart]` holds something other than a table of keys.
    #[error("`{SPEC_TABLE}` isn't a table of keys")]
    NotATable,
}

/// `text`, a spec's text, with `saved`'s `kind`, `x` and `y` written in it; or a new spec of
/// those keys when `text` is `None`.
///
/// Every key is written, even one the spec left to the starting choice, so a later change of
/// the result's columns can't change the chart silently. A key whose value is already the one
/// saved is left as it is, and every other key, comment and blank line is kept: a key that
/// changes keeps its place and the comment after it.
///
/// # Errors
///
/// [`WriteError`] when `text` isn't TOML, or its `chart` isn't a table.
pub fn write_back(text: Option<&str>, saved: &SavedChoice) -> Result<String, WriteError> {
    let mut document = match text {
        Some(text) => text
            .parse::<DocumentMut>()
            .map_err(|error| WriteError::Unreadable(error.message().trim().to_owned()))?,
        None => DocumentMut::new(),
    };
    let table = document
        .entry(SPEC_TABLE)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or(WriteError::NotATable)?;
    set(table, Key::Kind, Value::from(saved.kind.spec_name()));
    set(table, Key::X, Value::from(saved.x.as_str()));
    let y = match saved.y.as_slice() {
        [only] => Value::from(only.as_str()),
        names => Value::Array(names.iter().map(String::as_str).collect::<Array>()),
    };
    set(table, Key::Y, y);
    Ok(document.to_string())
}

/// `text`, a spec's text, as a report's `[[chart]]` (CH3), writing `file` first: the form
/// **Copy for a report** puts on the clipboard. Every key and comment of the spec is kept.
///
/// # Errors
///
/// [`WriteError`] when `text` isn't TOML, or its `chart` isn't a table.
pub fn for_report(text: &str, file: &str) -> Result<String, WriteError> {
    let mut document = text
        .parse::<DocumentMut>()
        .map_err(|error| WriteError::Unreadable(error.message().trim().to_owned()))?;
    let spec = match document.remove(SPEC_TABLE) {
        Some(Item::Table(spec)) => spec,
        None => Table::new(),
        Some(_) => return Err(WriteError::NotATable),
    };
    let mut chart = Table::new();
    chart.insert("file", Item::Value(Value::from(file)));
    for (key, item) in spec.iter() {
        if let Some(key) = spec.key(key) {
            chart.insert_formatted(key, item.clone());
        }
    }
    let mut charts = ArrayOfTables::new();
    charts.push(chart);
    document.insert(SPEC_TABLE, Item::ArrayOfTables(charts));
    Ok(document.to_string())
}

/// Sets `key` to `value`, unless it holds that value already, keeping the spaces and comment
/// around the old value.
fn set(table: &mut Table, key: Key, mut value: Value) {
    let Some(Item::Value(old)) = table.get_mut(key.name()) else {
        table.insert(key.name(), Item::Value(value));
        return;
    };
    if same(old, &value) {
        return;
    }
    *value.decor_mut() = old.decor().clone();
    *old = value;
}

/// Whether two values are equal as values, whatever their quotes and spaces.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same(a, b))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::ChartSpec;

    fn saved(kind: ChartKind, x: &str, y: &[&str]) -> SavedChoice {
        SavedChoice {
            kind,
            x: x.to_owned(),
            y: y.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    #[test]
    fn rewrites_only_what_changed_keeping_comments() {
        let text = "[chart]\n# Net sales by month.\nkind = \"bars\"  # bars read best\nx = 'month'\ny = \"net\"\ntitle = \"Net sales\"\n";
        let written = write_back(Some(text), &saved(ChartKind::Lines, "month", &["net"]));
        assert_eq!(
            written.as_deref(),
            Ok(
                "[chart]\n# Net sales by month.\nkind = \"lines\"  # bars read best\nx = 'month'\ny = \"net\"\ntitle = \"Net sales\"\n"
            )
        );
    }

    #[test]
    fn writes_the_keys_left_to_the_starting_choice() {
        let text = "[chart]\ntitle = \"Net sales\"\n";
        let written = write_back(
            Some(text),
            &saved(ChartKind::Bars, "month", &["net", "orders"]),
        )
        .unwrap();
        assert_eq!(
            written,
            "[chart]\ntitle = \"Net sales\"\nkind = \"bars\"\nx = \"month\"\ny = [\"net\", \"orders\"]\n"
        );
    }

    #[test]
    fn a_statement_without_a_spec_gets_one() {
        let written = write_back(None, &saved(ChartKind::Bars, "month", &["net"])).unwrap();
        assert_eq!(
            written,
            "[chart]\nkind = \"bars\"\nx = \"month\"\ny = \"net\"\n"
        );
    }

    #[test]
    fn a_written_spec_reads_back_as_saved() {
        let cases = [
            (None, saved(ChartKind::Scatter, "net", &["orders"])),
            (
                Some("[chart]\ny = [\"a\", \"b\"]\nlimit = 3\n"),
                saved(ChartKind::Lines, "month", &["b", "a"]),
            ),
            (
                Some("[chart]\ny = [\"a\", \"b\"]\n"),
                saved(ChartKind::Lines, "month", &["a"]),
            ),
        ];
        for (text, choice) in cases {
            let written = write_back(text, &choice).unwrap();
            let spec = ChartSpec::read(&written).unwrap();
            assert_eq!(spec.kind, Some(choice.kind), "{written}");
            assert_eq!(spec.x.as_deref(), Some(choice.x.as_str()), "{written}");
            assert_eq!(spec.y, choice.y, "{written}");
        }
    }

    #[test]
    fn copies_a_spec_as_a_reports_chart() {
        let text =
            "[chart]\n# Net sales by month.\nkind = \"bars\"  # bars read best\nx = \"month\"\n";
        assert_eq!(
            for_report(text, "net by month.svg").as_deref(),
            Ok(
                "[[chart]]\nfile = \"net by month.svg\"\n# Net sales by month.\nkind = \"bars\"  # bars read best\nx = \"month\"\n"
            )
        );
        assert_eq!(
            for_report("[chart]\n", "a \"quoted\" name.svg").as_deref(),
            Ok("[[chart]]\nfile = 'a \"quoted\" name.svg'\n")
        );
    }

    #[test]
    fn refuses_text_it_cant_keep() {
        let choice = saved(ChartKind::Bars, "month", &["net"]);
        assert!(matches!(
            write_back(Some("[chart]\nkind = "), &choice),
            Err(WriteError::Unreadable(_))
        ));
        assert_eq!(
            write_back(Some("chart = 1"), &choice),
            Err(WriteError::NotATable)
        );
        assert_eq!(for_report("chart = 1", "a.svg"), Err(WriteError::NotATable));
    }
}
