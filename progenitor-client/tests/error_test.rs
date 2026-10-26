// Copyright 2026 Oxide Computer Company

use bytes::Bytes;
use futures::executor::block_on;
use progenitor_client::{Error, JsonEventStream, ResponseValue};
use reqwest::{StatusCode, header::HeaderMap};
use serde::Deserialize;

#[test]
fn error_size_is_bounded_independently_of_typed_payload_size() {
    assert_eq!(size_of::<Error<[u8; 65_536]>>(), size_of::<Error<()>>());
    assert!(size_of::<Error<[u8; 65_536]>>() <= 128);
    assert!(size_of::<Result<(), Error<[u8; 65_536]>>>() <= 128);
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Payload {
    message: String,
    code: u32,
}

fn headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-request-id", "retained".parse().unwrap());
    headers.insert("retry-after", "42".parse().unwrap());
    headers
}

fn response(status: StatusCode, body: &'static [u8]) -> reqwest::Response {
    let mut response = http::Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers();
    response.into()
}

#[test]
fn typed_error_retains_decoded_payload_status_headers_and_formatting() {
    let value: ResponseValue<Payload> = block_on(ResponseValue::from_response::<()>(response(
        StatusCode::TOO_MANY_REQUESTS,
        br#"{"message":"slow down","code":17}"#,
    )))
    .unwrap();
    let error = Error::ErrorResponse(Box::new(value));
    assert_eq!(error.status(), Some(StatusCode::TOO_MANY_REQUESTS));
    assert!(error.is_retryable());
    let display = error.to_string();
    assert!(display.contains("Error Response:"));
    assert!(display.contains("slow down"));
    assert!(display.contains("retained"));
    assert_eq!(format!("{error:?}"), display);
    assert!(std::error::Error::source(&error).is_none());
    let Error::ErrorResponse(value) = error else {
        panic!("expected typed error response");
    };
    assert_eq!(value.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(value.headers(), &headers());
    assert_eq!(
        (*value).into_inner(),
        Payload {
            message: "slow down".into(),
            code: 17,
        }
    );
}

#[test]
fn response_error_classification_and_untyped_conversion_preserve_metadata() {
    for (status, retryable) in [
        (StatusCode::OK, false),
        (StatusCode::BAD_REQUEST, false),
        (StatusCode::REQUEST_TIMEOUT, true),
        (StatusCode::TOO_MANY_REQUESTS, true),
        (StatusCode::INTERNAL_SERVER_ERROR, false),
        (StatusCode::BAD_GATEWAY, true),
        (StatusCode::SERVICE_UNAVAILABLE, true),
        (StatusCode::GATEWAY_TIMEOUT, true),
    ] {
        let typed = Error::ErrorResponse(Box::new(ResponseValue::new(
            [7_u8; 65_536],
            status,
            headers(),
        )));
        let unexpected: Error<[u8; 65_536]> =
            Error::UnexpectedResponse(Box::new(response(status, b"\0unconsumed\xff HTTP body")));
        assert_eq!(typed.status(), Some(status));
        assert_eq!(typed.is_retryable(), retryable);
        assert_eq!(unexpected.status(), Some(status));
        assert_eq!(unexpected.is_retryable(), retryable);

        let typed = typed.into_untyped();
        let unexpected = unexpected.into_untyped();
        assert_eq!(typed.status(), Some(status));
        assert_eq!(typed.is_retryable(), retryable);
        assert_eq!(unexpected.status(), Some(status));
        assert_eq!(unexpected.is_retryable(), retryable);
        let Error::ErrorResponse(value) = typed else {
            panic!("expected typed response category");
        };
        assert_eq!(value.status(), status);
        assert_eq!(value.headers(), &headers());
        let (): () = (*value).into_inner();
        let Error::UnexpectedResponse(response) = unexpected else {
            panic!("expected unexpected response category");
        };
        assert_eq!(response.status(), status);
        assert_eq!(response.headers(), &headers());
        assert_eq!(
            block_on((*response).bytes()).unwrap(),
            Bytes::from_static(b"\0unconsumed\xff HTTP body")
        );
    }
}

#[test]
fn unexpected_response_constructor_retains_body_and_box_through_conversion() {
    let error = ResponseValue::<JsonEventStream<Payload>>::event_stream::<Payload>(response(
        StatusCode::SERVICE_UNAVAILABLE,
        b"not consumed by the SSE constructor",
    ))
    .unwrap_err();
    let Error::UnexpectedResponse(original) = &error else {
        panic!("expected unsupported-media response");
    };
    let address = std::ptr::from_ref(original.as_ref());
    let error = error.into_untyped();
    assert_eq!(error.status(), Some(StatusCode::SERVICE_UNAVAILABLE));
    assert!(error.is_retryable());
    let Error::UnexpectedResponse(response) = error else {
        panic!("expected unexpected response after conversion");
    };
    assert_eq!(std::ptr::from_ref(response.as_ref()), address);
    assert_eq!(response.headers(), &headers());
    assert_eq!(
        block_on((*response).bytes()).unwrap(),
        "not consumed by the SSE constructor"
    );
}

#[test]
fn strict_payload_errors_keep_original_bytes_and_serde_source_when_untyped() {
    for body in [
        br#"{"message":"strict","code":1,"extra":true}"#.as_slice(),
        br#"{"message":"strict","code":"wrong"}"#.as_slice(),
        b"not JSON".as_slice(),
    ] {
        let error = block_on(ResponseValue::<Payload>::from_response::<Payload>(
            response(StatusCode::BAD_REQUEST, body),
        ))
        .unwrap_err();
        assert_eq!(error.status(), None);
        assert!(!error.is_retryable());
        let source = std::error::Error::source(&error).unwrap().to_string();
        let error = error.into_untyped();
        assert_eq!(error.status(), None);
        assert!(!error.is_retryable());
        assert_eq!(
            std::error::Error::source(&error).unwrap().to_string(),
            source
        );
        let Error::InvalidResponsePayload(bytes, error) = error else {
            panic!("expected strict JSON decoding failure");
        };
        assert_eq!(bytes.as_ref(), body);
        assert_eq!(error.to_string(), source);
    }
}
