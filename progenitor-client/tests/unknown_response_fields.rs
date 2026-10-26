// Copyright 2026 Oxide Computer Company

use std::{collections::BTreeMap, sync::Mutex};

use progenitor_client::{
    ResponseAdditionalProperties::*, ResponseRecoveryKind::*, ResponseRecoveryNode as Node,
    decode_response_with_recovery as decode, response_matches as matches,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    kind: Kind,
    size: u32,
}
#[derive(Debug, Deserialize)]
enum Kind {
    File,
    Directory,
}

static ITEM: &[Node] = &[
    Node {
        accepts: matches::<Vec<Item>>,
        kind: Array(1),
    },
    Node {
        accepts: matches::<Item>,
        kind: Object {
            properties: &[("kind", 2), ("size", 3)],
            additional: Closed,
        },
    },
    Node {
        accepts: matches::<Kind>,
        kind: Leaf,
    },
    Node {
        accepts: matches::<u32>,
        kind: Leaf,
    },
];

struct Capture(Mutex<Vec<BTreeMap<String, String>>>);
static CAPTURE: Capture = Capture(Mutex::new(Vec::new()));
static START: std::sync::Once = std::sync::Once::new();
impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn flush(&self) {}
    fn log(&self, record: &log::Record<'_>) {
        if record.target() != "progenitor_client::unknown_response_field" {
            return;
        }
        struct Fields(BTreeMap<String, String>);
        impl<'kvs> log::kv::VisitSource<'kvs> for Fields {
            fn visit_pair(
                &mut self,
                key: log::kv::Key<'kvs>,
                value: log::kv::Value<'kvs>,
            ) -> Result<(), log::kv::Error> {
                self.0.insert(key.to_string(), value.to_string());
                Ok(())
            }
        }
        let mut fields = Fields(BTreeMap::new());
        record.key_values().visit(&mut fields).unwrap();
        self.0.lock().unwrap().push(fields.0);
    }
}
fn logger() {
    START.call_once(|| {
        log::set_logger(&CAPTURE).unwrap();
        log::set_max_level(log::LevelFilter::Warn);
    });
}
fn records(operation: &str) -> Vec<BTreeMap<String, String>> {
    CAPTURE
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["operation_id"] == operation)
        .cloned()
        .collect()
}

#[test]
fn nested_array_warnings_are_structured_normalized_and_deduplicated() {
    logger();
    let body = br#"[{"kind":"File","size":1,"extra":"SECRET-PAYLOAD"},{"kind":"Directory","size":2,"extra":false}]"#;
    for _ in 0..3 {
        let items: Vec<Item> = decode(body, ITEM, 0, "array-dedup", 200, "snapshot-1").unwrap();
        assert_eq!(items[0].size, 1);
        assert!(matches!(items[1].kind, Kind::Directory));
    }
    let warnings = records("array-dedup");
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0]["path"], "/[]/extra");
    assert_eq!(warnings[0]["status"], "200");
    assert_eq!(warnings[0]["specification"], "snapshot-1");
    assert!(!format!("{warnings:?}").contains("SECRET-PAYLOAD"));
    let _: Vec<Item> = decode(body, ITEM, 0, "array-dedup", 201, "snapshot-1").unwrap();
    assert_eq!(records("array-dedup").len(), 2);
}

#[test]
fn missing_wrong_type_range_and_unknown_enum_remain_errors_without_warnings() {
    logger();
    for body in [
        r#"[{"kind":"File","extra":1}]"#,
        r#"[{"kind":"File","size":"1","extra":1}]"#,
        r#"[{"kind":"File","size":4294967296,"extra":1}]"#,
        r#"[{"kind":"File","size":1.5,"extra":1}]"#,
        r#"[{"kind":"NEW_KIND","size":1,"extra":1}]"#,
    ] {
        assert!(
            decode::<Vec<Item>>(body.as_bytes(), ITEM, 0, "invalid-known", 200, "spec").is_err()
        );
    }
    assert!(records("invalid-known").is_empty());
}

