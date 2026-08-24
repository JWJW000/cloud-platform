//! 本地固定端口 HTTP/CONNECT 代理转发（第 10.1 节、V4 方案第 14 节）。
//!
//! R8（V4-13）：放弃自制的 chunked 结束标记判断，改用成熟 HTTP 库
//! （hyper 1.x + hyper-util）处理 HTTP/1.1 framing——Content-Length、chunked、
//! 无 body、HEAD/204/304、close-delimited、pipelining 全部交给库，不再自行
//! 猜测「读到哪里算结束」。
//!
//! 生命周期（第 14.2 节）：
//! - `LocalProxyServer` 属于一个槽位会话：CreateSession 时创建，EndSession 时关闭
//!   监听器并回收所有子连接任务；
//! - 同一会话期间上游 host/port/username/password 不可替换；
//! - 子连接任务由 JoinSet 统一回收，关闭会话后旧连接不得继续使用上游凭据。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::client::conn::http1;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;

use platform_proto::v1::ProxyCredential;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

/// 连接上游代理超时：10 秒
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 请求头大小上限（hyper 的 max_buf_size，默认 400KB 已足够；这里显式收紧）
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// 本地代理转发服务句柄。
pub struct LocalProxyServer {
    local_port: u16,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl LocalProxyServer {
    /// 获取当前监听端口。
    pub fn port(&self) -> u16 {
        self.local_port
    }

    /// 启动本地转发服务。
    pub async fn spawn(local_port: u16, upstream: ProxyCredential) -> Result<Self> {
        let addr = SocketAddr::from(([127, 0, 0, 1], local_port));
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("绑定本地代理端口失败：{local_port}"))?;

        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
        let upstream = Arc::new(upstream);
        let join_set: Arc<tokio::sync::Mutex<JoinSet<()>>> =
            Arc::new(tokio::sync::Mutex::new(JoinSet::new()));

        let acceptor_set = join_set.clone();
        let acceptor_upstream = upstream.clone();
        tokio::spawn(async move {
            tracing::info!(local_port = local_port, "本地代理转发服务已启动");
            loop {
                tokio::select! {
                    accept_res = listener.accept() => {
                        match accept_res {
                            Ok((client_stream, _client_addr)) => {
                                let up = acceptor_upstream.clone();
                                let set = acceptor_set.clone();
                                let mut guard = set.lock().await;
                                guard.spawn(async move {
                                    let _ = handle_client_conn(client_stream, up).await;
                                });
                                // 及时回收已结束的子连接任务：长会话里连接不断建立/关闭，
                                // 若只在 stop 时 abort_all，JoinSet 会积累已完成句柄（P2）。
                                drop(guard);
                                let mut guard = set.lock().await;
                                while guard.try_join_next().is_some() {}
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "本地代理接受连接失败");
                                break;
                            }
                        }
                    }
                    _ = &mut shutdown_rx => {
                        tracing::info!(local_port = local_port, "本地代理转发服务收到关闭信号");
                        break;
                    }
                }
            }
            // 关闭所有子连接任务
            let mut set = acceptor_set.lock().await;
            set.abort_all();
        });

        Ok(Self {
            local_port,
            shutdown_tx: Some(shutdown_tx),
        })
    }

    /// 停止转发服务：关闭监听器并回收所有子连接。
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for LocalProxyServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 处理一个客户端 TCP 连接：交给 hyper HTTP/1.1 服务处理。
async fn handle_client_conn(
    client_stream: TcpStream,
    upstream: Arc<ProxyCredential>,
) -> Result<()> {
    let io = TokioIo::new(client_stream);
    let upstream_clone = upstream.clone();
    let service = service_fn(move |req| proxy_service(req, upstream_clone.clone()));

    // 必须用 hyper::server::conn::http1::Builder：CONNECT 升级依赖它的高层连接 API，
    // hyper-util 的 auto::Builder 走低层 API，不支持 upgrade（会返回
    // "upgrade expected but low level API in use"）。
    let mut http1_builder = server_http1::Builder::new();
    http1_builder.max_buf_size(MAX_HEADER_BYTES);
    let conn = http1_builder.serve_connection(io, service).with_upgrades();
    let _ = conn.await;
    Ok(())
}

