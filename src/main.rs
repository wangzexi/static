use std::{env, net::SocketAddr, time::Duration};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{
        HeaderMap, Method, Request, StatusCode,
        header::{self, HeaderName, HeaderValue},
    },
    response::Response,
};
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, percent_encode};
use reqwest::Client;
use tracing::{error, info};
use url::Url;

const DEFAULT_PORT: u16 = 8080;
const URI_COMPONENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(92)
    .add(b']')
    .add(b'^')
    .add(96)
    .add(b'{')
    .add(b'|')
    .add(b'}');
const FORWARDED_REQUEST_HEADERS: &[&str] = &[
    "if-match",
    "if-modified-since",
    "if-none-match",
    "if-range",
    "range",
];
const FORWARDED_RESPONSE_HEADERS: &[&str] = &[
    "accept-ranges",
    "content-encoding",
    "content-length",
    "content-range",
    "content-type",
    "etag",
    "last-modified",
];

#[derive(Clone)]
struct AppState {
    endpoint: String,
    bucket: String,
    client: Client,
}

impl AppState {
    fn from_env() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let endpoint = normalize_endpoint(
            &env::var("S3_ENDPOINT")
                .unwrap_or_else(|_| "http://minio.minio.svc.cluster.local:9000".to_owned()),
        )?;
        let bucket =
            normalize_bucket(&env::var("S3_BUCKET").unwrap_or_else(|_| "sites".to_owned()))?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()?;

        Ok(Self {
            endpoint,
            bucket,
            client,
        })
    }
}

fn normalize_endpoint(value: &str) -> Result<String, String> {
    let mut endpoint =
        Url::parse(value).map_err(|error| format!("invalid S3_ENDPOINT: {error}"))?;
    if endpoint.scheme() != "http" && endpoint.scheme() != "https" {
        return Err("S3_ENDPOINT must use http or https".to_owned());
    }
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    Ok(endpoint.to_string().trim_end_matches('/').to_owned())
}

fn normalize_bucket(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let valid = value.len() >= 3
        && value.len() <= 63
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-' || *byte == b'.'
        })
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && (bytes[value.len() - 1].is_ascii_lowercase() || bytes[value.len() - 1].is_ascii_digit());
    if !valid {
        return Err("S3_BUCKET must be a valid S3 bucket name".to_owned());
    }
    Ok(value.to_owned())
}

pub fn normalize_host(value: Option<&str>) -> Option<String> {
    let value = value?;
    let mut host = value.trim().to_ascii_lowercase();
    if host.starts_with('[') {
        return None;
    }
    if let Some((without_port, _)) = host.split_once(':') {
        host = without_port.to_owned();
    }
    if host.ends_with('.') {
        host.pop();
    }

    if host.is_empty()
        || host.len() > 253
        || !host.contains('.')
        || !host.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'-'
        })
        || host.contains("..")
        || host.split('.').any(|label| {
            label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-')
        })
    {
        return None;
    }
    Some(host)
}

fn decode_pathname(pathname: &str) -> Option<String> {
    let pathname = percent_decode_str(pathname)
        .decode_utf8()
        .ok()?
        .into_owned();
    if pathname.contains('\0')
        || pathname.contains('\\')
        || pathname.split('/').any(|segment| segment == "..")
    {
        return None;
    }
    Some(pathname)
}

fn encode_object_key(host: &str, pathname: &str) -> String {
    std::iter::once(host)
        .chain(pathname.split('/').filter(|part| !part.is_empty()))
        .map(|part| percent_encode(part.as_bytes(), URI_COMPONENT).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn has_hashed_filename(pathname: &str) -> bool {
    let filename = pathname.rsplit('/').next().unwrap_or_default().as_bytes();
    for index in 0..filename.len() {
        if filename[index] != b'.' && filename[index] != b'-' {
            continue;
        }
        let start = index + 1;
        for end in (start + 8)..=filename.len() {
            let separator = end == filename.len() || filename[end] == b'.' || filename[end] == b'-';
            if !separator {
                continue;
            }
            if end > start
                && end - start >= 8
                && end < filename.len()
                && filename[start..end]
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-')
            {
                return true;
            }
            break;
        }
    }
    false
}

fn cache_control_for(pathname: &str, upstream: Option<&str>) -> String {
    if pathname.ends_with(".html") {
        "no-cache".to_owned()
    } else if pathname.starts_with("/assets/")
        || pathname.starts_with("/_astro/")
        || has_hashed_filename(pathname)
    {
        "public, max-age=31536000, immutable".to_owned()
    } else {
        upstream
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "public, max-age=3600".to_owned())
    }
}

async fn fetch_object(
    state: &AppState,
    method: &Method,
    request_headers: &HeaderMap,
    host: &str,
    pathname: &str,
) -> Result<reqwest::Response, reqwest::Error> {
    let object_key = encode_object_key(host, pathname);
    let object_url = format!("{}/{}/{}", state.endpoint, state.bucket, object_key);
    let upstream_method = if method == Method::HEAD {
        reqwest::Method::HEAD
    } else {
        reqwest::Method::GET
    };
    let mut request = state.client.request(upstream_method, object_url);
    for name in FORWARDED_REQUEST_HEADERS {
        if let Some(value) = request_headers.get(*name) {
            request = request.header(*name, value.clone());
        }
    }
    request.send().await
}

fn upstream_response(
    upstream: reqwest::Response,
    method: &Method,
    pathname: &str,
    status: StatusCode,
) -> Response {
    let mut headers = HeaderMap::new();
    for name in FORWARDED_RESPONSE_HEADERS {
        if let Some(value) = upstream.headers().get(*name) {
            headers.insert(HeaderName::from_static(name), value.clone());
        }
    }
    let cache_control = upstream
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok());
    if let Ok(value) = HeaderValue::from_str(&cache_control_for(pathname, cache_control)) {
        headers.insert(header::CACHE_CONTROL, value);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );

    let body = if method == Method::HEAD || status == StatusCode::NOT_MODIFIED {
        Body::empty()
    } else {
        Body::from_stream(upstream.bytes_stream())
    };
    let mut response = Response::builder()
        .status(status)
        .body(body)
        .expect("valid upstream response");
    *response.headers_mut() = headers;
    with_cors(response)
}

