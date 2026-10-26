// Copyright 2025 Oxide Computer Company

#![allow(dead_code)]

//! Support code for generated clients.

use std::ops::{Deref, DerefMut};

use bytes::Bytes;
use futures_core::Stream;
use reqwest::RequestBuilder;
use serde::{Serialize, de::DeserializeOwned, ser::SerializeStruct};

#[cfg(not(target_arch = "wasm32"))]
type InnerByteStream = std::pin::Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send + Sync>>;

#[cfg(target_arch = "wasm32")]
type InnerByteStream = std::pin::Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>>>>;

/// Untyped byte stream used for both success and error responses.
pub struct ByteStream(InnerByteStream);

impl ByteStream {
    /// Creates a new ByteStream
    ///
    /// Useful for generating test fixtures.
    pub fn new(inner: InnerByteStream) -> Self {
        Self(inner)
    }

    /// Consumes the [`ByteStream`] and return its inner [`Stream`].
    pub fn into_inner(self) -> InnerByteStream {
        self.0
    }
}

impl Deref for ByteStream {
    type Target = InnerByteStream;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ByteStream {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// A framed SSE event, including its name, data, last event ID, and retry delay.
///
/// This is normalized framing information, not the original wire bytes: comments
/// and line endings are consumed by the framing parser.
pub use eventsource_stream::Event as RawSseEvent;

#[cfg(not(target_arch = "wasm32"))]
type InnerEventStream = std::pin::Pin<
    Box<
        dyn Stream<Item = Result<RawSseEvent, eventsource_stream::EventStreamError<reqwest::Error>>>
            + Send
            + Sync,
    >,
>;

#[cfg(target_arch = "wasm32")]
type InnerEventStream = std::pin::Pin<
    Box<
        dyn Stream<
            Item = Result<RawSseEvent, eventsource_stream::EventStreamError<reqwest::Error>>,
        >,
    >,
>;

/// A decoded JSON event together with all its framed SSE metadata.
#[derive(Debug)]
pub struct JsonEvent<T> {
    /// The original framed event, including the JSON data string.
    pub raw: RawSseEvent,
    /// The value decoded from `raw.data`.
    pub value: T,
}

/// An error encountered while consuming a JSON-over-SSE stream.
///
/// These errors occur after the HTTP response has been returned and are separate
/// from [`Error`]. A decoding error consumes only that event; polling can continue.
#[derive(Debug)]
pub enum JsonEventError {
    /// Invalid framing/UTF-8, or an error from the underlying HTTP body stream.
    Framing(eventsource_stream::EventStreamError<reqwest::Error>),
    /// Event data could not be decoded, including unknown JSON discriminator tags.
    Decode {
        /// The complete framed event, retained without cloning or losing metadata.
        raw: RawSseEvent,
        /// The JSON decoding failure.
        source: serde_json::Error,
    },
}

impl std::fmt::Display for JsonEventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Framing(source) => write!(f, "SSE framing error: {source}"),
            Self::Decode { raw, source } => write!(
                f,
                "Invalid JSON in SSE event {:?} (id {:?}): {source}",
                raw.event, raw.id,
            ),
        }
    }
}

impl std::error::Error for JsonEventError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Framing(source) => Some(source),
            Self::Decode { source, .. } => Some(source),
        }
    }
}

/// A stream that decodes each SSE event's data as JSON.
///
/// The SSE name is preserved but does not select or modify the JSON type.
/// Unknown payload types and sentinel data such as `[DONE]` are decoding errors,
/// not silently skipped events. No automatic reconnection or retry is performed.
pub struct JsonEventStream<T> {
    // Store only raw events: T need not be Unpin, Send, or Sync to poll the stream.
    inner: InnerEventStream,
    marker: std::marker::PhantomData<fn() -> T>,
}

impl<T> JsonEventStream<T> {
    /// Frame an HTTP byte stream without buffering the complete response.
    pub fn new(stream: ByteStream) -> Self {
        Self {
            inner: Box::pin(eventsource_stream::EventStream::new(StripInitialBom {
                inner: stream.into_inner(),
                prefix: Vec::new(),
                started: false,
            })),
            marker: std::marker::PhantomData,
        }
    }

    /// Consume this decoder and access the remaining raw framed events directly.
    ///
    /// Events already polled from this decoder are not replayed.
    pub fn into_raw_stream(self) -> InnerEventStream {
        self.inner
    }
}

impl<T> std::fmt::Debug for JsonEventStream<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JsonEventStream(<stream>)")
    }
}