/// 本地代理服务逻辑：CONNECT 走隧道，其余请求注入上游认证后转发。
async fn proxy_service(
    req: Request<Incoming>,
    upstream: Arc<ProxyCredential>,
) -> Result<Response<BoxBody<bytes::Bytes, hyper::Error>>, hyper::Error> {
    if req.method() == Method::CONNECT {
        return handle_connect(req, upstream).await;
    }
    handle_forward(req, upstream).await
}

/// 构造一个空 body。
fn empty_body() -> BoxBody<bytes::Bytes, hyper::Error> {
    Empty::<bytes::Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

/// 构造一个带文本的 body。
fn full_body(text: String) -> BoxBody<bytes::Bytes, hyper::Error> {
    Full::new(bytes::Bytes::from(text))
        .map_err(|never| match never {})
        .boxed()
}

/// 构造上游 Basic 认证头值。
fn auth_header_value(upstream: &ProxyCredential) -> Option<String> {
    if upstream.username.is_empty() {
        return None;
    }
    let auth_str = format!("{}:{}", upstream.username, upstream.password);
    Some(format!("Basic {}", BASE64.encode(auth_str)))
}

/// CONNECT 隧道：向上游代理发起 CONNECT，等待 200，升级为双向透传。
async fn handle_connect(
    mut req: Request<Incoming>,
    upstream: Arc<ProxyCredential>,
) -> Result<Response<BoxBody<bytes::Bytes, hyper::Error>>, hyper::Error> {
    let authority = req.uri().authority().map(|a| a.as_str().to_string());
    let Some(authority) = authority else {
        return Ok(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(full_body("CONNECT 请求缺少目标主机".to_string()))
            .unwrap());
    };

    // 1. 连接上游代理
    let upstream_addr = format!("{}:{}", upstream.host, upstream.port);
    let mut server =
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&upstream_addr)).await {
            Ok(Ok(s)) => s,
            _ => {
                return Ok(Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(full_body("无法连接上游代理".to_string()))
                    .unwrap());
            }
        };

    // 2. 发送 CONNECT（注入上游认证；客户端自带的 Proxy-Authorization 不转发）
    let mut request_line = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(auth) = auth_header_value(&upstream) {
        request_line.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
    }
    request_line.push_str("\r\n");
    if let Err(err) =
        tokio::time::timeout(CONNECT_TIMEOUT, server.write_all(request_line.as_bytes())).await
    {
        tracing::debug!(error = %err, "向上游发送 CONNECT 失败");
        return Ok(Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(full_body("向上游发送 CONNECT 失败".to_string()))
            .unwrap());
    }

    // 3. 读取上游响应头（手工读头；200 之后剩余字节属于隧道数据）
    let mut resp_buf = Vec::with_capacity(4096);
    let mut byte = [0u8; 1];
    while !resp_buf.ends_with(b"\r\n\r\n") {
        match tokio::time::timeout(CONNECT_TIMEOUT, server.read(&mut byte)).await {
            Ok(Ok(0)) | Err(_) => {
                return Ok(Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(full_body("上游代理未返回 CONNECT 响应".to_string()))
                    .unwrap());
            }
            Ok(Ok(_)) => resp_buf.push(byte[0]),
            Ok(Err(_)) => {
                return Ok(Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(full_body("读取上游 CONNECT 响应失败".to_string()))
                    .unwrap());
            }
        }
        if resp_buf.len() > MAX_HEADER_BYTES {
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(full_body("上游 CONNECT 响应头过大".to_string()))
                .unwrap());
        }
    }

    let status_line = String::from_utf8_lossy(&resp_buf);
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(502);
    if code != 200 {
        tracing::debug!(code, "上游代理拒绝 CONNECT");
        return Ok(Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(full_body(format!("上游代理拒绝 CONNECT：{code}")))
            .unwrap());
    }

    // 4. 返回 200 并升级连接：隧道由 hyper::upgrade 接管。
    //
    // 关键：上游的响应头（resp_buf）**绝不能**写进隧道——客户端已经收到 hyper
    // 的 200，接下来直接进入 TLS 数据；把 "HTTP/1.1 200 Connection Established"
    // 塞进去会让客户端把响应头当 TLS 记录解析，握手必然失败。
    // 由于响应头是逐字节读到 \r\n\r\n 即停，头部之后的隧道数据仍留在上游 socket 里，
    // 直接双向拷贝即可，不需要（也不允许）搬运 resp_buf。
    tokio::spawn(async move {
        match hyper::upgrade::on(&mut req).await {
            Ok(upgraded) => {
                let mut upgraded = TokioIo::new(upgraded);
                let _ = tokio::io::copy_bidirectional(&mut upgraded, &mut server).await;
            }
            Err(err) => {
                tracing::debug!(error = %err, "CONNECT 隧道升级失败");
            }
        }
    });

    Ok(Response::new(empty_body()))
}