fn plain(status: StatusCode, message: &str) -> Response {
    let mut response = Response::new(Body::from(format!("{message}\n")));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    with_cors(response)
}

fn redirect(location: &str) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::PERMANENT_REDIRECT;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    with_cors(response)
}

fn preflight() -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    with_cors(response)
}

fn with_cors(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, HEAD, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("86400"),
    );
    response
}

fn upstream_status(status: reqwest::StatusCode) -> StatusCode {
    StatusCode::from_u16(status.as_u16()).expect("valid HTTP status")
}

async fn handler(State(state): State<AppState>, request: Request<Body>) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    if method == Method::OPTIONS {
        return preflight();
    }
    if uri.path() == "/healthz" || uri.path() == "/readyz" {
        return plain(StatusCode::OK, "ok");
    }
    if method != Method::GET && method != Method::HEAD {
        let mut response = plain(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return response;
    }

    let host = match normalize_host(
        request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok()),
    ) {
        Some(host) => host,
        None => return plain(StatusCode::BAD_REQUEST, "bad request"),
    };
    let pathname = match decode_pathname(uri.path()) {
        Some(pathname) => pathname,
        None => return plain(StatusCode::BAD_REQUEST, "bad request"),
    };
    let requested_path = if pathname.ends_with('/') {
        format!("{pathname}index.html")
    } else {
        pathname.clone()
    };
    let request_headers = request.headers().clone();

    let mut upstream =
        match fetch_object(&state, &method, &request_headers, &host, &requested_path).await {
            Ok(response) => response,
            Err(error) => {
                error!(%error, "storage request failed");
                return plain(StatusCode::BAD_GATEWAY, "storage unavailable");
            }
        };

    if upstream.status() == reqwest::StatusCode::NOT_FOUND
        && !pathname.ends_with('/')
        && !pathname
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .contains('.')
    {
        let _ = upstream.bytes().await;
        let index_path = format!("{pathname}/index.html");
        match fetch_object(&state, &method, &request_headers, &host, &index_path).await {
            Ok(index) if index.status().is_success() => {
                let mut location = uri.path().to_owned();
                location.push('/');
                if let Some(query) = uri.query() {
                    location.push('?');
                    location.push_str(query);
                }
                let _ = index.bytes().await;
                return redirect(&location);
            }
            Ok(index) => {
                let _ = index.bytes().await;
            }
            Err(error) => {
                error!(%error, "storage directory check failed");
            }
        }
        upstream =
            match fetch_object(&state, &method, &request_headers, &host, &requested_path).await {
                Ok(response) => response,
                Err(error) => {
                    error!(%error, "storage request failed");
                    return plain(StatusCode::BAD_GATEWAY, "storage unavailable");
                }
            };
    }

    if upstream.status() == reqwest::StatusCode::NOT_FOUND {
        let _ = upstream.bytes().await;
        match fetch_object(&state, &method, &request_headers, &host, "/404.html").await {
            Ok(not_found) if not_found.status().is_success() => {
                return upstream_response(not_found, &method, "/404.html", StatusCode::NOT_FOUND);
            }
            Ok(not_found) => {
                let _ = not_found.bytes().await;
            }
            Err(error) => {
                error!(%error, "custom 404 request failed");
            }
        }
        return plain(StatusCode::NOT_FOUND, "not found");
    }

    let status = upstream_status(upstream.status());
    if !upstream.status().is_success() && upstream.status() != reqwest::StatusCode::NOT_MODIFIED {
        let _ = upstream.bytes().await;
        return plain(StatusCode::BAD_GATEWAY, "storage unavailable");
    }
    upstream_response(upstream, &method, &requested_path, status)
}