impl<T: DeserializeOwned> Stream for JsonEventStream<T> {
    type Item = Result<JsonEvent<T>, JsonEventError>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        match self.get_mut().inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(raw))) => {
                // DeserializeOwned ensures the decoded value cannot borrow raw.data.
                let decoded = serde_json::from_str(&raw.data);
                Poll::Ready(Some(match decoded {
                    Ok(value) => Ok(JsonEvent { raw, value }),
                    Err(source) => Err(JsonEventError::Decode { raw, source }),
                }))
            }
            Poll::Ready(Some(Err(source))) => {
                Poll::Ready(Some(Err(JsonEventError::Framing(source))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// A response whose media type selects either one JSON value or a JSON SSE stream.
#[derive(Debug)]
pub enum JsonOrEventStream<J, T> {
    /// An ordinary `application/json` response.
    Json(J),
    /// A `text/event-stream` response, decoded one event at a time.
    EventStream(JsonEventStream<T>),
}

// eventsource-stream 0.2.3 slices its initial UTF-8 BOM at byte index 1, which
// panics. Strip exactly the initial BOM at the byte-stream boundary, including
// when its three bytes arrive separately; leave all other framing to that crate.
struct StripInitialBom {
    inner: InnerByteStream,
    prefix: Vec<u8>,
    started: bool,
}

impl Stream for StripInitialBom {
    type Item = reqwest::Result<Bytes>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        const BOM: &[u8] = b"\xef\xbb\xbf";
        let this = self.get_mut();
        loop {
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) if !this.started => {
                    for (index, byte) in bytes.iter().copied().enumerate() {
                        this.prefix.push(byte);
                        if !BOM.starts_with(&this.prefix) || this.prefix.len() == BOM.len() {
                            this.started = true;
                            let mut prefix = std::mem::take(&mut this.prefix);
                            if prefix == BOM {
                                prefix.clear();
                                // An empty line emits no event, but starts the
                                // parser before any subsequent BOM. Only the
                                // first BOM is special in SSE; a repeated BOM
                                // must not trigger the crate's initial-BOM bug.
                                prefix.push(b'\n');
                            }
                            prefix.extend_from_slice(&bytes[index + 1..]);
                            if !prefix.is_empty() {
                                return Poll::Ready(Some(Ok(Bytes::from(prefix))));
                            }
                            break;
                        }
                    }
                }
                Poll::Ready(None) if !this.prefix.is_empty() => {
                    this.started = true;
                    return Poll::Ready(Some(Ok(Bytes::from(std::mem::take(&mut this.prefix)))));
                }
                other => return other,
            }
        }
    }
}

fn has_media_type(response: &reqwest::Response, expected: &str) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

/// Interface for which an implementation is generated for all clients.
pub trait ClientInfo<Inner> {
    /// Get the version of this API.
    ///
    /// This string is pulled directly from the source OpenAPI document and may
    /// be in any format the API selects.
    fn api_version() -> &'static str;

    /// Get the base URL to which requests are made.
    fn baseurl(&self) -> &str;

    /// Get the internal `reqwest::Client` used to make requests.
    fn client(&self) -> &reqwest::Client;

    /// Get the inner value of type `T` if one is specified.
    fn inner(&self) -> &Inner;
}

impl<T, Inner> ClientInfo<Inner> for &T
where
    T: ClientInfo<Inner>,
{
    fn api_version() -> &'static str {
        T::api_version()
    }

    fn baseurl(&self) -> &str {
        (*self).baseurl()
    }

    fn client(&self) -> &reqwest::Client {
        (*self).client()
    }

    fn inner(&self) -> &Inner {
        (*self).inner()
    }
}

/// Information about an operation, consumed by hook implementations.
pub struct OperationInfo {
    /// The corresponding operationId from the source OpenAPI document.
    pub operation_id: &'static str,
}

/// Interface for changing the behavior of generated clients. All clients
/// implement this for `&Client`; to override the default behavior, implement
/// some or all of the interfaces for the `Client` type (without the
/// reference). This mechanism relies on so-called "auto-ref specialization".
#[allow(async_fn_in_trait, unused)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not implement `ClientHooks<{Inner}>`",
    label = "missing `impl ClientHooks<{Inner}> for {Self}`",
    note = "clients generated with `hooks = Expected` require `impl ClientHooks<{Inner}> for {Self}`"
)]
pub trait ClientHooks<Inner = ()>
where
    Self: ClientInfo<Inner>,
{
    /// Runs prior to the execution of the request. This may be used to modify
    /// the request before it is transmitted.
    async fn pre<E>(
        &self,
        request: &mut reqwest::Request,
        info: &OperationInfo,
    ) -> std::result::Result<(), Error<E>> {
        Ok(())
    }

    /// Runs after completion of the request.
    async fn post<E>(
        &self,
        result: &reqwest::Result<reqwest::Response>,
        info: &OperationInfo,
    ) -> std::result::Result<(), Error<E>> {
        Ok(())
    }

    /// Execute the request. Note that for almost any reasonable implementation
    /// this will include code equivalent to this:
    /// ```
    /// # use progenitor_client::{ClientHooks, ClientInfo, OperationInfo};
    /// # struct X;
    /// # impl ClientInfo<()> for X {
    /// #   fn api_version() -> &'static str { panic!() }
    /// #   fn baseurl(&self) -> &str { panic!() }
    /// #   fn client(&self) -> &reqwest::Client { panic!() }
    /// #   fn inner(&self) -> &() { panic!() }
    /// # }
    /// # impl ClientHooks for X {
    /// #   async fn exec(
    /// #       &self,
    /// #       request: reqwest::Request,
    /// #       info: &OperationInfo,
    /// #   ) -> reqwest::Result<reqwest::Response> {
    ///         self.client().execute(request).await
    /// #   }
    /// # }
    /// ```
    async fn exec(
        &self,
        request: reqwest::Request,
        info: &OperationInfo,
    ) -> reqwest::Result<reqwest::Response> {
        self.client().execute(request).await
    }
}

