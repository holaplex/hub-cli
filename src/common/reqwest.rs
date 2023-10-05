use std::{fmt, marker::PhantomData};

use anyhow::{Context, Result};
use graphql_client::{GraphQLQuery, Response as GraphQlResponse};
use reqwest::{Client, Response, StatusCode};

use super::graphql;

pub trait ClientExt {
    fn graphql<Q: GraphQLQuery>(&self) -> GraphQlClient<Q>;
}

impl ClientExt for Client {
    #[inline]
    fn graphql<Q: GraphQLQuery>(&self) -> GraphQlClient<Q> { GraphQlClient(self, PhantomData) }
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct GraphQlClient<'a, Q>(&'a Client, PhantomData<&'a Q>);

impl<'a, Q: GraphQLQuery> GraphQlClient<'a, Q> {
    // Avoid using the async_trait Box<dyn Future> hack by just wrapping Client in a newtype
    pub async fn post_unchecked<U: reqwest::IntoUrl, F: FnOnce() -> D + Clone, D: fmt::Display>(
        self,
        url: U,
        vars: Q::Variables,
        name: F,
    ) -> Result<GraphQlResponse<Q::ResponseData>> {
        self.0
            .post(url)
            .json(&Q::build_query(vars))
            .send()
            .await
            .error_for_hub_status(name.clone())?
            .json::<GraphQlResponse<Q::ResponseData>>()
            .await
            .with_context(|| format!("Error parsing JSON from {}", name()))
    }

    #[inline]
    pub async fn post<U: reqwest::IntoUrl, F: FnOnce() -> D + Clone, D: fmt::Display>(
        self,
        url: U,
        vars: Q::Variables,
        name: F,
    ) -> Result<graphql::CheckedResponse<Q::ResponseData>>
    where
        Q::ResponseData: fmt::Debug,
    {
        graphql::result_for_errors(self.post_unchecked(url, vars, name.clone()).await?, name)
    }
}

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
