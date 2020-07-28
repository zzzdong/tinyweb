use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use route_recognizer::{Params, Router as MethodRouter};

use hyper::service::{make_service_fn, service_fn};

macro_rules! register_method {
    ($method_name: ident, $method_def: expr) => {
        pub fn $method_name(&mut self, path: impl AsRef<str>, handler: impl HTTPHandler) {
            self.register($method_def, path, handler)
        }
    };
}

#[derive(Debug)]
pub struct Error(String);

impl Error {
    fn new(msg: impl ToString) -> Self {
        Error(msg.to_string())
    }
}

pub type HyperRequest = hyper::Request<hyper::Body>;
pub type Response = hyper::Response<hyper::Body>;

pub struct RequestCtx {
    pub request: HyperRequest,
    pub params: Params,
    pub remote_addr: SocketAddr,
}

pub struct ResponseBuiler;

impl ResponseBuiler {
    pub fn with_text(text: impl ToString) -> Response {
        hyper::Response::builder()
            .header(
                "Content-type".parse::<hyper::header::HeaderName>().unwrap(),
                "text/plain; charset=UTF-8"
                    .parse::<hyper::header::HeaderValue>()
                    .unwrap(),
            )
            .body(hyper::Body::from(text.to_string()))
            .unwrap()
    }

    pub fn with_html(text: impl ToString) -> Response {
        hyper::Response::builder()
            .header(
                "Content-type".parse::<hyper::header::HeaderName>().unwrap(),
                "text/html; charset=UTF-8"
                    .parse::<hyper::header::HeaderValue>()
                    .unwrap(),
            )
            .body(hyper::Body::from(text.to_string()))
            .unwrap()
    }

    pub fn with_status(status: hyper::StatusCode) -> Response {
        hyper::Response::builder()
            .status(status)
            .body(hyper::Body::empty())
            .unwrap()
    }
}

#[async_trait::async_trait]
pub trait HTTPHandler: Send + Sync + 'static {
    async fn handle(&self, ctx: RequestCtx) -> Response;
}

type BoxHTTPHandler = Box<dyn HTTPHandler>;

#[async_trait::async_trait]
impl<F: Send + Sync + 'static, Fut> HTTPHandler for F
where
    F: Fn(RequestCtx) -> Fut,
    Fut: Future<Output = Response> + Send + 'static,
{
    async fn handle(&self, ctx: RequestCtx) -> Response {
        self(ctx).await
    }
}

type Router = HashMap<String, MethodRouter<BoxHTTPHandler>>;

#[async_trait::async_trait]
pub trait Middleware: Send + Sync + 'static {
    async fn handle<'a>(&'a self, ctx: RequestCtx, next: Next<'a>) -> Response;
}

#[allow(missing_debug_implementations)]
pub struct Next<'a> {
    pub(crate) endpoint: &'a dyn HTTPHandler,
    pub(crate) next_middleware: &'a [Arc<dyn Middleware>],
}

impl<'a> Next<'a> {
    /// Asynchronously execute the remaining middleware chain.
    pub async fn run(mut self, ctx: RequestCtx) -> Response {
        if let Some((current, next)) = self.next_middleware.split_first() {
            self.next_middleware = next;
            current.handle(ctx, self).await
        } else {
            (self.endpoint).handle(ctx).await
        }
    }
}

pub struct AccessLog;

#[async_trait::async_trait]
impl Middleware for AccessLog {
    async fn handle<'a>(&'a self, ctx: RequestCtx, next: Next<'a>) -> Response {
        let start = Instant::now();
        let method = ctx.request.method().to_string();
        let path = ctx.request.uri().path().to_string();
        let remote_addr = ctx.remote_addr;
        let res = next.run(ctx).await;
        println!(
            "{} {:?} {} {} {}ms",
            method,
            path,
            res.status().as_str(),
            remote_addr,
            start.elapsed().as_millis()
        );
        res
    }
}

pub struct Server {
    router: Router,
    middlewares: Vec<Arc<dyn Middleware>>,
}

impl Server {
    pub fn new() -> Self {
        Server {
            router: HashMap::new(),
            middlewares: Vec::new(),
        }
    }

    pub fn register(
        &mut self,
        method: impl ToString,
        path: impl AsRef<str>,
        handler: impl HTTPHandler,
    ) {
        let method = method.to_string().to_uppercase();
        self.router
            .entry(method)
            .or_insert_with(MethodRouter::new)
            .add(path.as_ref(), Box::new(handler));
    }

    register_method!(get, "GET");
    register_method!(head, "HEAD");
    register_method!(post, "POST");
    register_method!(put, "PUT");
    register_method!(delete, "DELETE");
    register_method!(connect, "CONNECT");
    register_method!(options, "OPTIONS");
    register_method!(trace, "TRACE");
    register_method!(patch, "PATCH");

    pub fn middleware(&mut self, middleware: impl Middleware) {
        self.middlewares.push(Arc::new(middleware));
    }

    pub async fn run(self, addr: SocketAddr) -> Result<(), Error> {
        let Self {
            router,
            middlewares,
        } = self;

        let router = Arc::new(router);
        let middlewares = Arc::new(middlewares);

        let make_svc = make_service_fn(|conn: &hyper::server::conn::AddrStream| {
            let router = router.clone();
            let middlewares = middlewares.clone();
            let remote_addr = conn.remote_addr();

            async move {
                Ok::<_, Infallible>(service_fn(move |req: HyperRequest| {
                    let router = router.clone();
                    let middlewares = middlewares.clone();

                    async move {
                        let method = &req.method().as_str().to_uppercase();

                        let mut req_params = Params::new();
                        let endpoint = match router.get(method) {
                            Some(router) => match router.recognize(req.uri().path()) {
                                Ok(route_recognizer::Match { handler, params }) => {
                                    req_params = params;
                                    &**handler
                                }
                                Err(_) => &Self::handle_not_found,
                            },
                            None => &Self::handle_not_found,
                        };

                        let next = Next {
                            endpoint,
                            next_middleware: &middlewares,
                        };

                        let ctx = RequestCtx {
                            request: req,
                            params: req_params,
                            remote_addr,
                        };

                        let resp = next.run(ctx).await;

                        Ok::<_, Infallible>(resp)
                    }
                }))
            }
        });

        let server = hyper::Server::bind(&addr).serve(make_svc);

        server
            .await
            .map_err(|e| Error::new(format!("server run error: {:?}", e)))?;

        Ok(())
    }

    async fn handle_not_found(_ctx: RequestCtx) -> Response {
        ResponseBuiler::with_status(hyper::StatusCode::NOT_FOUND)
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}