/// Typed value returned by generated client methods.
///
/// This is used for successful responses and may appear in error responses
/// generated from the server (see [`Error::ErrorResponse`])
pub struct ResponseValue<T> {
    inner: T,
    status: reqwest::StatusCode,
    headers: reqwest::header::HeaderMap,
    // TODO cookies?
}

impl<T: DeserializeOwned> ResponseValue<T> {
    /// Decode with generated response-only recovery metadata. The original
    /// strict decoder always runs first; direct model decoding is unchanged.
    #[doc(hidden)]
    pub async fn from_response_with_recovery<E>(
        response: reqwest::Response,
        nodes: &'static [ResponseRecoveryNode],
        root: usize,
        operation_id: &'static str,
        specification: &'static str,
    ) -> Result<Self, Error<E>> {
        let status = response.status();
        let headers = response.headers().clone();
        let full = response.bytes().await.map_err(Error::ResponseBodyError)?;
        let inner = decode_response_with_recovery(
            &full,
            nodes,
            root,
            operation_id,
            status.as_u16(),
            specification,
        )
        .map_err(|error| Error::InvalidResponsePayload(full, error))?;
        Ok(Self {
            inner,
            status,
            headers,
        })
    }

    #[doc(hidden)]
    pub async fn from_response<E>(response: reqwest::Response) -> Result<Self, Error<E>> {
        let status = response.status();
        let headers = response.headers().clone();
        let full = response.bytes().await.map_err(Error::ResponseBodyError)?;
        let inner =
            serde_json::from_slice(&full).map_err(|e| Error::InvalidResponsePayload(full, e))?;

        Ok(Self {
            inner,
            status,
            headers,
        })
    }
}

/// Generated metadata for a strict model and its response-only projection.
#[doc(hidden)]
pub struct ResponseRecoveryNode {
    pub accepts: fn(&serde_json::Value) -> bool,
    pub kind: ResponseRecoveryKind,
}

/// Recovery is deliberately smaller than JSON Schema validation. All actual
/// validation remains in the original generated Serde implementations.
#[doc(hidden)]
pub enum ResponseRecoveryKind {
    Leaf,
    Unsupported,
    Ref(usize),
    Object {
        properties: &'static [(&'static str, usize)],
        additional: ResponseAdditionalProperties,
    },
    Array(usize),
    Union(&'static [usize]),
}

#[doc(hidden)]
pub enum ResponseAdditionalProperties {
    Closed,
    Open,
    Typed(usize),
}

#[doc(hidden)]
pub fn response_matches<T: DeserializeOwned>(value: &serde_json::Value) -> bool {
    // Use the same JSON decoder as the original response path, including for
    // every branch candidate; Value is only the projection data structure.
    serde_json::to_vec(value)
        .ok()
        .is_some_and(|bytes| serde_json::from_slice::<T>(&bytes).is_ok())
}

#[derive(Clone, Copy, Debug)]
enum RecoveryFailure {
    Invalid,
    Ambiguous,
    Unsupported,
    Limit,
}

type RecoveredResponse = (serde_json::Value, std::collections::BTreeSet<String>);

fn recover_response(
    nodes: &[ResponseRecoveryNode],
    index: usize,
    value: &serde_json::Value,
    path: &str,
    depth: usize,
    budget: &mut usize,
) -> Result<RecoveredResponse, RecoveryFailure> {
    if depth > 64 || *budget == 0 {
        return Err(RecoveryFailure::Limit);
    }
    *budget -= 1;
    let node = nodes.get(index).ok_or(RecoveryFailure::Unsupported)?;
    if (node.accepts)(value) {
        return Ok((value.clone(), Default::default()));
    }
    let mut paths = std::collections::BTreeSet::new();
    let mut projected = value.clone();
    match &node.kind {
        ResponseRecoveryKind::Leaf => return Err(RecoveryFailure::Invalid),
        ResponseRecoveryKind::Unsupported => return Err(RecoveryFailure::Unsupported),
        ResponseRecoveryKind::Ref(target) => {
            (projected, paths) = recover_response(nodes, *target, value, path, depth + 1, budget)?;
        }
        ResponseRecoveryKind::Object {
            properties,
            additional,
        } => {
            let object = projected.as_object_mut().ok_or(RecoveryFailure::Invalid)?;
            let mut remove = Vec::new();
            for (key, entry) in object.iter_mut() {
                let named = properties.iter().find(|(name, _)| *name == key);
                let (child, segment) = if let Some((name, child)) = named {
                    (*child, pointer_segment(name))
                } else {
                    match additional {
                        ResponseAdditionalProperties::Closed => {
                            remove.push(key.clone());
                            if paths.len() < 128 {
                                paths.insert(format!("{path}/{}", pointer_segment(key)));
                            }
                            continue;
                        }
                        ResponseAdditionalProperties::Open => continue,
                        // Never expose dynamic map keys in diagnostic paths.
                        ResponseAdditionalProperties::Typed(child) => (*child, "*".into()),
                    }
                };
                let (replacement, child_paths) = recover_response(
                    nodes,
                    child,
                    entry,
                    &format!("{path}/{segment}"),
                    depth + 1,
                    budget,
                )?;
                *entry = replacement;
                merge_recovery_paths(&mut paths, child_paths);
            }
            for key in remove {
                object.remove(&key);
            }
        }
        ResponseRecoveryKind::Array(child) => {
            let array = projected.as_array_mut().ok_or(RecoveryFailure::Invalid)?;
            for entry in array {
                let (replacement, child_paths) = recover_response(
                    nodes,
                    *child,
                    entry,
                    &format!("{path}/[]"),
                    depth + 1,
                    budget,
                )?;
                *entry = replacement;
                merge_recovery_paths(&mut paths, child_paths);
            }
        }
        ResponseRecoveryKind::Union(branches) => {
            let mut selected = None;
            for branch in *branches {
                match recover_response(nodes, *branch, value, path, depth + 1, budget) {
                    Ok(candidate) => {
                        if selected.is_some() {
                            return Err(RecoveryFailure::Ambiguous);
                        }
                        selected = Some(candidate);
                    }
                    Err(RecoveryFailure::Invalid) => (),
                    // Resource/unsupported/ambiguity failures cannot be treated
                    // as evidence that this branch has no possible recovery.
                    Err(other) => return Err(other),
                }
            }
            (projected, paths) = selected.ok_or(RecoveryFailure::Invalid)?;
            // A field known to a sibling is not an unambiguously unknown
            // union field. Do not hide its invalid type/enum by deleting it
            // from a narrower candidate (a conservative fail-closed rule).
            for branch in *branches {
                if !preserves_union_known_fields(nodes, *branch, value, &projected, 0, budget) {
                    return Err(RecoveryFailure::Invalid);
                }
            }
            // Root Serde must not select a different, earlier untagged branch
            // after projection. Refuse newly overlapping projected instances.
            if branches
                .iter()
                .filter(|branch| (nodes[**branch].accepts)(&projected))
                .count()
                != 1
            {
                return Err(RecoveryFailure::Ambiguous);
            }
        }
    }
    if !(node.accepts)(&projected) {
        return Err(RecoveryFailure::Invalid);
    }
    Ok((projected, paths))
}

