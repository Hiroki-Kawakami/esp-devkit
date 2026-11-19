/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! HTTP/1.1 and HTTP/2 client: connections reused per origin, redirects,
//! Basic and Digest authentication, decoded bodies.

mod auth;
mod body;
mod conn;
mod h1;
#[cfg(CONFIG_DEVKIT_NET_HTTP2)]
mod h2;
mod headers;
mod url;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::time::Duration;

use devkit_crypto::tls::{ClientConfig, Verify};
use devkit_crypto::x509::TrustStore;
use idf::sync::Mutex;
use idf::time::Instant;

use self::auth::Digest;
use self::body::{Carrier, Parts};
use self::conn::{Conn, Limits, Pool, Transport};
#[cfg(CONFIG_DEVKIT_NET_HTTP2)]
use self::h2::H2;
use self::url::{Origin, Url};
use crate::tcp_socket::{blocking, blocking_timeout};
use crate::{Error, Result, TcpStream, TlsStream};

pub use self::body::Response;
pub use self::headers::Headers;
#[cfg(test)]
pub(crate) use self::conn::Transport as TestTransport;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_REDIRECTS: u8 = 20;
const DEFAULT_IDLE: usize = 4;
const USER_AGENT: &str = "devkit_net";
/// Larger bodies, and streamed ones of unknown length, ask before they go.
const EXPECT_BYTES: u64 = 1024 * 1024;
const EXPECT_WAIT: Duration = Duration::from_secs(1);
/// Bodies up to this go out in one write with the head.
const COALESCE_BYTES: usize = 4096;

/// The HTTP version a response came in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Version {
    Http10,
    Http11,
    Http2,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
    Options,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
            Method::Options => "OPTIONS",
        }
    }

    fn sends_body(self) -> bool {
        matches!(self, Method::Post | Method::Put | Method::Patch)
    }
}

#[cfg(test)]
pub(crate) type Connector = Arc<dyn Fn(&str) -> Result<Transport> + Send + Sync>;

struct Inner {
    pool: Arc<Pool>,
    tls: Mutex<Option<Arc<ClientConfig>>>,
    connect_timeout: Option<Duration>,
    idle_timeout: Option<Duration>,
    timeout: Option<Duration>,
    redirects: u8,
    user_agent: String,
    #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
    windows: Windows,
    #[cfg(test)]
    connector: Option<Connector>,
}

/// HTTP/2 receive windows: what the server may send ahead, and so what a
/// stream and a connection hold at most unread.
#[cfg(CONFIG_DEVKIT_NET_HTTP2)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Windows {
    pub stream: u32,
    pub connection: u32,
}

/// Sends requests, keeping connections whose response was read to its end
/// for the next request to the same origin. Clones share them.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

pub struct ClientBuilder {
    tls: Option<Arc<ClientConfig>>,
    connect_timeout: Option<Duration>,
    idle_timeout: Option<Duration>,
    timeout: Option<Duration>,
    redirects: u8,
    max_idle: usize,
    user_agent: String,
    #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
    windows: Windows,
    #[cfg(test)]
    connector: Option<Connector>,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            tls: None,
            connect_timeout: Some(DEFAULT_TIMEOUT),
            idle_timeout: Some(DEFAULT_TIMEOUT),
            timeout: None,
            redirects: DEFAULT_REDIRECTS,
            max_idle: DEFAULT_IDLE,
            user_agent: String::from(USER_AGENT),
            #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
            windows: Windows { stream: 64 * 1024, connection: 128 * 1024 },
            #[cfg(test)]
            connector: None,
        }
    }
}

impl ClientBuilder {
    /// For https; without it, the default CA bundle, loaded at the first
    /// https request.
    pub fn tls(mut self, config: Arc<ClientConfig>) -> Self {
        self.tls = Some(config);
        self
    }

    /// Name lookup, TCP and TLS together. 30 s unless set; `None` waits as
    /// long as they take.
    pub fn connect_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Each wait for the server to take or send data. 30 s unless set.
    pub fn idle_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// From sending a request to the end of its response body, redirects
    /// included. None unless set.
    pub fn timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Redirects followed for one request, 20 unless set; 0 returns them.
    pub fn redirects(mut self, max: u8) -> Self {
        self.redirects = max;
        self
    }

