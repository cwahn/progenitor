// Copyright 2026 Oxide Computer Company

use std::{error::Error as _, time::Duration};

use bytes::Bytes;
use futures::{StreamExt, executor::block_on, stream};
use progenitor_client::{
    ByteStream, Error, JsonEventError, JsonEventStream, JsonOrEventStream, ResponseValue,
};
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    Delta { text: String },
}

fn decoder(chunks: Vec<reqwest::Result<Bytes>>) -> JsonEventStream<Event> {
    JsonEventStream::new(ByteStream::new(Box::pin(stream::iter(chunks))))
}

fn response(content_type: Option<&str>, body: &'static str) -> reqwest::Response {
    let mut builder = http::Response::builder()
        .status(201)
        .header("x-test", "retained");
    if let Some(content_type) = content_type {
        builder = builder.header("content-type", content_type);
    }
    builder.body(body).unwrap().into()
}

#[test]
fn every_split_and_single_byte_chunks_decode_utf8_and_bom() {
    block_on(async {
        for prefix in ["", "\u{feff}"] {
            let wire = format!("{prefix}data: {{\"type\":\"delta\",\"text\":\"👍한\"}}\n\n");
            let bytes = wire.as_bytes();
            let mut chunkings = (0..=bytes.len())
                .map(|split| {
                    vec![
                        Ok(Bytes::copy_from_slice(&bytes[..split])),
                        Ok(Bytes::copy_from_slice(&bytes[split..])),
                    ]
                })
                .collect::<Vec<_>>();
            chunkings.push(
                bytes
                    .iter()
                    .map(|byte| Ok(Bytes::from(vec![*byte])))
                    .collect(),
            );
            for chunks in chunkings {
                let mut events = decoder(chunks);
                let event = events.next().await.unwrap().unwrap();
                assert_eq!(
                    event.value,
                    Event::Delta {
                        text: "👍한".into()
                    }
                );
                assert_eq!(event.raw.event, "message");
                assert!(events.next().await.is_none());
            }
        }
    });
}

#[test]
fn multiline_crlf_comments_and_unknown_sse_name_preserve_metadata() {
    block_on(async {
        let mut events = decoder(vec![Ok(Bytes::from_static(
            b": comment\r\n\r\nevent: future-name\r\nid: item-42\r\nretry: 1200\r\ndata: {\"type\":\"delta\",\r\ndata: \"text\":\"hello\"}\r\n\r\n",
        ))]);
        let item = events.next().await.unwrap().unwrap();
        assert_eq!(
            item.value,
            Event::Delta {
                text: "hello".into()
            }
        );
        assert_eq!(item.raw.event, "future-name");
        assert_eq!(item.raw.id, "item-42");
        assert_eq!(item.raw.retry, Some(Duration::from_millis(1200)));
        assert_eq!(item.raw.data, "{\"type\":\"delta\",\n\"text\":\"hello\"}");
        assert!(events.next().await.is_none());
    });
}

#[test]
fn decode_errors_retain_raw_events_and_allow_continuation() {
    block_on(async {
        let mut events = decoder(vec![Ok(Bytes::from_static(
            b"event: malformed\nid: bad-1\nretry: 50\ndata: {bad\n\nevent: unknown\nid: bad-2\nretry: 60\ndata: {\"type\":\"future\"}\n\nevent: done\ndata: [DONE]\n\ndata: {\"type\":\"delta\",\"text\":\"after\"}\n\n",
        ))]);
        for (name, id, retry, data) in [
            (
                "malformed",
                "bad-1",
                Some(Duration::from_millis(50)),
                "{bad",
            ),
            (
                "unknown",
                "bad-2",
                Some(Duration::from_millis(60)),
                "{\"type\":\"future\"}",
            ),
            ("done", "bad-2", None, "[DONE]"),
        ] {
            let error = events.next().await.unwrap().unwrap_err();
            assert!(error.source().is_some());
            assert!(error.to_string().contains(name));
            let JsonEventError::Decode { raw, source } = error else {
                panic!("expected JSON error");
            };
            assert_eq!(raw.event, name);
            assert_eq!(raw.id, id);
            assert_eq!(raw.retry, retry);
            assert_eq!(raw.data, data);
            assert!(!source.to_string().is_empty());
        }
        assert_eq!(
            events.next().await.unwrap().unwrap().value,
            Event::Delta {
                text: "after".into()
            }
        );
        assert!(events.next().await.is_none());
    });
}