fn preserves_union_known_fields(
    nodes: &[ResponseRecoveryNode],
    index: usize,
    original: &serde_json::Value,
    projected: &serde_json::Value,
    depth: usize,
    budget: &mut usize,
) -> bool {
    if depth > 64 || *budget == 0 {
        return false;
    }
    *budget -= 1;
    let Some(node) = nodes.get(index) else {
        return false;
    };
    match &node.kind {
        ResponseRecoveryKind::Ref(child) => {
            preserves_union_known_fields(nodes, *child, original, projected, depth + 1, budget)
        }
        ResponseRecoveryKind::Union(branches) => branches.iter().all(|child| {
            preserves_union_known_fields(nodes, *child, original, projected, depth + 1, budget)
        }),
        ResponseRecoveryKind::Object {
            properties,
            additional,
        } => {
            let named_preserved =
                properties
                    .iter()
                    .all(|(name, child)| match original.get(*name) {
                        None => true,
                        Some(before) => projected.get(*name).is_some_and(|after| {
                            preserves_union_known_fields(
                                nodes,
                                *child,
                                before,
                                after,
                                depth + 1,
                                budget,
                            )
                        }),
                    });
            if !named_preserved {
                return false;
            }
            let Some(object) = original.as_object() else {
                return true;
            };
            object
                .iter()
                .filter(|(key, _)| !properties.iter().any(|(name, _)| *name == key.as_str()))
                .all(|(key, before)| match additional {
                    ResponseAdditionalProperties::Closed => true,
                    ResponseAdditionalProperties::Open => projected.get(key).is_some(),
                    ResponseAdditionalProperties::Typed(child) => {
                        projected.get(key).is_some_and(|after| {
                            preserves_union_known_fields(
                                nodes,
                                *child,
                                before,
                                after,
                                depth + 1,
                                budget,
                            )
                        })
                    }
                })
        }
        ResponseRecoveryKind::Array(child) => match (original.as_array(), projected.as_array()) {
            (Some(before), Some(after)) if before.len() == after.len() => {
                before.iter().zip(after).all(|(before, after)| {
                    preserves_union_known_fields(nodes, *child, before, after, depth + 1, budget)
                })
            }
            _ => true,
        },
        ResponseRecoveryKind::Leaf | ResponseRecoveryKind::Unsupported => true,
    }
}

fn pointer_segment(key: &str) -> String {
    // Bound retained diagnostic data even if a malicious server uses huge keys.
    // No JSON values or Serde error strings are ever logged.
    if key.len() > 128 {
        return "<oversized-field-name>".into();
    }
    key.replace('~', "~0").replace('/', "~1")
}

fn merge_recovery_paths(
    paths: &mut std::collections::BTreeSet<String>,
    incoming: std::collections::BTreeSet<String>,
) {
    for path in incoming {
        if paths.len() >= 128 {
            break;
        }
        paths.insert(path);
    }
}

