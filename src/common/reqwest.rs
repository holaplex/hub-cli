use std::fmt;

use anyhow::{Context, Result};
use reqwest::{Response, StatusCode};

pub trait ResponseExt: Sized {
    fn error_for_hub_status<F: FnOnce() -> D, D: fmt::Display>(self, name: F) -> Result<Response>;
}

impl<E> ResponseExt for Result<Response, E>
where Self: anyhow::Context<Response, E>
{
    fn error_for_hub_status<F: FnOnce() -> D, D: fmt::Display>(self, name: F) -> Result<Response> {
        let this = match self {
            Ok(r) => r,
            Err(e) => return Err(e).context(format!("Error sending {}", name())),
        };
        let status = this.status();

        this.error_for_status().with_context(|| match status {
            StatusCode::UNAUTHORIZED => format!(
                "{} returned Unauthorized, you may need to update your API token with `hub config \
                 token`",
                name()
            ),
            StatusCode::TOO_MANY_REQUESTS => format!(
                "{} exceeded the rate limit, try rerunning the operation later",
                name()
            ),
            _ => format!("{} returned an error", name()),
        })
    }
}