    /// Idle connections kept, 4 unless set.
    pub fn max_idle(mut self, connections: usize) -> Self {
        self.max_idle = connections;
        self
    }

    pub fn user_agent(mut self, agent: &str) -> Self {
        self.user_agent = String::from(agent);
        self
    }

    /// HTTP/2 receive windows, 64 KB per stream and 128 KB per connection
    /// unless set: the most a response held unread takes, and how far a
    /// download runs ahead of its round trips. Clamped to 16 KB to 2 GB;
    /// ignored without `DEVKIT_NET_HTTP2`.
    pub fn http2_windows(self, stream: u32, connection: u32) -> Self {
        #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
        {
            let clamp = |n: u32| n.clamp(16 * 1024, 0x7FFF_FFFF);
            Self { windows: Windows { stream: clamp(stream), connection: clamp(connection) }, ..self }
        }
        #[cfg(not(CONFIG_DEVKIT_NET_HTTP2))]
        {
            let _ = (stream, connection);
            self
        }
    }

    #[cfg(test)]
    pub(crate) fn connector(mut self, connector: Connector) -> Self {
        self.connector = Some(connector);
        self
    }

    pub fn build(self) -> Client {
        Client {
            inner: Arc::new(Inner {
                pool: Arc::new(Pool::new(self.max_idle)),
                tls: Mutex::new(self.tls),
                connect_timeout: self.connect_timeout,
                idle_timeout: self.idle_timeout,
                timeout: self.timeout,
                redirects: self.redirects,
                user_agent: self.user_agent,
                #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
                windows: self.windows,
                #[cfg(test)]
                connector: self.connector,
            }),
        }
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        ClientBuilder::default().build()
    }

    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    pub fn request(&self, method: Method, url: &str) -> RequestBuilder<'_> {
        RequestBuilder {
            client: self,
            method,
            url: Url::parse(url),
            headers: Headers::new(),
            body: None,
            length: None,
            credentials: None,
            timeout: self.inner.timeout,
        }
    }

    pub fn get(&self, url: &str) -> RequestBuilder<'_> {
        self.request(Method::Get, url)
    }

    pub fn head(&self, url: &str) -> RequestBuilder<'_> {
        self.request(Method::Head, url)
    }

    pub fn post(&self, url: &str) -> RequestBuilder<'_> {
        self.request(Method::Post, url)
    }

    pub fn put(&self, url: &str) -> RequestBuilder<'_> {
        self.request(Method::Put, url)
    }

    pub fn patch(&self, url: &str) -> RequestBuilder<'_> {
        self.request(Method::Patch, url)
    }

    pub fn delete(&self, url: &str) -> RequestBuilder<'_> {
        self.request(Method::Delete, url)
    }

    fn tls_config(&self) -> Result<Arc<ClientConfig>> {
        let mut tls = self.inner.tls.lock();
        if let Some(config) = &*tls {
            return Ok(config.clone());
        }
        let store = TrustStore::default_bundle().map_err(Error::Tls)?;
        let mut config = ClientConfig::new(Verify::WebPki(store));
        if cfg!(CONFIG_DEVKIT_NET_HTTP2) {
            config.alpn.push(b"h2".to_vec());
        }
        config.alpn.push(b"http/1.1".to_vec());
        let config = Arc::new(config);
        *tls = Some(config.clone());
        Ok(config)
    }

    async fn open(&self, origin: &Origin, limits: &Limits) -> Result<Transport> {
        limits
            .run(self.inner.connect_timeout, async {
                let tcp = TcpStream::connect_async((origin.host.as_str(), origin.port)).await?;
                tcp.set_nodelay(true)?;
                if !origin.tls {
                    return Ok(Transport::Tcp(tcp));
                }
                let tls = TlsStream::start_async(tcp, Some(&origin.host), self.tls_config()?).await?;
                Ok(Transport::Tls(Box::new(tls)))
            })
            .await
    }

    /// A new connection; one whose ALPN chose h2 also goes into the pool, to
    /// be shared.
    async fn connect(&self, origin: &Origin, limits: &Limits) -> Result<Link> {
        #[cfg(test)]
        let io = match &self.inner.connector {
            Some(connector) => connector(&origin.host)?,
            None => self.open(origin, limits).await?,
        };
        #[cfg(not(test))]
        let io = self.open(origin, limits).await?;
        #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
        let io = match h2::split(io) {
            Ok(halves) => {
                let conn = H2::start(halves, origin.clone(), self.inner.windows, limits).await?;
                self.inner.pool.put_h2(conn.clone());
                return Ok(Link::H2(conn));
            }
            Err(io) => io,
        };
        Ok(Link::H1(Conn::new(io, origin.clone())))
    }

    /// An HTTP/2 connection to `origin` there already is.
    fn shared(&self, origin: &Origin) -> Option<Link> {
        #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
        return self.inner.pool.h2(origin).map(Link::H2);
        #[cfg(not(CONFIG_DEVKIT_NET_HTTP2))]
        {
            let _ = origin;
            None
        }
    }

    fn limits(&self, timeout: Option<Duration>) -> Limits {
        Limits { idle: self.inner.idle_timeout, deadline: timeout.and_then(|t| Instant::now().checked_add(t)) }
    }

    /// The header fields, in the order they go out.
    fn fields(&self, req: &Req<'_>, length: Length, expect: bool) -> Vec<(String, String)> {
        let mut fields = Vec::new();
        let mut field = |name: &str, value: &str| fields.push((String::from(name), String::from(value)));
        let user = &req.headers;
        if !user.contains("host") {
            field("Host", &req.url.origin.authority());
        }
        if !user.contains("user-agent") {
            field("User-Agent", &self.inner.user_agent);
        }
        if !user.contains("accept") {
            field("Accept", "*/*");
        }
        if req.decode {
            field("Accept-Encoding", "gzip, deflate");
        }
        if let (Some((name, password)), false) = (&req.credentials, user.contains("authorization")) {
            let value = match &req.digest {
                Some(digest) => digest.authorization(name, password, req.method.as_str(), &req.url.target),
                None => auth::basic(name, password),
            };
            field("Authorization", &value);
        }
        match length {
            Length::None => {}
            Length::Known(n) => field("Content-Length", &format!("{n}")),
            Length::Chunked => field("Transfer-Encoding", "chunked"),
        }
        if expect {
            field("Expect", "100-continue");
        }
        for (name, value) in user.iter() {
            if !["content-length", "transfer-encoding", "expect"].iter().any(|n| name.eq_ignore_ascii_case(n)) {
                field(name, value);
            }
        }
        fields
    }

    fn head_text(&self, req: &Req<'_>, length: Length, expect: bool) -> String {
        let mut h = format!("{} {} HTTP/1.1\r\n", req.method.as_str(), req.url.target);
        for (name, value) in self.fields(req, length, expect) {
            h.push_str(&name);
            h.push_str(": ");
            h.push_str(&value);
            h.push_str("\r\n");
        }
        h.push_str("\r\n");
        h
    }

    #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
    fn head_block(&self, req: &Req<'_>, length: Length, expect: bool) -> Vec<u8> {
        let fields = self.fields(req, length, expect);
        let host = fields.iter().find(|(n, _)| n.eq_ignore_ascii_case("host")).map(|(_, v)| v.as_str()).unwrap_or("");
        h2::request_block(req.method.as_str(), req.url.origin.tls, host, &req.url.target, &fields)
    }

    fn response(&self, sink: Sink, req: &Req<'_>, head: h1::Head, complete: bool, limits: Limits) -> Result<Response> {
        let carrier = match sink {
            Sink::H1(conn) => Carrier::H1(conn),
            #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
            Sink::H2(stream) => {
                let (h2, id) = stream.release();
                Carrier::H2(h2, id)
            }
        };
        Response::new(carrier, self.inner.pool.clone(), self.parts(req, head, complete), limits)
    }

    fn parts(&self, req: &Req<'_>, head: h1::Head, complete: bool) -> Parts {
        Parts { head, url: req.url.href(), head_request: req.method == Method::Head, complete, decode: req.decode }
    }

    /// Sends `req` with its whole body and reads the response head, on a
    /// kept connection when there is one. A kept connection the server had
    /// closed gets the request again on a new one.
    async fn exchange(&self, req: &Req<'_>, limits: &Limits) -> Result<Response> {
        let mut retried = false;
        loop {
            #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
            if let Some(h2) = self.inner.pool.h2(&req.url.origin) {
                match self.round_trip_h2(&h2, req, limits).await {
                    Err(Error::ConnectionAborted | Error::ConnectionReset | Error::BrokenPipe | Error::NotConnected)
                        if !retried =>
                    {
                        retried = true;
                        continue;
                    }
                    result => return result,
                }
            }
            let kept = self.inner.pool.take(&req.url.origin);
            let reused = kept.is_some();
            let mut conn = match kept {
                Some(conn) => conn,
                None => match self.connect(&req.url.origin, limits).await? {
                    Link::H1(conn) => conn,
                    #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
                    Link::H2(_) => continue,
                },
            };
            match self.round_trip(&mut conn, req, limits).await {
                Ok((head, complete)) => return self.response(Sink::H1(conn), req, head, complete, *limits),
                Err(Error::ConnectionAborted | Error::ConnectionReset | Error::BrokenPipe | Error::NotConnected)
                    if reused && !retried =>
                {
                    retried = true;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// The final head, and whether the body went out.
    async fn round_trip(&self, conn: &mut Conn, req: &Req<'_>, limits: &Limits) -> Result<(h1::Head, bool)> {
        let body = req.body.unwrap_or(&[]);
        let length = if req.body.is_some() || req.method.sends_body() { Length::Known(body.len() as u64) } else { Length::None };
        let expect = body.len() as u64 > EXPECT_BYTES;
        let head = self.head_text(req, length, expect);
        if !expect && body.len() <= COALESCE_BYTES {
            let mut first = Vec::new();
            first.try_reserve_exact(head.len() + body.len()).map_err(|_| Error::NoMemory)?;
            first.extend_from_slice(head.as_bytes());
            first.extend_from_slice(body);
            conn.write_all(&first, limits).await?;
        } else {
            conn.write_all(head.as_bytes(), limits).await?;
            if expect {
                if let Some(head) = wait_continue(limits, h1_continue(conn, limits)).await? {
                    return Ok((head, false));
                }
            }
            conn.write_all(body, limits).await?;
        }
        Ok((h1::final_head(conn, limits).await?, true))
    }

    #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
    async fn round_trip_h2(&self, h2: &Arc<H2>, req: &Req<'_>, limits: &Limits) -> Result<Response> {
        let body = req.body.unwrap_or(&[]);
        let length = if req.body.is_some() || req.method.sends_body() { Length::Known(body.len() as u64) } else { Length::None };
        let expect = body.len() as u64 > EXPECT_BYTES;
        let id = h2.open(&self.head_block(req, length, expect), body.is_empty(), limits).await?;
        let stream = h2::StreamRef::new(h2.clone(), id);
        if expect {
            if let Some(head) = wait_continue(limits, h2.continued(id, limits)).await? {
                return self.response(Sink::H2(stream), req, head, false, *limits);
            }
        }
        if !body.is_empty() {
            h2.send(id, body, true, limits).await?;
        }
        let head = h2.head(id, limits).await?;
        self.response(Sink::H2(stream), req, head, true, *limits)
    }

    async fn send(&self, mut req: Req<'_>, limits: Limits) -> Result<Response> {
        let mut redirects = 0;
        let mut asked = false;
        loop {
            let resp = self.exchange(&req, &limits).await?;
            if let Some(digest) = self.challenge(&req, &resp, &mut asked) {
                req.digest = Some(digest);
                resp.discard().await;
                continue;
            }
            match self.redirect(&req, &resp, &mut redirects)? {
                Some(next) => {
                    resp.discard().await;
                    req = next;
                    asked = false;
                }
                None => return Ok(resp),
            }
        }
    }

    /// A Digest challenge to answer once, for a request that has credentials.
    fn challenge(&self, req: &Req<'_>, resp: &Response, asked: &mut bool) -> Option<Digest> {
        if resp.status != 401 || *asked || req.credentials.is_none() {
            return None;
        }
        *asked = true;
        Digest::from_headers(resp.headers())
    }

    /// The request a redirect asks for: 303, and 301 or 302 after a POST,
    /// become a GET; credentials and the Authorization and Cookie fields stay
    /// only on the same origin.
    fn redirect<'a>(&self, req: &Req<'a>, resp: &Response, count: &mut u8) -> Result<Option<Req<'a>>> {
        if self.inner.redirects == 0 || !matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            return Ok(None);
        }
        let Some(location) = resp.headers().get("location") else { return Ok(None) };
        if *count >= self.inner.redirects {
            return Err(Error::TooManyRedirects);
        }
        *count += 1;
        let url = req.url.join(location)?;
        let to_get = (resp.status == 303 && req.method != Method::Head)
            || (matches!(resp.status, 301 | 302) && req.method == Method::Post);
        let mut next = Req {
            method: if to_get { Method::Get } else { req.method },
            body: if to_get { None } else { req.body },
            headers: req.headers.clone(),
            credentials: req.credentials.clone(),
            digest: req.digest.clone(),
            decode: req.decode,
            url,
        };
        if next.url.origin != req.url.origin {
            next.credentials = None;
            next.digest = None;
            next.headers.remove("authorization");
            next.headers.remove("cookie");
        }
        if let Some(user) = next.url.user.take() {
            next.credentials = Some((user, next.url.password.take().unwrap_or_default()));
        }
        Ok(Some(next))
    }
}