/// Shared by generated decoders; warnings are emitted only after final strict
/// decoding succeeds. Invalid or ambiguous candidates never produce warnings.
#[doc(hidden)]
pub fn decode_response_with_recovery<T: DeserializeOwned>(
    bytes: &[u8],
    nodes: &[ResponseRecoveryNode],
    root: usize,
    operation_id: &'static str,
    status: u16,
    specification: &'static str,
) -> serde_json::Result<T> {
    let original_error = match serde_json::from_slice(bytes) {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    let value: RecoveryJson = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(_) => return Err(original_error),
    };
    let (projected, paths) = match recover_response(nodes, root, &value.0, "", 0, &mut 100_000) {
        Ok(candidate) => candidate,
        Err(RecoveryFailure::Ambiguous) => {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "ambiguous unknown-response-field recovery: multiple union branches match",
            ));
        }
        Err(_) => return Err(original_error),
    };
    if paths.is_empty() {
        return Err(original_error);
    }
    // Re-enter the actual JSON decoder, not just a Value deserializer.
    let decoded = serde_json::from_slice(&serde_json::to_vec(&projected)?)?;
    for path in paths {
        warn_unknown_response_field(operation_id, status, specification, path);
    }
    Ok(decoded)
}

// Value's ordinary map visitor silently keeps the last duplicate key. Recovery
// must not erase a strict decoder's duplicate-known-field failure. Reject all
// duplicate keys on the recovery path, including differently escaped spellings.
struct RecoveryJson(serde_json::Value);

impl<'de> serde::Deserialize<'de> for RecoveryJson {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> serde::de::Visitor<'de> for UniqueVisitor {
            type Value = RecoveryJson;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON without duplicate keys")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(RecoveryJson(serde_json::Value::Null))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(RecoveryJson(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(RecoveryJson(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(RecoveryJson(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|number| RecoveryJson(number.into()))
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(RecoveryJson(v.into()))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(RecoveryJson(v.into()))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<RecoveryJson>()? {
                    values.push(value.0);
                }
                Ok(RecoveryJson(values.into()))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom(
                            "duplicate JSON key during response recovery",
                        ));
                    }
                    let value = map.next_value::<RecoveryJson>()?;
                    values.insert(key, value.0);
                }
                Ok(RecoveryJson(values.into()))
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}

type ResponseWarningKey = (&'static str, u16, &'static str, String);

#[derive(Default)]
struct ResponseWarningDedup(std::collections::HashSet<ResponseWarningKey>);

impl ResponseWarningDedup {
    fn admit(&mut self, key: ResponseWarningKey) -> bool {
        self.0.len() < 1024 && self.0.insert(key)
    }
}

fn warn_unknown_response_field(
    operation_id: &'static str,
    status: u16,
    specification: &'static str,
    path: String,
) {
    if !log::log_enabled!(target: "progenitor_client::unknown_response_field", log::Level::Warn) {
        return;
    }
    static SEEN: std::sync::OnceLock<std::sync::Mutex<ResponseWarningDedup>> =
        std::sync::OnceLock::new();
    let mut seen = SEEN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // Process-wide hard cap. Once full, suppress new warnings rather than
    // retaining unbounded server-controlled keys or churning repeated logs.
    if !seen.admit((operation_id, status, specification, path.clone())) {
        return;
    }
    drop(seen);
    log::warn!(target: "progenitor_client::unknown_response_field",
        operation_id = operation_id, status = status,
        path = path.as_str(), specification = specification;
        "ignored forbidden field in accepted JSON response"
    );
}

#[cfg(test)]
mod response_warning_tests {
    use super::ResponseWarningDedup;