fn app(state: AppState) -> Router {
    Router::new().fallback(handler).with_state(state)
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| "web_static=info".to_owned()))
        .init();

    let port = env::var("PORT")
        .unwrap_or_else(|_| DEFAULT_PORT.to_string())
        .parse::<u16>()?;
    let state = AppState::from_env()?;
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    info!(%address, bucket = %state.bucket, "web static gateway listening");
    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use tower::ServiceExt;

    #[test]
    fn normalizes_hosts_and_rejects_unsafe_values() {
        assert_eq!(
            normalize_host(Some("EXAMPLE.ME:443")).as_deref(),
            Some("example.me")
        );
        assert_eq!(
            normalize_host(Some("example.me.")).as_deref(),
            Some("example.me")
        );
        assert_eq!(normalize_host(Some("localhost")), None);
        assert_eq!(normalize_host(Some("../example.me")), None);
    }

    #[test]
    fn rejects_unsafe_paths() {
        assert!(decode_pathname("/a/%2e%2e/b").is_none());
        assert!(decode_pathname("/a/%00").is_none());
        assert_eq!(
            decode_pathname("/articles/%E4%B8%AD%E6%96%87/").as_deref(),
            Some("/articles/中文/")
        );
    }

    #[test]
    fn maps_hosts_and_paths_to_object_keys() {
        assert_eq!(
            encode_object_key("example.me", "/articles/hello/index.html"),
            "example.me/articles/hello/index.html"
        );
    }

    #[test]
    fn applies_cache_policy() {
        assert_eq!(cache_control_for("/index.html", None), "no-cache");
        assert_eq!(
            cache_control_for("/assets/app.Dewnqifn.js", None),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            cache_control_for("/robots.txt", None),
            "public, max-age=3600"
        );
    }

    async fn mock_storage() -> (String, tokio::task::JoinHandle<()>) {
        async fn serve_object(request: Request<Body>) -> Response {
            let path = percent_decode_str(request.uri().path())
                .decode_utf8()
                .expect("mock path is valid UTF-8");
            let object = match path.as_ref() {
                "/sites/example.me/index.html" => Some(("<h1>home</h1>", r#""home-v1""#)),
                "/sites/example.me/articles/hello/index.html" => {
                    Some(("<h1>hello</h1>", r#""hello-v1""#))
                }
                "/sites/example.me/404.html" => Some(("<h1>missing</h1>", r#""404-v1""#)),
                _ => None,
            };
            let Some((body, etag)) = object else {
                return Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::empty())
                    .unwrap();
            };
            if request
                .headers()
                .get(header::IF_NONE_MATCH)
                .and_then(|value| value.to_str().ok())
                == Some(etag)
            {
                return Response::builder()
                    .status(StatusCode::NOT_MODIFIED)
                    .header(header::ETAG, etag)
                    .body(Body::empty())
                    .unwrap();
            }
            let body = if request.method() == Method::HEAD {
                Body::empty()
            } else {
                Body::from(body)
            };
            Response::builder()
                .header(header::CONTENT_TYPE, "text/html")
                .header(header::ETAG, etag)
                .body(body)
                .unwrap()
        }

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, Router::new().fallback(serve_object)).await;
        });
        (format!("http://{address}"), handle)
    }

    #[tokio::test]
    async fn serves_static_gateway_behaviour() {
        let (endpoint, storage) = mock_storage().await;
        let state = AppState {
            endpoint,
            bucket: "sites".to_owned(),
            client: Client::new(),
        };
        let gateway = app(state);

        let response = gateway
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::HOST, "example.me")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "*"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache"
        );
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .as_ref(),
            b"<h1>home</h1>"
        );

        let response = gateway
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/assets/example.js")
                    .method(Method::OPTIONS)
                    .header(header::HOST, "example.me")
                    .header(header::ORIGIN, "https://cheer.world")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "*"
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_METHODS)
                .unwrap(),
            "GET, HEAD, OPTIONS"
        );

        let response = gateway
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/articles/hello?from=test")
                    .header(header::HOST, "example.me")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "/articles/hello/?from=test"
        );

        let response = gateway
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .method(Method::HEAD)
                    .header(header::HOST, "example.me")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );

        let response = gateway
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/missing")
                    .header(header::HOST, "example.me")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .as_ref(),
            b"<h1>missing</h1>"
        );

        let response = gateway
            .oneshot(
                Request::builder()
                    .uri("/")
                    .method(Method::PUT)
                    .header(header::HOST, "example.me")
                    .body(Body::from("no"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);

        storage.abort();
    }
}