async fn h1_continue(conn: &mut Conn, limits: &Limits) -> Result<Option<h1::Head>> {
    loop {
        let head = h1::read_head(conn, limits).await?;
        if head.status == 100 {
            return Ok(None);
        }
        if !h1::is_interim(head.status) {
            return Ok(Some(head));
        }
    }
}

/// `Some` with a final response the server sent instead of 100 Continue;
/// `None` to send the body, after 100 Continue or a second of silence.
async fn wait_continue(limits: &Limits, wait: impl Future<Output = Result<Option<h1::Head>>>) -> Result<Option<h1::Head>> {
    match limits.run(Some(EXPECT_WAIT), wait).await {
        Err(Error::TimedOut) => {
            if limits.deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(Error::TimedOut);
            }
            Ok(None)
        }
        other => other,
    }
}

enum Link {
    H1(Conn),
    #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
    H2(Arc<H2>),
}

/// Where a request's body goes.
enum Sink {
    H1(Conn),
    #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
    H2(h2::StreamRef),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Length {
    None,
    Known(u64),
    Chunked,
}

struct Req<'a> {
    method: Method,
    url: Url,
    headers: Headers,
    body: Option<&'a [u8]>,
    credentials: Option<(String, String)>,
    digest: Option<Digest>,
    /// Accept-Encoding went out from this client, so the body is decoded.
    decode: bool,
}

