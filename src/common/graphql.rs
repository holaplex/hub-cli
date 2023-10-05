use std::{
    collections::HashMap,
    fmt::{self, Write},
};

use anyhow::{anyhow, Result};
use graphql_client::Response;

#[allow(clippy::upper_case_acronyms)]
pub type UUID = uuid::Uuid;

#[derive(Debug)]
pub struct CheckedResponse<T> {
    pub data: T,
    pub extensions: Option<HashMap<String, serde_json::Value>>,
}

pub fn result_for_errors<T: fmt::Debug, F: FnOnce() -> D + Clone, D: fmt::Display>(
    res: Response<T>,
    name: F,
) -> Result<CheckedResponse<T>> {
    log::trace!("GraphQL response for {}: {res:?}", (name.clone())());

    let Response {
        data,
        errors,
        extensions,
    } = res;

    if let Some(data) = data {
        format_errors(errors, (), |s| {
            log::warn!("{} returned one or more errors:{s}", name());
        });

        Ok(CheckedResponse { data, extensions })
    } else {
        let n = name.clone();
        Err(format_errors(errors, None, |s| {
            Some(anyhow!("{} returned one or more errors:{s}", n()))
        })
        .unwrap_or_else(|| anyhow!("{} returned no data", name())))
    }
}

pub fn format_errors<T>(
    errors: Option<Vec<graphql_client::Error>>,
    ok: T,
    f: impl FnOnce(String) -> T,
) -> T {
    let mut errs = errors.into_iter().flatten().peekable();

    if errs.peek().is_some() {
        let mut s = String::new();

        for err in errs {
            write!(s, "\n  {err}").unwrap();
        }

        f(s)
    } else {
        ok
    }
}