    #[test]
    fn warning_dedup_is_bounded_and_suppresses_repeated_and_overflow_keys() {
        let mut dedup = ResponseWarningDedup::default();
        assert!(dedup.admit(("operation", 200, "spec", "/[]/extra".into())));
        assert!(!dedup.admit(("operation", 200, "spec", "/[]/extra".into())));
        for index in 1..1024 {
            assert!(dedup.admit(("operation", 200, "spec", format!("/extra{index}"))));
        }
        assert!(!dedup.admit(("operation", 200, "spec", "/overflow".into())));
        assert!(!dedup.admit(("operation", 200, "spec", "/[]/extra".into())));
        assert_eq!(dedup.0.len(), 1024);
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ResponseValue<reqwest::Upgraded> {
    #[doc(hidden)]
    pub async fn upgrade<E: std::fmt::Debug>(
        response: reqwest::Response,
    ) -> Result<Self, Error<E>> {
        let status = response.status();
        let headers = response.headers().clone();
        if status == reqwest::StatusCode::SWITCHING_PROTOCOLS {
            let inner = response.upgrade().await.map_err(Error::InvalidUpgrade)?;

            Ok(Self {
                inner,
                status,
                headers,
            })
        } else {
            Err(Error::UnexpectedResponse(response))
        }
    }
}

impl ResponseValue<ByteStream> {
    #[doc(hidden)]
    pub fn stream(response: reqwest::Response) -> Self {
        let status = response.status();
        let headers = response.headers().clone();
        Self {
            inner: ByteStream(Box::pin(response.bytes_stream())),
            status,
            headers,
        }
    }
}

impl<T> ResponseValue<JsonEventStream<T>> {
    /// Create a streaming response, retaining status and headers.
    ///
    /// An absent or unsupported Content-Type returns the unconsumed response in
    /// [`Error::UnexpectedResponse`]. JSON decoding occurs only when polled.
    pub fn event_stream<E>(response: reqwest::Response) -> Result<Self, Error<E>> {
        if !has_media_type(&response, "text/event-stream") {
            return Err(Error::UnexpectedResponse(response));
        }
        let status = response.status();
        let headers = response.headers().clone();
        Ok(Self {
            inner: JsonEventStream::new(ByteStream(Box::pin(response.bytes_stream()))),
            status,
            headers,
        })
    }
}

impl<J: DeserializeOwned, T> ResponseValue<JsonOrEventStream<J, T>> {
    /// Decode JSON or return an SSE stream according to the response Content-Type.
    ///
    /// Media type matching is case-insensitive and ignores parameters. Unsupported,
    /// missing, or invalid Content-Type leaves the response body unconsumed in
    /// [`Error::UnexpectedResponse`]. HTTP status selection is the caller's job.
    pub async fn from_json_or_event_stream<E>(
        response: reqwest::Response,
    ) -> Result<Self, Error<E>> {
        if has_media_type(&response, "application/json") {
            let ResponseValue {
                inner,
                status,
                headers,
            } = ResponseValue::<J>::from_response(response).await?;
            Ok(Self {
                inner: JsonOrEventStream::Json(inner),
                status,
                headers,
            })
        } else {
            let ResponseValue {
                inner,
                status,
                headers,
            } = ResponseValue::<JsonEventStream<T>>::event_stream(response)?;
            Ok(Self {
                inner: JsonOrEventStream::EventStream(inner),
                status,
                headers,
            })
        }
    }
}

impl ResponseValue<()> {
    #[doc(hidden)]
    pub fn empty(response: reqwest::Response) -> Self {
        let status = response.status();
        let headers = response.headers().clone();
        // TODO is there anything we want to do to confirm that there is no
        // content?
        Self {
            inner: (),
            status,
            headers,
        }
    }
}

impl<T> ResponseValue<T> {
    /// Creates a [`ResponseValue`] from the inner type, status, and headers.
    ///
    /// Useful for generating test fixtures.
    pub fn new(inner: T, status: reqwest::StatusCode, headers: reqwest::header::HeaderMap) -> Self {
        Self {
            inner,
            status,
            headers,
        }
    }

    /// Consumes the ResponseValue, returning the wrapped value.
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Gets the status from this response.
    pub fn status(&self) -> reqwest::StatusCode {
        self.status
    }

    /// Gets the headers from this response.
    pub fn headers(&self) -> &reqwest::header::HeaderMap {
        &self.headers
    }

    /// Gets the parsed value of the Content-Length header, if present and
    /// valid.
    pub fn content_length(&self) -> Option<u64> {
        self.headers
            .get(reqwest::header::CONTENT_LENGTH)?
            .to_str()
            .ok()?
            .parse::<u64>()
            .ok()
    }

    #[doc(hidden)]
    pub fn map<U: std::fmt::Debug, F, E>(self, f: F) -> Result<ResponseValue<U>, E>
    where
        F: FnOnce(T) -> U,
    {
        let Self {
            inner,
            status,
            headers,
        } = self;

        Ok(ResponseValue {
            inner: f(inner),
            status,
            headers,
        })
    }
}

impl ResponseValue<ByteStream> {
    /// Consumes the `ResponseValue`, returning the wrapped [`Stream`].
    pub fn into_inner_stream(self) -> InnerByteStream {
        self.into_inner().into_inner()
    }
}

impl<T> Deref for ResponseValue<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> DerefMut for ResponseValue<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<T> AsRef<T> for ResponseValue<T> {
    fn as_ref(&self) -> &T {
        &self.inner
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for ResponseValue<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(f)
    }
}

/// Error produced by generated client methods.
///
/// The type parameter may be a struct if there's a single expected error type
/// or an enum if there are multiple valid error types. It can be the unit type
/// if there are no structured returns expected.
pub enum Error<E = ()> {
    /// The request did not conform to API requirements.
    InvalidRequest(String),

    /// A server error either due to the data, or with the connection.
    CommunicationError(reqwest::Error),

    /// An expected response when upgrading connection.
    InvalidUpgrade(reqwest::Error),

    /// A documented, expected error response.
    ErrorResponse(ResponseValue<E>),

    /// Encountered an error reading the body for an expected response.
    ResponseBodyError(reqwest::Error),

    /// An expected response code whose deserialization failed.
    InvalidResponsePayload(Bytes, serde_json::Error),

    /// A response not listed in the API description. This may represent a
    /// success or failure response; check `status().is_success()`.
    UnexpectedResponse(reqwest::Response),

    /// A custom error from a consumer-defined hook.
    Custom(String),
}

impl<E> Error<E> {
    /// Returns the status code, if the error was generated from a response.
    pub fn status(&self) -> Option<reqwest::StatusCode> {
        match self {
            Error::InvalidRequest(_) => None,
            Error::Custom(_) => None,
            Error::CommunicationError(e) => e.status(),
            Error::ErrorResponse(rv) => Some(rv.status()),
            Error::InvalidUpgrade(e) => e.status(),
            Error::ResponseBodyError(e) => e.status(),
            Error::InvalidResponsePayload(_, _) => None,
            Error::UnexpectedResponse(r) => Some(r.status()),
        }
    }

    /// Returns `true` if this error indicates that it is transient, and that
    /// the operation could succeed if retried.
    ///
    /// The following are considered retryable:
    ///
    /// * Communication errors (connection reset, timeout, etc.)
    /// * 408 Request Timeout
    /// * 429 Too Many Requests
    /// * 502 Bad Gateway
    /// * 503 Service Unavailable
    /// * 504 Gateway Timeout
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::CommunicationError(_) => true,
            Error::ErrorResponse(rv) => is_retryable_status(rv.status()),
            Error::UnexpectedResponse(r) => is_retryable_status(r.status()),
            Error::InvalidUpgrade(e) | Error::ResponseBodyError(e) => {
                // We expect InvalidUpgrade and ResponseBodyError to be 2xx
                // statuses.
                //
                // * If they are 2xx statuses, is_retryable_status returns
                //   false so there's no harm in checking.
                // * In the unlikely case that they are not 2xx statuses
                //   (e.g., the error type was created outside of Progenitor),
                //   it is appropriate to check for retryability.
                e.status().is_some_and(is_retryable_status)
            }
            Error::InvalidRequest(_) => false,
            Error::InvalidResponsePayload(_, _) => false,
            Error::Custom(_) => false,
        }
    }

