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
