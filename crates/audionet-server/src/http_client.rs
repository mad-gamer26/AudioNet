//! A small HTTP client for push notifications: HTTPS with the operating
//! system's certificate check, HTTP/2 when the other side offers it (Apple's
//! push service requires it) and HTTP/1.1 otherwise. Plain HTTP only for
//! tests on this machine.

use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Request, Uri};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpStream;

const TIMEOUT: Duration = Duration::from_secs(15);
/// Answers are small; more than this is not read.
const MAX_BODY: usize = 64 * 1024;

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Plain HTTP: which protocol to speak (no negotiation without TLS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlainHttp {
    Http1,
    Http2,
}

fn tls_connector() -> Result<tokio_rustls::TlsConnector, String> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_platform_verifier()
        .map_err(|e| e.to_string())?
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

/// POSTs `body` to `url`. `plain` chooses the protocol for `http://` URLs.
pub async fn post(
    url: &str,
    headers: &[(&str, String)],
    body: Vec<u8>,
    plain: PlainHttp,
) -> Result<Response, String> {
    tokio::time::timeout(TIMEOUT, post_inner(url, headers, body, plain))
        .await
        .map_err(|_| format!("no answer from {url} in time"))?
}

async fn post_inner(
    url: &str,
    headers: &[(&str, String)],
    body: Vec<u8>,
    plain: PlainHttp,
) -> Result<Response, String> {
    let uri: Uri = url.parse().map_err(|e| format!("bad address {url}: {e}"))?;
    let https = match uri.scheme_str() {
        Some("https") => true,
        Some("http") => false,
        _ => return Err(format!("not an http(s) address: {url}")),
    };
    let host = uri
        .host()
        .ok_or_else(|| format!("no host in {url}"))?
        .to_owned();
    let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });
    let tcp = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| format!("could not connect to {host}: {e}"))?;
    let _ = tcp.set_nodelay(true);
    let path = uri.path_and_query().map_or("/", |p| p.as_str()).to_owned();
    let authority = uri
        .authority()
        .map(|a| a.as_str().to_owned())
        .unwrap_or(host.clone());
    let mut req = Request::post(path)
        .header("host", authority.clone())
        .header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, v.as_str());
    }
    let req = req
        .body(Full::new(Bytes::from(body)))
        .map_err(|e| e.to_string())?;

    if https {
        let name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|e| format!("bad host name {host}: {e}"))?;
        let tls = tls_connector()?
            .connect(name, tcp)
            .await
            .map_err(|e| format!("TLS with {host} failed: {e}"))?;
        let h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2".as_slice());
        send(TokioIo::new(tls), req, h2, "https", &authority).await
    } else {
        send(
            TokioIo::new(tcp),
            req,
            plain == PlainHttp::Http2,
            "http",
            &authority,
        )
        .await
    }
}

async fn send<T>(
    io: T,
    mut req: Request<Full<Bytes>>,
    h2: bool,
    scheme: &str,
    authority: &str,
) -> Result<Response, String>
where
    T: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let resp = if h2 {
        // HTTP/2 takes the authority from the URI, not a Host header.
        req.headers_mut().remove("host");
        *req.uri_mut() = format!("{scheme}://{authority}{}", req.uri())
            .parse()
            .map_err(|e: hyper::http::uri::InvalidUri| e.to_string())?;
        let (mut sender, conn) = hyper::client::conn::http2::handshake(TokioExecutor::new(), io)
            .await
            .map_err(|e| format!("HTTP/2 with {authority} failed: {e}"))?;
        tokio::spawn(conn);
        sender
            .send_request(req)
            .await
            .map_err(|e| format!("request to {authority} failed: {e}"))?
    } else {
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|e| format!("HTTP with {authority} failed: {e}"))?;
        tokio::spawn(conn);
        sender
            .send_request(req)
            .await
            .map_err(|e| format!("request to {authority} failed: {e}"))?
    };
    let status = resp.status().as_u16();
    let body = http_body_util::Limited::new(resp.into_body(), MAX_BODY)
        .collect()
        .await
        .map_err(|e| format!("reading the answer from {authority} failed: {e}"))?
        .to_bytes()
        .to_vec();
    Ok(Response { status, body })
}