/// A request being put together; nothing is sent until `send` or `start`.
pub struct RequestBuilder<'a> {
    client: &'a Client,
    method: Method,
    url: Result<Url>,
    headers: Headers,
    body: Option<&'a [u8]>,
    length: Option<u64>,
    credentials: Option<(String, String)>,
    timeout: Option<Duration>,
}

impl<'a> RequestBuilder<'a> {
    /// Host, User-Agent, Accept and Authorization replace the client's own;
    /// Content-Length, Transfer-Encoding and Expect are the client's to set.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.append(name, value);
        self
    }

    /// Sent with Basic up front, and with Digest when a 401 asks for it.
    /// Overrides a user and password in the URL.
    pub fn credentials(mut self, user: &str, password: &str) -> Self {
        self.credentials = Some((String::from(user), String::from(password)));
        self
    }

    /// The body for `send`, sent again when a redirect or a Digest challenge
    /// asks for the request once more.
    pub fn body(mut self, body: &'a [u8]) -> Self {
        self.body = Some(body);
        self
    }

    /// The length of the body `start` streams; without it, the body goes
    /// out chunked.
    pub fn content_length(mut self, length: u64) -> Self {
        self.length = Some(length);
        self
    }

    /// Overrides the client's `timeout` for this request.
    pub fn timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    fn into_req(self) -> Result<(&'a Client, Req<'a>, Limits, Option<u64>)> {
        let mut url = self.url?;
        let from_url = url.user.take().map(|user| (user, url.password.take().unwrap_or_default()));
        let decode = cfg!(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS) && !self.headers.contains("accept-encoding");
        let req = Req {
            method: self.method,
            url,
            headers: self.headers,
            body: self.body,
            credentials: self.credentials.or(from_url),
            digest: None,
            decode,
        };
        Ok((self.client, req, self.client.limits(self.timeout), self.length))
    }

    /// Sends the request with the `body` given, follows redirects and
    /// answers a Digest challenge, and returns the response once its head is in.
    pub fn send(self) -> Result<Response> {
        blocking(self.send_async())
    }

    pub fn send_timeout(self, timeout: Duration) -> Result<Response> {
        blocking_timeout(timeout, self.send_async())
    }

    pub async fn send_async(self) -> Result<Response> {
        let (client, req, limits, _) = self.into_req()?;
        client.send(req, limits).await
    }

    /// Opens a connection of its own and sends the head, for a body written
    /// through the `BodyWriter`. When the server answers before the body
    /// goes out, that response comes back instead, after the redirects and
    /// the Digest challenge that can still be followed without the body.
    pub fn start(self) -> Result<Started> {
        blocking(self.start_async())
    }

    pub fn start_timeout(self, timeout: Duration) -> Result<Started> {
        blocking_timeout(timeout, self.start_async())
    }

    pub async fn start_async(self) -> Result<Started> {
        let (client, mut req, limits, length) = self.into_req()?;
        req.body = None;
        let length = length.map_or(Length::Chunked, Length::Known);
        let expect = match length {
            Length::Known(n) => n > EXPECT_BYTES || req.credentials.is_some(),
            _ => true,
        };
        let mut redirects = 0;
        let mut asked = false;
        loop {
            let link = match client.shared(&req.url.origin) {
                Some(link) => link,
                None => client.connect(&req.url.origin, &limits).await?,
            };
            let (sink, head) = match link {
                Link::H1(mut conn) => {
                    conn.write_all(client.head_text(&req, length, expect).as_bytes(), &limits).await?;
                    let head = if expect { wait_continue(&limits, h1_continue(&mut conn, &limits)).await? } else { None };
                    (Sink::H1(conn), head)
                }
                #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
                Link::H2(h2) => {
                    let id = h2.open(&client.head_block(&req, length, expect), false, &limits).await?;
                    let stream = h2::StreamRef::new(h2.clone(), id);
                    let head = if expect { wait_continue(&limits, h2.continued(id, &limits)).await? } else { None };
                    (Sink::H2(stream), head)
                }
            };
            let Some(head) = head else {
                return Ok(Started::Body(BodyWriter {
                    client: client.clone(),
                    sink: Some(sink),
                    req: req.into_owned(),
                    length,
                    written: 0,
                    limits,
                    redirects,
                    scratch: Vec::new(),
                }));
            };
            let resp = client.response(sink, &req, head, false, limits)?;
            if let Some(digest) = client.challenge(&req, &resp, &mut asked) {
                req.digest = Some(digest);
                continue;
            }
            match client.redirect(&req, &resp, &mut redirects)? {
                Some(next) if next.method == req.method => req = next,
                Some(next) => return Ok(Started::Response(client.send(next, limits).await?)),
                None => return Ok(Started::Response(resp)),
            }
        }
    }
}