#[test]
fn transport_error_and_invalid_utf8_are_framing_errors() {
    block_on(async {
        let body = reqwest::Body::wrap_stream(stream::iter(vec![Err::<Bytes, _>(
            std::io::Error::new(std::io::ErrorKind::ConnectionReset, "test body reset"),
        )]));
        let failed_response: reqwest::Response = http::Response::new(body).into();
        let transport = failed_response.bytes().await.unwrap_err();
        let expected = transport.to_string();
        let mut events = decoder(vec![Err(transport)]);
        let error = events.next().await.unwrap().unwrap_err();
        assert!(error.source().is_some());
        match error {
            JsonEventError::Framing(eventsource_stream::EventStreamError::Transport(source)) => {
                assert_eq!(source.to_string(), expected);
                assert!(source.is_decode());
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(events.next().await.is_none());

        // An incomplete BOM must be passed through, not swallowed by the adapter.
        let mut events = decoder(vec![Ok(Bytes::from_static(b"\xef\xbb"))]);
        assert!(matches!(
            events.next().await.unwrap(),
            Err(JsonEventError::Framing(
                eventsource_stream::EventStreamError::Utf8(_)
            ))
        ));
    });
}

#[test]
fn repeated_bom_is_not_treated_as_another_initial_bom() {
    block_on(async {
        let mut events = decoder(vec![Ok(Bytes::from_static(
            b"\xef\xbb\xbf\xef\xbb\xbf:ignored field\ndata: {\"type\":\"delta\",\"text\":\"ok\"}\n\n",
        ))]);
        assert_eq!(
            events.next().await.unwrap().unwrap().value,
            Event::Delta { text: "ok".into() }
        );
        assert!(events.next().await.is_none());
    });
}

#[test]
fn remaining_raw_framed_events_are_accessible_without_decoding() {
    block_on(async {
        let mut events = decoder(vec![Ok(Bytes::from_static(
            b"data: {\"type\":\"delta\",\"text\":\"first\"}\n\nevent: opaque\nid: raw-id\ndata: not json\n\n",
        ))]);
        assert!(events.next().await.unwrap().is_ok());
        let mut raw = events.into_raw_stream();
        let event = raw.next().await.unwrap().unwrap();
        assert_eq!(event.event, "opaque");
        assert_eq!(event.id, "raw-id");
        assert_eq!(event.data, "not json");
        assert!(raw.next().await.is_none());
    });
}

#[test]
fn constructors_dispatch_media_types_and_retain_response_metadata() {
    block_on(async {
        let rv = ResponseValue::<JsonOrEventStream<serde_json::Value, Event>>::
            from_json_or_event_stream::<()>(response(
                Some("Application/JSON ; charset=utf-8"), "{\"ok\":true}",
            )).await.unwrap();
        assert_eq!(rv.status(), 201);
        assert_eq!(rv.headers()["x-test"], "retained");
        assert!(matches!(rv.into_inner(), JsonOrEventStream::Json(value) if value["ok"] == true));

        let rv = ResponseValue::<JsonOrEventStream<serde_json::Value, Event>>::
            from_json_or_event_stream::<()>(response(
                Some("Text/Event-Stream; charset=utf-8"),
                "data: {\"type\":\"delta\",\"text\":\"stream\"}\n\n",
            )).await.unwrap();
        assert_eq!(rv.status(), 201);
        assert_eq!(rv.headers()["x-test"], "retained");
        let JsonOrEventStream::EventStream(mut events) = rv.into_inner() else {
            panic!("expected SSE")
        };
        assert_eq!(
            events.next().await.unwrap().unwrap().value,
            Event::Delta {
                text: "stream".into()
            }
        );

        let rv = ResponseValue::<JsonEventStream<Event>>::event_stream::<()>(response(
            Some("text/event-stream"),
            "data: {}\n\n",
        ))
        .unwrap();
        assert_eq!(rv.status(), 201);
        assert_eq!(rv.headers()["x-test"], "retained");
    });
}

#[test]
fn unsupported_media_type_keeps_body_and_json_errors_keep_existing_model() {
    block_on(async {
        for content_type in [None, Some("text/plain"), Some("application/jsonish")] {
            let error = ResponseValue::<JsonOrEventStream<serde_json::Value, Event>>::
                from_json_or_event_stream::<()>(response(content_type, "untouched"))
                .await.unwrap_err();
            let Error::UnexpectedResponse(response) = error else {
                panic!("expected unexpected response")
            };
            assert_eq!(response.bytes().await.unwrap(), "untouched");
        }
        let error = ResponseValue::<JsonOrEventStream<serde_json::Value, Event>>::
            from_json_or_event_stream::<()>(response(Some("application/json"), "not-json"))
            .await.unwrap_err();
        assert!(matches!(error, Error::InvalidResponsePayload(bytes, _) if bytes == "not-json"));
    });
}
