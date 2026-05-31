//! Scheme validation for the web-facing tools.
//!
//! One source of scheme policy so every tool that takes a user-supplied web
//! address (`fetch_url`, and any agent that hands one to `desktop_launch`)
//! rejects the same things (`file://`, `ftp://`, …) identically. This is a
//! web concern, not an application one — it knows nothing about browsers.

/// Validate a URL for the web-facing tools: it must parse and use http or
/// https.
pub(crate) fn validate_web_url(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid url {url:?}: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        other => Err(format!(
            "only http/https URLs are supported, got scheme {other:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_http_and_https() {
        assert!(validate_web_url("http://example.com").is_ok());
        assert!(validate_web_url("https://example.com/x?q=1").is_ok());
    }

    #[test]
    fn validate_rejects_other_schemes() {
        for bad in ["file:///etc/passwd", "ftp://h/x", "data:text/plain,hi"] {
            let err = validate_web_url(bad).unwrap_err();
            assert!(err.contains("http"), "{bad}: {err}");
        }
        assert!(validate_web_url("not a url").is_err());
    }
}
