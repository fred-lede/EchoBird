use std::net::SocketAddr;
use std::sync::atomic::Ordering;

use axum::serve::Listener;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(super) struct ServerControl {
    shutdown: CancellationToken,
    stopped: oneshot::Receiver<()>,
    #[cfg(windows)]
    addr: SocketAddr,
}

impl ServerControl {
    pub(super) fn start(listener: TcpListener, app: axum::Router) -> Self {
        #[cfg(windows)]
        let addr = listener.local_addr().expect("bound listener");
        let shutdown = CancellationToken::new();
        let signal = shutdown.clone();
        let (released, stopped) = oneshot::channel();
        let listener = TrackedListener {
            listener: Some(listener),
            released: Some(released),
        };
        super::RUNNING.store(true, Ordering::Relaxed);
        tauri::async_runtime::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(signal.clone().cancelled_owned())
                .await;
            // A previous server may finish draining after a new one has started.
            if !signal.is_cancelled() {
                super::RUNNING.store(false, Ordering::Relaxed);
            }
            if let Err(error) = result {
                log::error!("[SmartRouter] serve failed: {error}");
            }
        });
        Self {
            shutdown,
            stopped,
            #[cfg(windows)]
            addr,
        }
    }

    pub(super) async fn stop(self) {
        self.shutdown.cancel();
        // Wait for the listening socket; in-flight responses drain independently.
        let _ = self.stopped.await;
        super::RUNNING.store(false, Ordering::Relaxed);
        // Windows may still be processing canceled socket I/O after Drop.
        #[cfg(windows)]
        if let Err(error) = wait_for_port_release(self.addr).await {
            // Closed connections or another listener may still reserve the port.
            // A subsequent start retains its existing bind fallback.
            log::debug!(
                "[SmartRouter] listener closed; port {} not yet reusable: {error}",
                self.addr
            );
        }
    }
}

#[cfg(windows)]
async fn wait_for_port_release(addr: SocketAddr) -> std::io::Result<()> {
    use std::io::ErrorKind;
    use std::time::Duration;
    use tokio::net::TcpSocket;
    use tokio::time::{sleep, Instant};

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        // Bind only: no listener, accepted connections, or reactor registration.
        let socket = if addr.is_ipv4() {
            TcpSocket::new_v4()?
        } else {
            TcpSocket::new_v6()?
        };
        socket.set_reuseaddr(false)?;
        match socket.bind(addr) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == ErrorKind::AddrInUse => {
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        ErrorKind::TimedOut,
                        "port remains reserved or occupied",
                    ));
                }
            }
            Err(error) => return Err(error),
        }
        drop(socket);
        sleep(Duration::from_millis(1)).await;
    }
}

struct TrackedListener {
    listener: Option<TcpListener>,
    released: Option<oneshot::Sender<()>>,
}

impl Listener for TrackedListener {
    type Io = TcpStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        Listener::accept(self.listener.as_mut().expect("live listener")).await
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.as_ref().expect("live listener").local_addr()
    }
}

impl Drop for TrackedListener {
    fn drop(&mut self) {
        drop(self.listener.take());
        if let Some(released) = self.released.take() {
            let _ = released.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Notify;

    #[test]
    fn stopping_closes_listener_and_restart_preserves_existing_request() {
        tauri::async_runtime::block_on(async {
            let entered = Arc::new(Notify::new());
            let finish = Arc::new(Notify::new());
            let app = axum::Router::new().route(
                "/",
                axum::routing::get({
                    let entered = entered.clone();
                    let finish = finish.clone();
                    move || async move {
                        entered.notify_one();
                        finish.notified().await;
                        "completed"
                    }
                }),
            );
            // Keep listener registration and serving on the production runtime.
            let listener = crate::services::local_proxy::bind_with_fallback(0).unwrap();
            listener.set_nonblocking(true).unwrap();
            let listener = TcpListener::from_std(listener).unwrap();
            let addr = listener.local_addr().unwrap();
            let server = ServerControl::start(listener, app);
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let request_client = client.clone();
            let request = tokio::spawn(async move {
                request_client
                    .get(format!("http://{addr}/"))
                    .send()
                    .await
                    .unwrap()
            });
            tokio::time::timeout(Duration::from_secs(5), entered.notified())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), server.stop())
                .await
                .unwrap();
            assert!(!request.is_finished());
            // A draining connection can still reserve the old Windows port.
            // Restart with the same binding policy as production.
            let listener = crate::services::local_proxy::bind_with_fallback(addr.port()).unwrap();
            let restarted_addr = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let listener = TcpListener::from_std(listener).unwrap();
            let restarted = ServerControl::start(
                listener,
                axum::Router::new().route("/", axum::routing::get(|| async { "restarted" })),
            );
            assert_eq!(
                client
                    .get(format!("http://{restarted_addr}/"))
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap(),
                "restarted"
            );
            assert!(!request.is_finished());
            finish.notify_one();
            let response = tokio::time::timeout(Duration::from_secs(5), request)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(response.text().await.unwrap(), "completed");
            restarted.stop().await;
        });
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn port_release_probe_does_not_take_an_active_listener() {
        let listener = TcpListener::bind("127.0.0.2:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let error = tokio::time::timeout(Duration::from_secs(2), wait_for_port_release(addr))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        let unused_listener = TcpListener::bind("127.0.0.2:0").await.unwrap();
        let released_addr = unused_listener.local_addr().unwrap();
        let connected = TcpStream::connect(addr).await.unwrap();
        let (accepted, _) = listener.accept().await.unwrap();
        drop((connected, accepted, listener));
        // Closed connections may retain their own endpoint after listener Drop.
        drop(unused_listener);
        wait_for_port_release(released_addr).await.unwrap();
        drop(TcpListener::bind(released_addr).await.unwrap());
    }
}