impl Req<'_> {
    fn into_owned(self) -> Req<'static> {
        Req {
            method: self.method,
            url: self.url,
            headers: self.headers,
            body: None,
            credentials: self.credentials,
            digest: self.digest,
            decode: self.decode,
        }
    }
}

pub enum Started {
    /// The server waits for the body.
    Body(BodyWriter),
    /// The server answered before the body went out.
    Response(Response),
}

/// The body of a request `start` began. Dropping it before `finish`
/// abandons the request and its connection.
pub struct BodyWriter {
    client: Client,
    sink: Option<Sink>,
    req: Req<'static>,
    length: Length,
    written: u64,
    limits: Limits,
    redirects: u8,
    scratch: Vec<u8>,
}

impl BodyWriter {
    /// More than the `content_length` given fails with `InvalidInput`.
    pub fn write_all(&mut self, data: &[u8]) -> Result<()> {
        blocking(self.write_all_async(data))
    }

    pub async fn write_all_async(&mut self, data: &[u8]) -> Result<()> {
        let sink = self.sink.as_mut().ok_or(Error::NotConnected)?;
        if data.is_empty() {
            return Ok(());
        }
        if let Length::Known(n) = self.length {
            if self.written + data.len() as u64 > n {
                return Err(Error::InvalidInput);
            }
        }
        match (sink, self.length) {
            #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
            (Sink::H2(stream), _) => stream.h2().send(stream.id, data, false, &self.limits).await?,
            (Sink::H1(conn), Length::Known(_)) => conn.write_all(data, &self.limits).await?,
            (Sink::H1(conn), _) => {
                let size = format!("{:x}\r\n", data.len());
                if data.len() <= COALESCE_BYTES {
                    self.scratch.clear();
                    self.scratch.try_reserve(size.len() + data.len() + 2).map_err(|_| Error::NoMemory)?;
                    self.scratch.extend_from_slice(size.as_bytes());
                    self.scratch.extend_from_slice(data);
                    self.scratch.extend_from_slice(b"\r\n");
                    conn.write_all(&self.scratch, &self.limits).await?;
                } else {
                    conn.write_all(size.as_bytes(), &self.limits).await?;
                    conn.write_all(data, &self.limits).await?;
                    conn.write_all(b"\r\n", &self.limits).await?;
                }
            }
        }
        self.written += data.len() as u64;
        Ok(())
    }

