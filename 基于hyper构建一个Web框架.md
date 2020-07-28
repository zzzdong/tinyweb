基于hyper构建一个Web框架
====

> 本文尝试基于hyper构建一个简易的Web框架，主体实现是从`Tide`中摘抄的。

## 前言

作为一名`Rust`编程语言的爱好者，看到`Rust`生态在Web方面不断发展，越来越多有些的类库，框架层出不穷，心中满是欣喜。

在参考[`warp`](https://github.com/seanmonstar/warp)和[`Tide`](https://github.com/http-rs/tide)的代码之后，不禁想自己尝试一下实现一个Web框架，用来学习。于是便有了此文。

## 假想的Web框架

在常见的Web框架，我是比较喜欢[`Flask`](https://flask.palletsprojects.com/en/1.1.x/)的，简单，小巧，方便使用。这一次的实现也是朝着这个方向前进。

假想中的框架：

```rust
    let server = Server::new();
    server.get("/", index);
    server.register("GET", "/hello", hello_world);
    server.run("127.0.0.1:5000");

    fn index(req: Request) -> Response {
        Response::with_text("Welcome!")
    }
```

很简单，对吧。让我们朝这个目标出发。

## 基于成熟的hyper库

在`Rust`的Web生态中，[`hyper`](https://github.com/hyperium/hyper)是一个非常强大的HTTP实现库。起来一个简易的服务器，只需要几行代码：

```rust
async fn hello_world(_req: Request<Body>) -> Result<Response<Body>, Infallible> {
    Ok(Response::new("Hello, World".into()))
}

#[tokio::main]
async fn main() {
    // We'll bind to 127.0.0.1:3000
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    // A `Service` is needed for every connection, so this
    // creates one from our `hello_world` function.
    let make_svc = make_service_fn(|_conn| async {
        // service_fn converts our function into a `Service`
        Ok::<_, Infallible>(service_fn(hello_world))
    });

    let server = Server::bind(&addr).serve(make_svc);

    // Run this server for... forever!
    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
}
```

好啦，对比一下我们的假想的框架，基本的东西都有了，接下来慢慢补吧。

## 整理思路

在`hyper`的基础上开始，我们要做的是从每一个请求的处理开始，简单就是把例子中的hello_world修改为我们的总入口。

```rust
async fn handle_request(req: Request<Body>) -> Result<Response<Body>, Infallible> {
    // TODO：处理请求
    Ok(Response::new("Hello, World".into()))
}
```

好的，首先需要的是请求和响应的类型：

```rust
type HyperRequest = hyper::Request<hyper::Body>;
type HyperResponse = hyper::Response<hyper::Body>;

// 我们的请求体
pub struct RequestCtx {
    request: HyperRequest
}

// 直接使用hyper的Response
pub type Response = HyperResponse;

```

## 添加路由

一般Web框架，都会需要有个路由，就是根据url的不同自动派发到不同的处理函数。最简单的情况，基本是可以使用HashMap可以了，当然，这次中，我们是打算借鉴`Tide`的处理，使用[`route-recognizer`](https://github.com/http-rs/route-recognizer)来做这个事情。顺便一提，这个路由库的实现是走NFA的方式，不是常见的[`Radix tree`](https://en.wikipedia.org/wiki/Radix_tree)，代码挺值得一看的。

结果，出来的路由：

```rust
#[async_trait::async_trait]
pub trait HTTPHandler: Send + Sync + 'static {
    async fn handle(&self, ctx: RequestCtx) -> Response;
}

type BoxHTTPHandler = Box<dyn HTTPHandler>;

type Router = HashMap<String, route_recognizer::Router<BoxHTTPHandler>>;

```

好吧，我承认，一开始我是想着用`type BoxHTTPHandler = Box<dyn Fn(Request)->Response>`，折腾了很久，都没有成功，加上要支持async/await，在抄袭了`Tide`的实现后，才是可以使用。


## 需要一个中间件

中间件在Web框架中，可以用来做一些前置、后置处理。

```rust
#[async_trait::async_trait]
pub trait Middleware: Send + Sync + 'static {
    async fn handle<'a>(&'a self, ctx: RequestCtx, next: Next<'a>) -> Response;
}

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

```

中间件的实现，是直接拿了`Tide`的过来用，不解释，从代码可以看清楚思路。

## 加上`hyper`的使用

```rust
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
```

## 写个例子来使用

```rust
use tinyweb::{RequestCtx, Response, ResponseBuiler, Server, AccessLog};
use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    let addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let mut srv = Server::new();

    srv.middleware(AccessLog);

    srv.get("/", |_req| async move {
        hyper::Response::builder()
            .status(hyper::StatusCode::OK)
            .body(hyper::Body::from("Welcome!"))
            .unwrap()
    });
    srv.get("/hello/:name", hello);

    srv.run(addr).await.unwrap();
}

async fn hello(ctx: RequestCtx) -> Response {
    let name = ctx.params.find("name").unwrap_or("world");

    ResponseBuiler::with_text(format!("Hello {}!", name))
}
```
离开始的预期差不多了，勉强吧。

## 大功告成

完整的代码可以在[Github](https://github.com/zzzdong/tinyweb)上找到。