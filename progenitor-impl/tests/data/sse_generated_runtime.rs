#[cfg(test)]
mod generated_tests {
    use futures::StreamExt;
    use progenitor_client::{Error, JsonEventError, JsonOrEventStream};
    use std::io::{Read, Write};

    fn server(status: u16, media: Option<&str>, body: &str, accept: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let header = media
            .map(|m| format!("Content-Type: {m}\r\n"))
            .unwrap_or_default();
        let reply = format!(
            "HTTP/1.1 {status} Test\r\n{header}Content-Length: {}\r\nX-Test: retained\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                assert_ne!(n, 0);
                request.extend_from_slice(&buf[..n]);
            }
            assert!(
                String::from_utf8_lossy(&request)
                    .to_ascii_lowercase()
                    .contains(accept)
            );
            stream.write_all(reply.as_bytes()).unwrap();
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn generated_mixed_dispatch_and_raw_event_errors() {
        let url = server(
            200,
            Some("application/json"),
            r#"{"id":"disabled"}"#,
            "accept: application/json\r\n",
        );
        let response: super::disabled::types::Response = super::disabled::Client::new(&url)
            .create_response()
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.id, "disabled");
        let url = server(
            200,
            Some("text/event-stream"),
            "data: {}\n\n",
            "accept: application/json\r\n",
        );
        assert!(matches!(
            super::disabled::Client::new(&url).create_response().await,
            Err(Error::InvalidResponsePayload(..))
        ));
        let url = server(
            200,
            Some("Application/JSON; charset=utf-8"),
            r#"{"id":"json","future":true}"#,
            "accept: application/json, text/event-stream",
        );
        let rv = super::positional::Client::new(&url)
            .create_response()
            .await
            .unwrap();
        assert_eq!(rv.status(), 200);
        assert_eq!(rv.headers()["x-test"], "retained");
        let JsonOrEventStream::Json(value) = rv.into_inner() else {
            panic!("expected JSON")
        };
        assert_eq!(value.id, "json");

        let wire = "event: malformed\nid: bad-1\ndata: {bad\n\nevent: future\nid: bad-2\ndata: {\"type\":\"future\"}\n\ndata: {\"type\":\"delta\",\"text\":\"한👍\"}\n\n";
        let url = server(
            200,
            Some("Text/Event-Stream; charset=utf-8"),
            wire,
            "accept: application/json, text/event-stream",
        );
        let rv = super::positional::Client::new(&url)
            .create_response()
            .await
            .unwrap();
        assert_eq!(rv.headers()["x-test"], "retained");
        let JsonOrEventStream::EventStream(mut stream) = rv.into_inner() else {
            panic!("expected SSE")
        };
        for (event, id, data) in [
            ("malformed", "bad-1", "{bad"),
            ("future", "bad-2", r#"{"type":"future"}"#),
        ] {
            let JsonEventError::Decode { raw, .. } = stream.next().await.unwrap().unwrap_err()
            else {
                panic!("expected decode error")
            };
            assert_eq!(raw.event, event);
            assert_eq!(raw.id, id);
            assert_eq!(raw.data, data);
        }
        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(serde_json::to_value(event.value).unwrap()["text"], "한👍");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn generated_builder_and_strict_mixed_clients_and_sse_only() {
        let url = server(
            200,
            Some("application/json"),
            r#"{"id":"builder"}"#,
            "accept: application/json, text/event-stream",
        );
        let rv = super::builder::Client::new(&url)
            .create_response()
            .send()
            .await
            .unwrap();
        assert!(matches!(rv.into_inner(), JsonOrEventStream::Json(value) if value.id == "builder"));
        let url = server(
            200,
            Some("application/json"),
            r#"{"id":"strict"}"#,
            "accept: application/json, text/event-stream",
        );
        assert!(
            super::strict::Client::new(&url)
                .create_response()
                .await
                .is_ok()
        );
        let url = server(
            200,
            Some("application/json"),
            r#"{"id":"strict","future":true}"#,
            "accept: application/json, text/event-stream",
        );
        assert!(matches!(
            super::strict::Client::new(&url).create_response().await,
            Err(Error::InvalidResponsePayload(..))
        ));
        let url = server(
            200,
            Some("text/event-stream"),
            "data: {\"type\":\"delta\",\"text\":\"strict events\",\"future\":true}\n\n",
            "accept: application/json, text/event-stream",
        );
        let JsonOrEventStream::EventStream(mut stream) = super::positional::Client::new(&url)
            .create_response()
            .await
            .unwrap()
            .into_inner()
        else {
            panic!("expected SSE")
        };
        assert!(matches!(
            stream.next().await.unwrap(),
            Err(JsonEventError::Decode { .. })
        ));
        let url = server(
            200,
            Some("text/event-stream"),
            "data: {\"type\":\"delta\",\"text\":\"only\"}\n\n",
            "accept: text/event-stream",
        );
        let mut stream = super::builder::Client::new(&url)
            .list_events()
            .send()
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            serde_json::to_value(stream.next().await.unwrap().unwrap().value).unwrap()["text"],
            "only"
        );
    }

    #[tokio::test]
    async fn generated_unsupported_media_keeps_body_and_http_error_is_not_streamed() {
        for media in [None, Some("text/plain"), Some("application/jsonish")] {
            let url = server(
                200,
                media,
                "untouched",
                "accept: application/json, text/event-stream",
            );
            let Error::UnexpectedResponse(response) = super::positional::Client::new(&url)
                .create_response()
                .await
                .unwrap_err()
            else {
                panic!("expected unexpected media")
            };
            assert_eq!(response.text().await.unwrap(), "untouched");
        }
        let url = server(
            400,
            Some("application/json"),
            "\"bad request\"",
            "accept: application/json, text/event-stream",
        );
        let Error::ErrorResponse(response) = super::positional::Client::new(&url)
            .create_response()
            .await
            .unwrap_err()
        else {
            panic!("expected HTTP error")
        };
        assert_eq!(response.status(), 400);
        assert_eq!(response.into_inner(), "bad request");
    }
}
