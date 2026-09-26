//! `#[action]` against the Python library as the oracle.
//!
//! `fixtures/python_declaration.json` is what the Python `arkitekt`/`rekuest`
//! registers for the functions in `fixtures/python_declaration.py`. The Rust
//! twins below must produce the same declaration (modulo fields that are
//! null or empty on either side, which the backend treats as defaults).

use std::collections::HashMap;

use futures::stream::{self, Stream};
use rekuest::{action, Action, ActionError, Context, PortType, Structure, Task};
use serde_json::{json, Map, Value};

/// A stand-in for mikro's ArrayDataset.
#[derive(Debug, Clone, PartialEq)]
struct ArrayDataset {
    id: String,
}

const SEARCH_ARRAY_DATASETS: &str = "query SearchArrayDatasets($search: String, $values: [ID!], $limit: Int, $offset: Int = 0) {\n  options: arrayDatasets(\n    filters: {search: $search, ids: $values}\n    pagination: {limit: $limit, offset: $offset}\n  ) {\n    value: id\n    label: name\n    __typename\n  }\n}";

impl Structure for ArrayDataset {
    const IDENTIFIER: &'static str = "@mikro/arraydataset";

    fn structure_id(&self) -> String {
        self.id.clone()
    }

    async fn expand(id: String, _ctx: &Context) -> anyhow::Result<Self> {
        Ok(ArrayDataset { id })
    }

    fn widget() -> Option<Value> {
        Some(rekuest::widgets::search(SEARCH_ARRAY_DATASETS, "mikro"))
    }
}

/// Greet someone
///
/// Says hello, possibly several times.
///
/// # Arguments
/// * `name` - Who to greet
/// * `times` - How often
///
/// # Returns
/// The greeting
#[action]
async fn greet(name: String, #[port(default = 1)] times: i64) -> String {
    name.repeat(times as usize)
}

/// Rescale an image
#[action]
async fn rescale(
    image: ArrayDataset,
    factors: Vec<f64>,
    label: Option<String>,
) -> (ArrayDataset, i64) {
    let _ = label;
    (image, factors.len() as i64)
}

#[action]
fn no_doc(flag: bool, lookup: HashMap<String, i64>) {
    let _ = (flag, lookup);
}

/// Fails on purpose
#[action]
async fn fails(reason: String) -> anyhow::Result<i64> {
    anyhow::bail!("{reason}")
}

/// Counts up
#[action]
async fn count(to: i64, task: Task) -> impl Stream<Item = i64> {
    task.progress(0, "counting");
    stream::iter(0..to)
}

/// Drop what the backend treats as unset: nulls and empty lists.
fn normalize(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, v)| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()))
                .map(|(k, v)| (k, normalize(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(normalize).collect()),
        other => other,
    }
}

#[test]
fn declaration_matches_python() {
    let expected: Value = serde_json::from_str(include_str!("fixtures/python_declaration.json"))
        .expect("fixture parses");
    let expected = normalize(expected["implementations"].clone());

    let actual = normalize(json!([
        greet.implementation(),
        rescale.implementation(),
        no_doc.implementation(),
    ]));

    let (expected, actual) = (expected.as_array().unwrap(), actual.as_array().unwrap());
    assert_eq!(expected.len(), actual.len());
    for (e, a) in expected.iter().zip(actual) {
        assert_eq!(
            e,
            a,
            "\nexpected: {}\nactual:   {}",
            serde_json::to_string_pretty(e).unwrap(),
            serde_json::to_string_pretty(a).unwrap()
        );
    }
}

#[test]
fn generator_kind() {
    let definition = count.definition();
    assert_eq!(definition.kind, rekuest::ActionKind::Generator);
    assert_eq!(definition.args.len(), 1, "the Task parameter is not a port");
    assert_eq!(definition.returns[0].key, "return0");
}

/// Run an action with a local task, capturing what it yields.
async fn run(action: impl Action, args: Value) -> Result<(), ActionError> {
    let args: Map<String, Value> = serde_json::from_value(args).unwrap();
    action.run(args, Context::default(), Task::local()).await
}

#[tokio::test]
async fn runs_actions() {
    run(greet, json!({"name": "hi", "times": 2})).await.unwrap();
    // `times` falls back to its default.
    run(greet, json!({"name": "hi"})).await.unwrap();
    run(
        rescale,
        json!({"image": {"__identifier": "@mikro/arraydataset", "object": "3"}, "factors": [1.0, 2.0]}),
    )
    .await
    .unwrap();
    run(no_doc, json!({"flag": true, "lookup": {"a": 1}}))
        .await
        .unwrap();
    run(count, json!({"to": 3})).await.unwrap();

    // A bad input is a FAILED (the caller's fault)...
    let err = run(greet, json!({"times": 2})).await.unwrap_err();
    assert!(
        matches!(err, ActionError::Failed(ref m) if m.contains("port 'name'")),
        "{err:?}"
    );
    // ...an error from the body is CRITICAL.
    let err = run(fails, json!({"reason": "nope"})).await.unwrap_err();
    assert_eq!(err, ActionError::Critical("nope".into()));
}

#[tokio::test]
async fn direct_call_still_works() {
    assert_eq!(greet::call("a".into(), 3).await, "aaa");
    let port = <ArrayDataset as PortType>::port("x");
    assert_eq!(port.identifier.as_deref(), Some("@mikro/arraydataset"));
}
