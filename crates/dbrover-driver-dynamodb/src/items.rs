//! DynamoDB's items as documents (ADR 0006): each attribute as Extended JSON, with DynamoDB's
//! sets as DbRover's own `$stringSet`, `$numberSet` and `$binarySet`.

use std::collections::HashMap;

use aws_sdk_dynamodb::primitives::Blob;
use aws_sdk_dynamodb::types::AttributeValue;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use dbrover_document::Value;

/// The BSON subtype `$binary` is written with for DynamoDB's binary values: generic data.
const GENERIC_SUBTYPE: &str = "00";

/// `item` as a document, the attributes named in `first` leading in that order (a table's
/// keys), then the rest by name: DynamoDB keeps no order of its own.
pub(crate) fn document(item: &HashMap<String, AttributeValue>, first: &[String]) -> Value {
    let mut attributes: Vec<(&String, &AttributeValue)> = item.iter().collect();
    attributes.sort_by_key(|(name, _)| {
        let rank = first.iter().position(|key| key == *name);
        (rank.unwrap_or(usize::MAX), *name)
    });
    Value::Object(
        attributes
            .into_iter()
            .map(|(name, attribute)| (name.clone(), value(attribute)))
            .collect(),
    )
}

/// One attribute's value. A type the SDK doesn't know yet shows as an object naming it, so it
/// is seen rather than lost.
pub(crate) fn value(attribute: &AttributeValue) -> Value {
    match attribute {
        AttributeValue::S(text) => Value::String(text.clone()),
        AttributeValue::N(number) => Value::Number(number.clone()),
        AttributeValue::Bool(flag) => Value::Bool(*flag),
        AttributeValue::Null(_) => Value::Null,
        AttributeValue::B(blob) => binary(blob),
        AttributeValue::L(items) => Value::Array(items.iter().map(value).collect()),
        AttributeValue::M(fields) => document(fields, &[]),
        AttributeValue::Ss(items) => set("$stringSet", items.iter().cloned().map(Value::String)),
        AttributeValue::Ns(items) => set("$numberSet", items.iter().cloned().map(Value::Number)),
        AttributeValue::Bs(items) => set(
            "$binarySet",
            items
                .iter()
                .map(|blob| Value::String(BASE64.encode(blob.as_ref()))),
        ),
        _ => Value::Object(vec![(
            "$unknownDynamoDbType".to_owned(),
            Value::String(format!("{attribute:?}")),
        )]),
    }
}

fn binary(blob: &Blob) -> Value {
    Value::Object(vec![(
        "$binary".to_owned(),
        Value::Object(vec![
            (
                "base64".to_owned(),
                Value::String(BASE64.encode(blob.as_ref())),
            ),
            (
                "subType".to_owned(),
                Value::String(GENERIC_SUBTYPE.to_owned()),
            ),
        ]),
    )])
}

fn set(name: &str, items: impl Iterator<Item = Value>) -> Value {
    Value::Object(vec![(name.to_owned(), Value::Array(items.collect()))])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(attributes: &[(&str, AttributeValue)]) -> HashMap<String, AttributeValue> {
        attributes
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect()
    }

    #[test]
    fn writes_each_attribute_type_as_extended_json() {
        let all = item(&[
            ("s", AttributeValue::S("text".into())),
            ("n", AttributeValue::N("12345678901234567890.10".into())),
            ("bool", AttributeValue::Bool(true)),
            ("null", AttributeValue::Null(true)),
            ("b", AttributeValue::B(Blob::new(vec![1, 2, 3]))),
            (
                "l",
                AttributeValue::L(vec![
                    AttributeValue::N("1".into()),
                    AttributeValue::S("a".into()),
                ]),
            ),
            (
                "m",
                AttributeValue::M(item(&[
                    ("z", AttributeValue::N("1".into())),
                    ("a", AttributeValue::Bool(false)),
                ])),
            ),
            ("ss", AttributeValue::Ss(vec!["x".into(), "y".into()])),
            ("ns", AttributeValue::Ns(vec!["1".into(), "2.5".into()])),
            ("bs", AttributeValue::Bs(vec![Blob::new(vec![255])])),
        ]);
        assert_eq!(
            document(&all, &["s".to_owned()]).to_json(),
            r#"{"s":"text","b":{"$binary":{"base64":"AQID","subType":"00"}},"bool":true,"bs":{"$binarySet":["/w=="]},"l":[1,"a"],"m":{"a":false,"z":1},"n":12345678901234567890.10,"ns":{"$numberSet":[1,2.5]},"null":null,"ss":{"$stringSet":["x","y"]}}"#
        );
    }

    #[test]
    fn puts_the_keys_first_in_their_order() {
        let keyed = item(&[
            ("amount", AttributeValue::N("1".into())),
            ("sk", AttributeValue::S("b".into())),
            ("pk", AttributeValue::S("a".into())),
        ]);
        let document = document(&keyed, &["pk".to_owned(), "sk".to_owned()]);
        let Value::Object(fields) = document else {
            panic!("not an object");
        };
        let names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["pk", "sk", "amount"]);
    }
}
