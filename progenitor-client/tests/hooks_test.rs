// Copyright 2026 Oxide Computer Company

use std::{
    cell::Cell,
    future::Future,
    pin::pin,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use futures::executor::block_on;
use progenitor_client::{ClientHooks, ClientInfo, Error, OperationInfo};

struct Client {
    client: reqwest::Client,
    calls: Rc<Cell<usize>>,
}

impl Client {
    fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            calls: Rc::new(Cell::new(0)),
        }
    }
}

impl ClientInfo<Rc<Cell<usize>>> for Client {
    fn api_version() -> &'static str {
        "test"
    }

    fn baseurl(&self) -> &str {
        "http://127.0.0.1"
    }

    fn client(&self) -> &reqwest::Client {
        self.calls.set(self.calls.get() + 1);
        &self.client
    }

    fn inner(&self) -> &Rc<Cell<usize>> {
        &self.calls
    }
}

// Downstream async fn implementations must remain accepted. These futures
// deliberately retain Rc across await and are run without a Send executor.
impl ClientHooks<Rc<Cell<usize>>> for Client {
    async fn pre<E>(
        &self,
        request: &mut reqwest::Request,
        info: &OperationInfo,
    ) -> Result<(), Error<E>> {
        let calls = self.calls.clone();
        let mut pending_once = false;
        futures::future::poll_fn(|cx| {
            if pending_once {
                Poll::Ready(())
            } else {
                pending_once = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
        calls.set(calls.get() + 1);
        assert_eq!(info.operation_id, "probe");
        request
            .headers_mut()
            .insert("x-probe", "yes".parse().unwrap());
        Ok(())
    }

    async fn post<E>(
        &self,
        _result: &reqwest::Result<reqwest::Response>,
        info: &OperationInfo,
    ) -> Result<(), Error<E>> {
        let calls = self.calls.clone();
        futures::future::pending::<()>().await;
        calls.set(calls.get() + 1);
        assert_eq!(info.operation_id, "probe");
        Ok(())
    }

    async fn exec(
        &self,
        request: reqwest::Request,
        info: &OperationInfo,
    ) -> reqwest::Result<reqwest::Response> {
        let calls = self.calls.clone();
        futures::future::ready(()).await;
        calls.set(calls.get() + 1);
        assert_eq!(request.method(), reqwest::Method::GET);
        assert_eq!(info.operation_id, "probe");
        Ok(http::Response::new("overridden").into())
    }
}

// Generated clients use a reference implementation for the default hooks.
impl ClientHooks<Rc<Cell<usize>>> for &Client {}

fn request() -> reqwest::Request {
    reqwest::Request::new(reqwest::Method::GET, "http://127.0.0.1".parse().unwrap())
}

fn info() -> OperationInfo {
    OperationInfo {
        operation_id: "probe",
    }
}

fn generic_pre<'a, 'b, 'c, T, E: 'a>(
    client: &'a T,
    request: &'b mut reqwest::Request,
    info: &'c OperationInfo,
) -> impl Future<Output = Result<(), Error<E>>> + 'a
where
    T: ClientHooks<Rc<Cell<usize>>> + 'a,
    'b: 'a,
    'c: 'a,
{
    client.pre::<E>(request, info)
}

fn generic_post<'a, 'b, 'c, T, E: 'a>(
    client: &'a T,
    result: &'b reqwest::Result<reqwest::Response>,
    info: &'c OperationInfo,
) -> impl Future<Output = Result<(), Error<E>>> + 'a
where
    T: ClientHooks<Rc<Cell<usize>>> + 'a,
    'b: 'a,
    'c: 'a,
{
    client.post::<E>(result, info)
}

#[test]
fn downstream_async_impl_supports_non_send_futures_and_distinct_borrows() {
    let client = Client::new();
    let info = info();
    let mut request = request();
    let future = generic_pre::<_, Rc<()>>(&client, &mut request, &info);
    assert_eq!(client.calls.get(), 0);
    block_on(future).unwrap();
    assert_eq!(client.calls.get(), 1);
    assert_eq!(request.headers()["x-probe"], "yes");
    let response = Ok(http::Response::new("body").into());
    let future = generic_post::<_, Rc<()>>(&client, &response, &info);
    drop(future);
    assert_eq!(client.calls.get(), 1);
}

#[test]
fn defaults_complete_without_side_effects_and_exec_is_lazy() {
    let client = Client::new();
    let reference = &client;
    let info = info();
    let mut request = request();
    block_on(generic_pre::<_, Rc<()>>(&reference, &mut request, &info)).unwrap();
    let response = Ok(http::Response::new("body").into());
    block_on(generic_post::<_, Rc<()>>(&reference, &response, &info)).unwrap();
    let future = <&Client as ClientHooks<Rc<Cell<usize>>>>::exec(&reference, request, &info);
    assert_eq!(client.calls.get(), 0);
    drop(future);
    assert_eq!(client.calls.get(), 0);
}

#[test]
fn cancellation_after_poll_releases_capture_without_running_continuation() {
    let client = Client::new();
    let info = info();
    let response = Ok(http::Response::new("body").into());
    {
        let mut future = pin!(client.post::<Rc<()>>(&response, &info));
        let mut context = Context::from_waker(futures::task::noop_waker_ref());
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        assert_eq!(Rc::strong_count(client.inner()), 2);
    }
    assert_eq!(Rc::strong_count(client.inner()), 1);
    assert_eq!(client.calls.get(), 0);
}

#[test]
fn downstream_async_exec_impl_is_lazy_and_dispatches() {
    let client = Client::new();
    let info = info();
    let future = client.exec(request(), &info);
    assert_eq!(client.calls.get(), 0);
    let response = block_on(future).unwrap();
    assert_eq!(client.calls.get(), 1);
    assert_eq!(block_on(response.text()).unwrap(), "overridden");
}

#[test]
fn default_exec_invokes_client_only_on_poll() {
    let client = Client::new();
    let reference = &client;
    let info = info();
    // An unsupported scheme exercises default dispatch without network I/O.
    let request = reqwest::Request::new(reqwest::Method::GET, "ftp://127.0.0.1".parse().unwrap());
    let future = <&Client as ClientHooks<Rc<Cell<usize>>>>::exec(&reference, request, &info);
    assert_eq!(client.calls.get(), 0);
    assert!(block_on(future).is_err());
    assert_eq!(client.calls.get(), 1);
}

#[test]
fn error_type_can_borrow_non_static_data() {
    fn run<'a>(client: &Client, value: &'a str) -> &'a str {
        let info = info();
        let mut request = request();
        block_on(generic_pre::<_, &'a str>(client, &mut request, &info)).unwrap();
        value
    }
    let client = Client::new();
    let value = String::from("borrowed");
    assert_eq!(run(&client, &value), "borrowed");
}

#[test]
fn dropping_unpolled_default_exec_releases_owned_request_body() {
    struct Guard(Arc<AtomicUsize>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let guard = Guard(drops.clone());
    let body = reqwest::Body::wrap_stream(futures::stream::once(async move {
        let _guard = guard;
        futures::future::pending::<Result<Vec<u8>, std::io::Error>>().await
    }));
    let client = Client::new();
    let reference = &client;
    let info = info();
    let mut request = request();
    *request.body_mut() = Some(body);
    let future = <&Client as ClientHooks<Rc<Cell<usize>>>>::exec(&reference, request, &info);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(future);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(client.calls.get(), 0);
}
