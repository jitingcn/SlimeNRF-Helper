//! Application-owned async I/O driver. Device threads and GPUI keep their own executors.
use std::{
    sync::Arc,
    thread::{self, JoinHandle},
};

use reqwest::{Certificate, Client, redirect::Policy};
use tokio::{runtime::Handle, sync::oneshot};

use crate::features::firmware::ble_ota::Backend;

/// A concrete submission handle, explicitly injected at the application boundary.
#[derive(Clone)]
pub(crate) struct Executor {
    handle: Handle,
    client: Client,
    ble: Arc<Backend>,
}

impl Executor {
    pub(crate) fn http_client(&self) -> Client {
        self.client.clone()
    }

    pub(crate) fn ble_backend(&self) -> Arc<Backend> {
        Arc::clone(&self.ble)
    }

    pub(crate) fn spawn<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> tokio::task::JoinHandle<T> {
        self.handle.spawn(future)
    }
}

/// Lives outside the native event loop and stops only after retained jobs have joined.
pub(crate) struct AppRuntime {
    executor: Executor,
    stop: Option<oneshot::Sender<()>>,
    driver: Option<JoinHandle<()>>,
}

impl AppRuntime {
    pub(crate) fn new() -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|error| format!("Cannot start application I/O runtime: {error}"))?;
        // Keep Mozilla roots bundled; native CA availability must not gate offline startup.
        let client = webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .try_fold(Client::builder(), |builder, certificate| {
                Certificate::from_der(certificate.as_ref())
                    .map(|certificate| builder.tls_certs_only([certificate]))
            })
            .map_err(|error| format!("Cannot load bundled HTTPS roots: {}", error.without_url()))?;
        let client = client
            .user_agent("SmolHelper-firmware/1")
            // HTTP acquisition validates every redirect before constructing a new request.
            .redirect(Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|error| format!("Cannot initialize HTTPS: {}", error.without_url()))?;
        let executor = Executor {
            handle: runtime.handle().clone(),
            client,
            ble: Arc::new(Backend::default()),
        };
        let (stop, stopped) = oneshot::channel();
        let driver = thread::Builder::new()
            .name("application-async-io".into())
            .spawn(move || {
                runtime.block_on(async {
                    let _ = stopped.await;
                });
                // Jobs are joined before the stop signal. Dropping on the driver
                // thread also joins blocking work rather than silently detaching it.
                drop(runtime);
            })
            .map_err(|error| format!("Cannot start application I/O driver: {error}"))?;
        Ok(Self {
            executor,
            stop: Some(stop),
            driver: Some(driver),
        })
    }

    pub(crate) fn executor(&self) -> Executor {
        self.executor.clone()
    }

    pub(crate) fn shutdown(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(driver) = self.driver.take() {
            let _ = driver.join();
        }
    }
}

impl Drop for AppRuntime {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}
