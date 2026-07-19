use anyhow::{bail, Result};
use http::{HeaderMap, HeaderName, HeaderValue, Method};
use url::Url;

pub fn is_hop_by_hop_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
    )
}

pub fn headers_to_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    let connection_headers = headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|name| name.trim().to_ascii_lowercase())
        .collect::<std::collections::HashSet<_>>();
    headers
        .iter()
        .filter(|(name, _)| {
            !is_hop_by_hop_header(name) && !connection_headers.contains(name.as_str())
        })
        .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_string())))
        .collect()
}

pub fn pairs_to_headers(pairs: Vec<(String, String)>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        if let (Ok(name), Ok(value)) = (name.parse::<HeaderName>(), HeaderValue::from_str(&value)) {
            headers.append(name, value);
        }
    }
    headers
}

pub fn join_upstream_url(upstream: &Url, path_and_query: &str) -> Result<Url> {
    if !path_and_query.starts_with('/') || path_and_query.contains("\\") {
        bail!("invalid request path")
    }
    let mut url = upstream.clone();
    let (path, query) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""));
    url.set_path(path);
    url.set_query((!query.is_empty()).then_some(query));
    Ok(url)
}

pub fn is_supported_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET
            | Method::POST
            | Method::PUT
            | Method::PATCH
            | Method::DELETE
            | Method::HEAD
            | Method::OPTIONS
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_and_joins() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ok", "yes".parse().unwrap());
        headers.insert("connection", "close".parse().unwrap());
        let pairs = headers_to_pairs(&headers);
        assert_eq!(pairs_to_headers(pairs).get("x-ok").unwrap(), "yes");
        let u = Url::parse("https://example.com").unwrap();
        assert_eq!(
            join_upstream_url(&u, "/foo?x=1").unwrap().as_str(),
            "https://example.com/foo?x=1"
        );
        assert!(is_supported_method(&Method::GET));
        assert!(!is_supported_method(&Method::CONNECT));
        assert_eq!(
            join_upstream_url(&u, "/").unwrap().as_str(),
            "https://example.com/"
        );
    }

    #[test]
    fn filters_headers_nominated_by_connection() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", "x-private, close".parse().unwrap());
        headers.insert("x-private", "secret".parse().unwrap());
        headers.insert("x-end-to-end", "yes".parse().unwrap());

        let pairs = headers_to_pairs(&headers);
        assert!(!pairs.iter().any(|(name, _)| name == "x-private"));
        assert!(pairs
            .iter()
            .any(|(name, value)| name == "x-end-to-end" && value == "yes"));
    }
}
