//! Describing a failed HTTP request for a user.

/// The error and every cause under it, without the URL. reqwest's own message
/// stops at "error sending request", which hides whether DNS, the connection,
/// or TLS failed.
pub fn describe(err: reqwest::Error) -> String {
    let err = err.without_url();
    let mut message = err.to_string();
    let mut source = std::error::Error::source(&err);
    while let Some(inner) = source {
        let text = inner.to_string();
        if !message.ends_with(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = inner.source();
    }
    message
}