    /// Ends the body and reads the response. A redirect that turns the
    /// request into a GET is followed; one that needs the body again comes back.
    /// Fewer bytes than the `content_length` given fail with `InvalidInput`.
    pub fn finish(self) -> Result<Response> {
        blocking(self.finish_async())
    }

    pub fn finish_timeout(self, timeout: Duration) -> Result<Response> {
        blocking_timeout(timeout, self.finish_async())
    }

    pub async fn finish_async(mut self) -> Result<Response> {
        let sink = self.sink.take().ok_or(Error::NotConnected)?;
        if matches!(self.length, Length::Known(n) if self.written != n) {
            return Err(Error::InvalidInput);
        }
        let (sink, head) = match sink {
            Sink::H1(mut conn) => {
                if self.length == Length::Chunked {
                    conn.write_all(b"0\r\n\r\n", &self.limits).await?;
                }
                let head = h1::final_head(&mut conn, &self.limits).await?;
                (Sink::H1(conn), head)
            }
            #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
            Sink::H2(stream) => {
                stream.h2().send(stream.id, &[], true, &self.limits).await?;
                let head = stream.h2().head(stream.id, &self.limits).await?;
                (Sink::H2(stream), head)
            }
        };
        let client = &self.client;
        let resp = client.response(sink, &self.req, head, true, self.limits)?;
        match client.redirect(&self.req, &resp, &mut self.redirects)? {
            Some(next) if next.method != self.req.method => {
                resp.discard().await;
                client.send(next, self.limits).await
            }
            _ => Ok(resp),
        }
    }
}

/// GET with a client of its own, dropped with the response.
pub fn get(url: &str) -> Result<Response> {
    blocking(get_async(url))
}

pub fn get_timeout(url: &str, timeout: Duration) -> Result<Response> {
    blocking_timeout(timeout, get_async(url))
}

pub async fn get_async(url: &str) -> Result<Response> {
    Client::new().get(url).send_async().await
}
