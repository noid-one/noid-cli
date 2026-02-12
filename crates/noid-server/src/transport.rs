use std::collections::HashMap;

/// Parsed request context — handlers never touch tiny_http types directly.
pub struct RequestContext {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub remote_addr: String,
    pub forwarded_for: Option<String>,
}

/// Response to send back.
pub struct ResponseBuilder {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl ResponseBuilder {
    pub fn json(status: u16, body: &impl serde::Serialize) -> Self {
        let body = serde_json::to_vec(body).unwrap_or_default();
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body,
        }
    }

    pub fn error(status: u16, msg: &str) -> Self {
        Self::json(status, &noid_types::ErrorResponse { error: msg.into() })
    }

    pub fn no_content() -> Self {
        Self {
            status: 204,
            headers: vec![],
            body: vec![],
        }
    }
}

/// Convert a tiny_http::Request into a RequestContext.
pub fn from_tiny_http(
    request: &mut tiny_http::Request,
    trust_forwarded_for: bool,
) -> RequestContext {
    let method = request.method().to_string();
    let path = request.url().to_string();
    let remote_addr = request.remote_addr().map(|a| a.to_string()).unwrap_or_default();

    let mut headers = HashMap::new();
    for h in request.headers() {
        headers.insert(
            h.field.as_str().as_str().to_lowercase(),
            h.value.as_str().to_string(),
        );
    }

    let forwarded_for = if trust_forwarded_for {
        headers.get("x-forwarded-for").cloned()
    } else {
        None
    };

    // Limit request body to 1 MB to prevent memory exhaustion
    const MAX_BODY_SIZE: usize = 1024 * 1024;
    let mut body = Vec::new();
    let reader = request.as_reader();
    let mut buf = [0u8; 8192];
    loop {
        match std::io::Read::read(reader, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                body.extend_from_slice(&buf[..n]);
                if body.len() > MAX_BODY_SIZE {
                    body.truncate(MAX_BODY_SIZE);
                    break;
                }
            }
            Err(_) => break,
        }
    }

    RequestContext {
        method,
        path,
        headers,
        body,
        remote_addr,
        forwarded_for,
    }
}

/// Convert a ResponseBuilder into a tiny_http::Response.
pub fn to_tiny_http_response(
    resp: ResponseBuilder,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let status = tiny_http::StatusCode(resp.status);
    let mut response = tiny_http::Response::new(
        status,
        vec![],
        std::io::Cursor::new(resp.body.clone()),
        Some(resp.body.len()),
        None,
    );
    for (key, value) in &resp.headers {
        if let Ok(header) = tiny_http::Header::from_bytes(key.as_bytes(), value.as_bytes()) {
            response.add_header(header);
        }
    }
    // Always add API version header
    if let Ok(h) = tiny_http::Header::from_bytes(b"X-Noid-Api-Version", b"1") {
        response.add_header(h);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_builder_json_sets_content_type() {
        let resp = ResponseBuilder::json(200, &serde_json::json!({"ok": true}));
        assert_eq!(resp.status, 200);
        assert!(resp.headers.iter().any(|(k, v)| k == "Content-Type" && v == "application/json"));
        let parsed: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(parsed["ok"], true);
    }

    #[test]
    fn response_builder_error_wraps_in_error_response() {
        let resp = ResponseBuilder::error(404, "not found");
        assert_eq!(resp.status, 404);
        let parsed: noid_types::ErrorResponse = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(parsed.error, "not found");
    }

    #[test]
    fn response_builder_no_content_has_empty_body() {
        let resp = ResponseBuilder::no_content();
        assert_eq!(resp.status, 204);
        assert!(resp.body.is_empty());
        assert!(resp.headers.is_empty());
    }

    #[test]
    fn response_builder_json_status_codes() {
        for code in [200, 201, 400, 401, 409, 500] {
            let resp = ResponseBuilder::json(code, &serde_json::json!({}));
            assert_eq!(resp.status, code);
        }
    }
}