    /// Converts this error into one without a typed body.
    ///
    /// This is useful for unified error handling with APIs that distinguish
    /// various error response bodies.
    pub fn into_untyped(self) -> Error {
        match self {
            Error::InvalidRequest(s) => Error::InvalidRequest(s),
            Error::Custom(s) => Error::Custom(s),
            Error::CommunicationError(e) => Error::CommunicationError(e),
            Error::ErrorResponse(ResponseValue {
                inner: _,
                status,
                headers,
            }) => Error::ErrorResponse(ResponseValue {
                inner: (),
                status,
                headers,
            }),
            Error::InvalidUpgrade(e) => Error::InvalidUpgrade(e),
            Error::ResponseBodyError(e) => Error::ResponseBodyError(e),
            Error::InvalidResponsePayload(b, e) => Error::InvalidResponsePayload(b, e),
            Error::UnexpectedResponse(r) => Error::UnexpectedResponse(r),
        }
    }
}

impl<E> From<std::convert::Infallible> for Error<E> {
    fn from(x: std::convert::Infallible) -> Self {
        match x {}
    }
}

impl<E> From<reqwest::Error> for Error<E> {
    fn from(e: reqwest::Error) -> Self {
        Self::CommunicationError(e)
    }
}

impl<E> From<reqwest::header::InvalidHeaderValue> for Error<E> {
    fn from(e: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidRequest(e.to_string())
    }
}

impl<E> std::fmt::Display for Error<E>
where
    ResponseValue<E>: ErrorFormat,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidRequest(s) => {
                write!(f, "Invalid Request: {}", s)?;
            }
            Error::CommunicationError(e) => {
                write!(f, "Communication Error: {}", e)?;
            }
            Error::ErrorResponse(rve) => {
                write!(f, "Error Response: ")?;
                rve.fmt_info(f)?;
            }
            Error::InvalidUpgrade(e) => {
                write!(f, "Invalid Response Upgrade: {}", e)?;
            }
            Error::ResponseBodyError(e) => {
                write!(f, "Invalid Response Body Bytes: {}", e)?;
            }
            Error::InvalidResponsePayload(b, e) => {
                write!(f, "Invalid Response Payload ({:?}): {}", b, e)?;
            }
            Error::UnexpectedResponse(r) => {
                write!(f, "Unexpected Response: {:?}", r)?;
            }
            Error::Custom(s) => {
                write!(f, "Error: {}", s)?;
            }
        }

        if f.alternate() {
            use std::error::Error as _;

            let mut src = self.source().and_then(|e| e.source());
            while let Some(s) = src {
                write!(f, ": {s}")?;
                src = s.source();
            }
        }
        Ok(())
    }
}

trait ErrorFormat {
    fn fmt_info(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result;
}

impl<E> ErrorFormat for ResponseValue<E>
where
    E: std::fmt::Debug,
{
    fn fmt_info(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "status: {}; headers: {:?}; value: {:?}",
            self.status, self.headers, self.inner,
        )
    }
}

impl ErrorFormat for ResponseValue<ByteStream> {
    fn fmt_info(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "status: {}; headers: {:?}; value: <stream>",
            self.status, self.headers,
        )
    }
}

impl<E> std::fmt::Debug for Error<E>
where
    ResponseValue<E>: ErrorFormat,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}
impl<E> std::error::Error for Error<E>
where
    ResponseValue<E>: ErrorFormat,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::CommunicationError(e) => Some(e),
            Error::InvalidUpgrade(e) => Some(e),
            Error::ResponseBodyError(e) => Some(e),
            Error::InvalidResponsePayload(_b, e) => Some(e),
            _ => None,
        }
    }
}

/// Returns `true` if the given HTTP status code is considered transient and
/// worth retrying.
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::REQUEST_TIMEOUT
            | reqwest::StatusCode::TOO_MANY_REQUESTS
            | reqwest::StatusCode::BAD_GATEWAY
            | reqwest::StatusCode::SERVICE_UNAVAILABLE
            | reqwest::StatusCode::GATEWAY_TIMEOUT
    )
}

// See https://url.spec.whatwg.org/#url-path-segment-string
const PATH_SET: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'%');

