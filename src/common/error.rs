pub(crate) fn format_errors<T>(
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
use std::fmt::Write;
