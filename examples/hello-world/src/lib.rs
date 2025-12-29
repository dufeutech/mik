//! hello-world - A mik HTTP handler

#[allow(warnings)]
mod bindings;

use bindings::exports::mik::core::handler::{self, Guest, Response};
use mik_sdk::prelude::*;

routes! {
    GET "/" | "" => home,
    GET "/health" => health,
}

fn home(_req: &Request) -> Response {
    ok!({
        "service": "hello-world",
        "message": "Hello from mik!"
    })
}

fn health(_req: &Request) -> Response {
    ok!({ "status": "healthy" })
}