#[doc(hidden)]
/// Percent encode input string.
pub fn encode_path(pc: &str) -> String {
    percent_encoding::utf8_percent_encode(pc, PATH_SET).to_string()
}

#[doc(hidden)]
pub trait RequestBuilderExt<E> {
    fn form_urlencoded<T: Serialize + ?Sized>(self, body: &T) -> Result<RequestBuilder, Error<E>>;
}

impl<E> RequestBuilderExt<E> for RequestBuilder {
    fn form_urlencoded<T: Serialize + ?Sized>(self, body: &T) -> Result<Self, Error<E>> {
        Ok(self
            .header(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static("application/x-www-form-urlencoded"),
            )
            .body(
                serde_urlencoded::to_string(body)
                    .map_err(|_| Error::InvalidRequest("failed to serialize body".to_string()))?,
            ))
    }
}

#[doc(hidden)]
pub struct QueryParam<'a, T> {
    name: &'a str,
    value: &'a T,
}

impl<'a, T> QueryParam<'a, T> {
    #[doc(hidden)]
    pub fn new(name: &'a str, value: &'a T) -> Self {
        Self { name, value }
    }
}
impl<T> Serialize for QueryParam<'_, T>
where
    T: Serialize,
{
    fn serialize<S>(&self, inner: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let serializer = QuerySerializer {
            inner,
            name: self.name,
        };
        self.value.serialize(serializer)
    }
}

pub(crate) struct QuerySerializer<'a, S> {
    inner: S,
    name: &'a str,
}

macro_rules! serialize_scalar {
    ($f:ident, $t:ty) => {
        fn $f(self, v: $t) -> Result<Self::Ok, Self::Error> {
            [(self.name, v)].serialize(self.inner)
        }
    };
}

impl<'a, S> serde::Serializer for QuerySerializer<'a, S>
where
    S: serde::Serializer,
{
    type Ok = S::Ok;
    type Error = S::Error;
    type SerializeSeq = QuerySeq<'a, S::SerializeSeq>;
    type SerializeTuple = S::SerializeTuple;
    type SerializeTupleStruct = S::SerializeTupleStruct;
    type SerializeTupleVariant = S::SerializeTupleVariant;
    type SerializeMap = S::SerializeMap;
    type SerializeStruct = S::SerializeStruct;
    type SerializeStructVariant = S::SerializeStructVariant;

    serialize_scalar!(serialize_bool, bool);
    serialize_scalar!(serialize_i8, i8);
    serialize_scalar!(serialize_i16, i16);
    serialize_scalar!(serialize_i32, i32);
    serialize_scalar!(serialize_i64, i64);
    serialize_scalar!(serialize_u8, u8);
    serialize_scalar!(serialize_u16, u16);
    serialize_scalar!(serialize_u32, u32);
    serialize_scalar!(serialize_u64, u64);
    serialize_scalar!(serialize_f32, f32);
    serialize_scalar!(serialize_f64, f64);
    serialize_scalar!(serialize_char, char);
    serialize_scalar!(serialize_str, &str);

    fn serialize_bytes(self, v: &[u8]) -> Result<Self::Ok, Self::Error> {
        self.inner.serialize_bytes(v)
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        self.inner.serialize_none()
    }

    fn serialize_some<T>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        // Serialize the value through self which will proxy into the inner
        // Serializer as appropriate.
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        self.inner.serialize_unit()
    }

    fn serialize_unit_struct(self, name: &'static str) -> Result<Self::Ok, Self::Error> {
        self.inner.serialize_unit_struct(name)
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        // A query parameter with a list of enumerated values will produce an
        // enum with unit variants. We treat these as scalar values, ignoring
        // the unit variant wrapper.
        variant.serialize(self)
    }

    fn serialize_newtype_struct<T>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        self.inner.serialize_newtype_struct(name, value)
    }

    fn serialize_newtype_variant<T>(
        self,
        name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        // As with serde_json, we treat a newtype variant like a struct with a
        // single field. This may seem a little weird, but if an OpenAPI
        // document were to specify a query parameter whose schema was a oneOf
        // whose elements were objects with a single field, the user would end
        // up with an enum like this as a parameter.
        let mut map = self.inner.serialize_struct(name, 1)?;
        map.serialize_field(variant, value)?;
        map.end()
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        let Self { inner, name, .. } = self;
        Ok(QuerySeq {
            inner: inner.serialize_seq(len)?,
            name,
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        self.inner.serialize_tuple(len)
    }

    fn serialize_tuple_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        self.inner.serialize_tuple_struct(name, len)
    }

    fn serialize_tuple_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        self.inner
            .serialize_tuple_variant(name, variant_index, variant, len)
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        self.inner.serialize_map(len)
    }

    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        self.inner.serialize_struct(name, len)
    }

    fn serialize_struct_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        self.inner
            .serialize_struct_variant(name, variant_index, variant, len)
    }
}

#[doc(hidden)]
pub struct QuerySeq<'a, S> {
    inner: S,
    name: &'a str,
}

impl<S> serde::ser::SerializeSeq for QuerySeq<'_, S>
where
    S: serde::ser::SerializeSeq,
{
    type Ok = S::Ok;

    type Error = S::Error;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        let v = (self.name, value);
        self.inner.serialize_element(&v)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.inner.end()
    }
}