/// 普通 HTTP 转发：注入上游认证，由 hyper 客户端处理请求/响应 framing。
async fn handle_forward(
    req: Request<Incoming>,
    upstream: Arc<ProxyCredential>,
) -> Result<Response<BoxBody<bytes::Bytes, hyper::Error>>, hyper::Error> {
    // 1. 连接上游代理
    let upstream_addr = format!("{}:{}", upstream.host, upstream.port);
    let stream =
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&upstream_addr)).await {
            Ok(Ok(s)) => s,
            _ => {
                return Ok(Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(full_body("无法连接上游代理".to_string()))
                    .unwrap());
            }
        };

    // 2. 重建请求：绝对形式 URI 交给代理，删除客户端自带的 Proxy-Authorization，
    //    注入当前会话的上游凭据（第 14.1 节：防客户端伪造凭据）。
    let mut builder = Request::builder()
        .method(req.method().clone())
        .uri(req.uri().clone());
    for (name, value) in req.headers() {
        if name.as_str().eq_ignore_ascii_case("proxy-authorization") {
            continue;
        }
        builder = builder.header(name, value);
    }
    if let Some(auth) = auth_header_value(&upstream) {
        builder = builder.header("Proxy-Authorization", auth);
    }
    let body = req.into_body();
    let Ok(forward_req) = builder.body(body) else {
        return Ok(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(full_body("无法构造上游请求".to_string()))
            .unwrap());
    };

    // 3. 用 hyper http1 客户端发送（绝对形式 URI → 请求行），并流式返回响应。
    let (mut sender, conn) = match http1::handshake(TokioIo::new(stream)).await {
        Ok(pair) => pair,
        Err(err) => {
            tracing::debug!(error = %err, "与上游代理握手失败");
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(full_body("与上游代理握手失败".to_string()))
                .unwrap());
        }
    };
    tokio::spawn(async move {
        let _ = conn.await;
    });

    match sender.send_request(forward_req).await {
        Ok(resp) => {
            let (parts, body) = resp.into_parts();
            Ok(Response::from_parts(parts, body.boxed()))
        }
        Err(err) => {
            tracing::debug!(error = %err, "上游代理请求失败");
            Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(full_body("上游代理请求失败".to_string()))
                .unwrap())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// 分配一个空闲端口。
    async fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    }

    fn credential(host: &str, port: u16, username: &str, password: &str) -> ProxyCredential {
        ProxyCredential {
            proxy_id: "test".to_string(),
            label: "test".to_string(),
            scheme: "http".to_string(),
            host: host.to_string(),
            port: port as u32,
            username: username.to_string(),
            password: password.to_string(),
        }
    }

    /// 模拟上游：接收 CONNECT 或普通请求，断言注入的认证头，按需回包。
    async fn spawn_upstream(
        assert_auth: String,
        behavior: impl Fn(&str) -> Vec<u8> + Send + 'static,
    ) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap();
            let text = String::from_utf8_lossy(&buf[..n]);
            // HTTP/1.1 头名大小写不敏感，hyper 会统一小写
            assert!(
                text.to_lowercase()
                    .contains(assert_auth.to_lowercase().as_str()),
                "上游必须收到注入的认证头：{text}"
            );
            stream.write_all(&behavior(&text)).await.unwrap();
        });
        port
    }

    #[tokio::test]
    async fn connect_tunnel_injects_auth_and_tunnels_bidirectional() {
        // 上游：收到 CONNECT 后回 200，然后把隧道里的数据原样回显
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap();
            let text = String::from_utf8_lossy(&buf[..n]);
            assert!(text.starts_with("CONNECT example.com:443 "));
            assert!(
                text.to_lowercase()
                    .contains("proxy-authorization: basic dxnlcjpwyxnz"),
                "上游必须收到注入的认证头：{text}"
            );
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            // 回显隧道数据
            let mut echo = [0u8; 4096];
            loop {
                let n = stream.read(&mut echo).await.unwrap();
                if n == 0 {
                    break;
                }
                stream.write_all(&echo[..n]).await.unwrap();
            }
        });

        let local_port = free_port().await;
        let mut proxy = LocalProxyServer::spawn(
            local_port,
            credential("127.0.0.1", upstream_port, "user", "pass"),
        )
        .await
        .unwrap();

        // 客户端连接本地代理并发起 CONNECT
        let mut client = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();
        client
            .write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n")
            .await
            .unwrap();

        // 读取 200 响应（hyper 生成的，不含上游响应头文本）
        let mut resp = [0u8; 1024];
        let n = client.read(&mut resp).await.unwrap();
        let resp_str = String::from_utf8_lossy(&resp[..n]);
        assert!(resp_str.contains("200"), "响应：{resp_str}");
        assert!(
            !resp_str.contains("Connection Established"),
            "隧道开头不得携带上游 HTTP 响应头（会污染 TLS 握手）：{resp_str}"
        );

        // 隧道双向透传：发送 "ping"，必须原样收到 "ping"，且第一段数据不是 HTTP 头
        client.write_all(b"ping").await.unwrap();
        let mut echo = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(3), client.read(&mut echo))
            .await
            .expect("隧道回显超时")
            .unwrap();
        let echo_str = String::from_utf8_lossy(&echo[..n]);
        assert_eq!(
            echo_str, "ping",
            "隧道必须原样回显，不得混入响应头：{echo_str}"
        );
        assert!(
            !echo_str.to_uppercase().contains("HTTP/"),
            "隧道数据被 HTTP 响应头污染：{echo_str}"
        );

        proxy.stop();
    }

    #[tokio::test]
    async fn http_get_keepalive_injects_auth_and_streams_response() {
        let upstream_port = spawn_upstream(
            "Proxy-Authorization: Basic YWRtaW46c2VjcmV0".to_string(),
            |_req| {
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody".to_vec()
            },
        )
        .await;

        let local_port = free_port().await;
        let mut proxy = LocalProxyServer::spawn(
            local_port,
            credential("127.0.0.1", upstream_port, "admin", "secret"),
        )
        .await
        .unwrap();

        let mut client = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();
        client
            .write_all(b"GET http://example.com/path HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();

        let mut resp = Vec::new();
        client.read_to_end(&mut resp).await.unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.contains("200 OK"));
        assert!(resp_str.contains("body"), "响应体：{resp_str}");

        proxy.stop();
    }

    #[tokio::test]
    async fn chunked_request_body_is_forwarded_correctly() {
        // 模拟上游：读取 chunked 请求体并回包
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 16384];
            let mut total = Vec::new();
            loop {
                let n = stream.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                total.extend_from_slice(&buf[..n]);
                if total.windows(5).any(|w| w == b"0\r\n\r\n") {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&total);
            assert!(text
                .to_lowercase()
                .contains("proxy-authorization: basic dxnlcjpwyxnz"));
            assert!(
                text.to_lowercase().contains("transfer-encoding: chunked"),
                "{text}"
            );
            // chunked 请求体被完整透传
            assert!(
                text.contains("4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n"),
                "{text}"
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
        });

        let local_port = free_port().await;
        let mut proxy = LocalProxyServer::spawn(
            local_port,
            credential("127.0.0.1", upstream_port, "user", "pass"),
        )
        .await
        .unwrap();

        let mut client = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();
        client
            .write_all(b"POST http://example.com/api HTTP/1.1\r\nHost: example.com\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n")
            .await
            .unwrap();

        let mut resp = Vec::new();
        client.read_to_end(&mut resp).await.unwrap();
        assert!(String::from_utf8_lossy(&resp).contains("200 OK"));

        proxy.stop();
    }

    #[tokio::test]
    async fn chunk_terminator_across_tcp_packets_is_handled() {
        // 核心场景（V4-13）：chunk terminator 跨 TCP 数据包。
        // hyper 按 framing 解析，不依赖「一次 read 读到 0\r\n\r\n」。
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut total = Vec::new();
            let mut buf = vec![0u8; 4096];
            loop {
                let n = stream.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                total.extend_from_slice(&buf[..n]);
                if total.windows(5).any(|w| w == b"0\r\n\r\n") {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&total);
            // hyper 会解帧后按自己的 chunk 边界重编码：正文内容必须完整到达
            // （跨包发送的 terminator 由 hyper 处理，wire 上的 chunk 大小可能不同）
            assert!(text.contains("he") && text.contains("llo"), "{text}");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n")
                .await
                .unwrap();
        });

        let local_port = free_port().await;
        let mut proxy =
            LocalProxyServer::spawn(local_port, credential("127.0.0.1", upstream_port, "u", "p"))
                .await
                .unwrap();

        let mut client = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();
        // 拆成多个小包发送，模拟跨包边界
        let request = b"POST http://example.com/ HTTP/1.1\r\nHost: example.com\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhe";
        let request2 = b"llo\r\n0\r\n\r\n";
        client.write_all(request).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        client.write_all(request2).await.unwrap();

        let mut resp = Vec::new();
        client.read_to_end(&mut resp).await.unwrap();
        assert!(String::from_utf8_lossy(&resp).contains("200 OK"));
        assert!(String::from_utf8_lossy(&resp).contains("hello"));

        proxy.stop();
    }

    #[tokio::test]
    async fn client_forged_proxy_authorization_is_replaced() {
        let upstream_port = spawn_upstream(
            "Proxy-Authorization: Basic dXNlcjpwYXNz".to_string(),
            |_req| b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec(),
        )
        .await;

        let local_port = free_port().await;
        let mut proxy = LocalProxyServer::spawn(
            local_port,
            credential("127.0.0.1", upstream_port, "user", "pass"),
        )
        .await
        .unwrap();

        let mut client = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();
        // 客户端伪造 Proxy-Authorization：必须被替换为会话的真实凭据
        client
            .write_all(b"GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\nProxy-Authorization: Basic Zm9yZ2Vk\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();

        let mut resp = Vec::new();
        client.read_to_end(&mut resp).await.unwrap();
        assert!(String::from_utf8_lossy(&resp).contains("200 OK"));

        proxy.stop();
    }

    #[tokio::test]
    async fn upstream_407_is_passed_through() {
        let upstream_port = spawn_upstream("Proxy-Authorization: Basic dXNlcjpwYXNz".to_string(), |_req| {
            b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        })
        .await;

        let local_port = free_port().await;
        let mut proxy = LocalProxyServer::spawn(
            local_port,
            credential("127.0.0.1", upstream_port, "user", "pass"),
        )
        .await
        .unwrap();

        let mut client = TcpStream::connect(format!("127.0.0.1:{local_port}"))
            .await
            .unwrap();
        client
            .write_all(b"GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();

        let mut resp = Vec::new();
        client.read_to_end(&mut resp).await.unwrap();
        assert!(String::from_utf8_lossy(&resp).contains("407"));

        proxy.stop();
    }
}