#[test]
fn duplicates_including_escaped_known_names_fail_closed() {
    for body in [
        r#"[{"kind":"File","size":1,"size":2,"extra":1}]"#,
        r#"[{"kind":"File","size":1,"s\u0069ze":2,"extra":1}]"#,
        r#"[{"kind":"File","size":1,"size":2}]"#,
    ] {
        assert!(decode::<Vec<Item>>(body.as_bytes(), ITEM, 0, "duplicate", 200, "spec").is_err());
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    groups: Vec<Vec<Item>>,
}

#[test]
fn deeply_nested_array_paths_are_normalized_without_dropping_known_data() {
    logger();
    let nodes = [
        Node {
            accepts: matches::<Envelope>,
            kind: Object {
                properties: &[("groups", 1)],
                additional: Closed,
            },
        },
        Node {
            accepts: matches::<Vec<Vec<Item>>>,
            kind: Array(2),
        },
        Node {
            accepts: matches::<Vec<Item>>,
            kind: Array(3),
        },
        Node {
            accepts: matches::<Item>,
            kind: Object {
                properties: &[("kind", 4), ("size", 5)],
                additional: Closed,
            },
        },
        Node {
            accepts: matches::<Kind>,
            kind: Leaf,
        },
        Node {
            accepts: matches::<u32>,
            kind: Leaf,
        },
    ];
    let value: Envelope = decode(br#"{"groups":[[{"kind":"File","size":7,"extra":"SECRET"}], [{"kind":"Directory","size":8,"extra":null}]]}"#,
        &nodes, 0, "nested-groups", 200, "spec").unwrap();
    assert_eq!(value.groups[0][0].size, 7);
    assert_eq!(value.groups[1][0].size, 8);
    let warnings = records("nested-groups");
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0]["path"], "/groups/[]/[]/extra");
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct A {
    a: u32,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct B {
    b: u32,
}
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Union {
    A(A),
    B(B),
}
static UNION: &[Node] = &[
    Node {
        accepts: matches::<Union>,
        kind: Union(&[1, 2]),
    },
    Node {
        accepts: matches::<A>,
        kind: Object {
            properties: &[("a", 3)],
            additional: Closed,
        },
    },
    Node {
        accepts: matches::<B>,
        kind: Object {
            properties: &[("b", 3)],
            additional: Closed,
        },
    },
    Node {
        accepts: matches::<u32>,
        kind: Leaf,
    },
];

#[test]
fn union_recovery_requires_exactly_one_branch_and_logs_only_that_candidate() {
    logger();
    let value: Union = decode(
        br#"{"b":2,"extra":3}"#,
        UNION,
        0,
        "unique-union",
        200,
        "spec",
    )
    .unwrap();
    assert!(matches!(value, Union::B(B { b: 2 })));
    assert_eq!(records("unique-union").len(), 1);
    let value: Union = decode(
        br#"{"a":1,"extra":3}"#,
        UNION,
        0,
        "unique-union-a",
        200,
        "spec",
    )
    .unwrap();
    assert!(matches!(value, Union::A(A { a: 1 })));
    assert_eq!(records("unique-union-a").len(), 1);
    let error = decode::<Union>(
        br#"{"a":1,"b":2,"extra":3}"#,
        UNION,
        0,
        "ambiguous-union",
        200,
        "spec",
    )
    .unwrap_err();
    assert!(error.to_string().contains("ambiguous"));
    assert!(records("ambiguous-union").is_empty());
    assert!(
        decode::<Union>(
            br#"{"a":"wrong","b":"wrong","extra":3}"#,
            UNION,
            0,
            "zero-union",
            200,
            "spec"
        )
        .is_err()
    );
    // Do not use B as a pretext to delete A's known-but-invalid field.
    assert!(
        decode::<Union>(
            br#"{"a":"wrong","b":2,"extra":3}"#,
            UNION,
            0,
            "wrong-sibling",
            200,
            "spec"
        )
        .is_err()
    );
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wide {
    a: u32,
    b: u32,
}
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Ordered {
    Narrow(A),
    Wide(Wide),
}

#[test]
fn strict_success_keeps_existing_variant_without_consulting_recovery() {
    let value: Ordered =
        decode(br#"{"a":1,"b":2}"#, &[], 999, "strict-success", 200, "spec").unwrap();
    assert!(matches!(value, Ordered::Wide(Wide { a: 1, b: 2 })));
    let value: Ordered = decode(
        br#"{"a":1}"#,
        &[],
        999,
        "strict-success-narrow",
        200,
        "spec",
    )
    .unwrap();
    assert!(matches!(value, Ordered::Narrow(A { a: 1 })));
    assert!(serde_json::from_str::<A>(r#"{"a":1,"extra":2}"#).is_err());
}

#[derive(Debug, Deserialize)]
struct Open {
    fixed: u32,
    #[serde(flatten)]
    more: BTreeMap<String, serde_json::Value>,
}

#[test]
fn schema_allowed_additional_properties_are_retained_and_not_warned() {
    logger();
    let nodes = [Node {
        accepts: matches::<Open>,
        kind: Object {
            properties: &[],
            additional: Open,
        },
    }];
    let value: Open = decode(
        br#"{"fixed":1,"allowed":"keep"}"#,
        &nodes,
        0,
        "open-map",
        200,
        "spec",
    )
    .unwrap();
    assert_eq!(value.fixed, 1);
    assert_eq!(value.more["allowed"], "keep");
    assert!(records("open-map").is_empty());
}

#[test]
fn typed_map_recovers_values_without_exposing_dynamic_keys() {
    logger();
    let nodes = [
        Node {
            accepts: matches::<BTreeMap<String, Item>>,
            kind: Object {
                properties: &[],
                additional: Typed(1),
            },
        },
        Node {
            accepts: matches::<Item>,
            kind: Object {
                properties: &[("kind", 2), ("size", 3)],
                additional: Closed,
            },
        },
        Node {
            accepts: matches::<Kind>,
            kind: Leaf,
        },
        Node {
            accepts: matches::<u32>,
            kind: Leaf,
        },
    ];
    let value: BTreeMap<String, Item> = decode(
        br#"{"PERSON-SECRET":{"kind":"File","size":1,"extra":"VALUE-SECRET"}}"#,
        &nodes,
        0,
        "typed-map",
        200,
        "spec",
    )
    .unwrap();
    assert!(value.contains_key("PERSON-SECRET"));
    let warnings = records("typed-map");
    assert_eq!(warnings[0]["path"], "/*/extra");
    assert!(!format!("{warnings:?}").contains("SECRET"));
}

#[test]
fn unsupported_shapes_and_reference_cycles_fail_closed() {
    let unsupported = [Node {
        accepts: matches::<A>,
        kind: Unsupported,
    }];
    assert!(
        decode::<A>(
            br#"{"a":1,"extra":2}"#,
            &unsupported,
            0,
            "unsupported",
            200,
            "spec"
        )
        .is_err()
    );
    let cycle = [Node {
        accepts: matches::<A>,
        kind: Ref(0),
    }];
    assert!(decode::<A>(br#"{"a":1,"extra":2}"#, &cycle, 0, "cycle", 200, "spec").is_err());
}
