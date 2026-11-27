//! Native HID inventory and opens; readers own their handles independently.

use std::borrow::Cow;
use std::ffi::CStr;
#[cfg(target_os = "macos")]
use std::sync::Arc;

use hidapi::{DeviceInfo, HidApi, HidDevice, HidResult};

#[cfg(target_os = "macos")]
type Snapshot = Arc<HidApi>;
#[cfg(not(target_os = "macos"))]
type Snapshot = HidApi;

/// One fresh native inventory, reusable for path opens without another scan.
pub(crate) struct Devices(Snapshot);

impl Devices {
    pub(crate) fn device_list(&self) -> impl Iterator<Item = &DeviceInfo> {
        self.0.device_list()
    }

    pub(crate) fn open_path<'path>(
        &self,
        path: impl Into<Cow<'path, CStr>>,
    ) -> HidResult<HidDevice> {
        let path = path.into();
        #[cfg(target_os = "macos")]
        {
            darwin::open_path(Arc::clone(&self.0), path.into_owned())
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.0.open_path(path.as_ref())
        }
    }
}

pub(crate) fn enumerate() -> HidResult<Devices> {
    #[cfg(target_os = "macos")]
    let snapshot = darwin::enumerate()?;
    #[cfg(not(target_os = "macos"))]
    let snapshot = HidApi::new()?;
    Ok(Devices(snapshot))
}

/// Stop accepting native HID work after all handle owners have joined.
#[cfg(target_os = "macos")]
pub(crate) fn shutdown() {
    darwin::shutdown();
}

#[cfg(target_os = "macos")]
mod darwin {
    use super::owner::Owner;
    use hidapi::{HidApi, HidDevice, HidResult};
    use std::ffi::{CString, c_int};
    use std::sync::{
        Arc,
        mpsc::{Receiver, SyncSender},
    };

    // hid_init schedules the process-global IOHIDManager on the first caller's
    // run loop. hid_enumerate pumps that loop and rematches the manager; a mutex
    // alone does not keep the original thread (and its run loop) alive.
    static OWNER: Owner<Request> = Owner::new(serve);

    enum Request {
        Enumerate(SyncSender<HidResult<Arc<HidApi>>>),
        Open {
            snapshot: Arc<HidApi>,
            path: CString,
            reply: SyncSender<HidResult<HidDevice>>,
        },
    }

    unsafe extern "C" {
        // Exported by the bundled HIDAPI C library; hidapi-rs has no exit API.
        fn hid_exit() -> c_int;
    }

    fn serve(requests: Receiver<Request>) {
        for request in requests {
            match request {
                Request::Enumerate(reply) => {
                    let _ = reply.send(HidApi::new().map(Arc::new));
                }
                Request::Open {
                    snapshot,
                    path,
                    reply,
                } => {
                    // The shared-device feature sets the nonexclusive default
                    // on init. Each Send handle has its own HIDAPI read thread.
                    let _ = reply.send(snapshot.open_path(&path));
                }
            }
        }
        // SAFETY: The app has joined its handle owners and closed the request
        // queue; all accepted requests have finished. Release the manager while
        // its run loop is still alive. This owner is never started again.
        unsafe { hid_exit() };
    }

    pub(super) fn enumerate() -> HidResult<Arc<HidApi>> {
        OWNER.request(Request::Enumerate)?
    }

    pub(super) fn open_path(snapshot: Arc<HidApi>, path: CString) -> HidResult<HidDevice> {
        OWNER.request(|reply| Request::Open {
            snapshot,
            path,
            reply,
        })?
    }

    pub(super) fn shutdown() {
        OWNER.shutdown();
    }
}

#[cfg(any(target_os = "macos", test))]
mod owner {
    use hidapi::{HidError, HidResult};
    use parking_lot::Mutex;
    use std::io;
    use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
    use std::thread::{self, JoinHandle};

    pub(super) struct Owner<Request> {
        state: Mutex<State<Request>>,
        serve: fn(Receiver<Request>),
    }

    enum State<Request> {
        Idle,
        Running {
            requests: Sender<Request>,
            thread: JoinHandle<()>,
        },
        Stopped,
    }

    impl<Request: Send + 'static> Owner<Request> {
        pub(super) const fn new(serve: fn(Receiver<Request>)) -> Self {
            Self {
                state: Mutex::new(State::Idle),
                serve,
            }
        }

        pub(super) fn request<T>(
            &self,
            make: impl FnOnce(SyncSender<T>) -> Request,
        ) -> HidResult<T> {
            let mut state = self.state.lock();
            if matches!(*state, State::Idle) {
                let (requests, incoming) = mpsc::channel();
                let serve = self.serve;
                let thread = thread::Builder::new()
                    .name("macos-hid".into())
                    .spawn(move || serve(incoming))?;
                *state = State::Running { requests, thread };
            }
            let State::Running { requests, .. } = &*state else {
                return Err(stopped());
            };
            let (reply, response) = mpsc::sync_channel(1);
            requests.send(make(reply)).map_err(|_| stopped())?;
            drop(state);
            response.recv().map_err(|_| stopped())
        }

        pub(super) fn shutdown(&self) {
            let state = std::mem::replace(&mut *self.state.lock(), State::Stopped);
            if let State::Running { requests, thread } = state {
                drop(requests);
                let _ = thread.join();
            }
        }
    }

    fn stopped() -> HidError {
        io::Error::new(io::ErrorKind::BrokenPipe, "macOS HID worker stopped").into()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::thread::ThreadId;

        #[test]
        fn native_owner_outlives_the_first_requesting_thread() {
            let owner = Owner::new(|requests: Receiver<SyncSender<ThreadId>>| {
                for reply in requests {
                    reply.send(thread::current().id()).unwrap();
                }
            });
            let request_from_transient_thread = || {
                thread::scope(|scope| {
                    scope
                        .spawn(|| {
                            let owner_id = owner.request(|reply| reply).unwrap();
                            (thread::current().id(), owner_id)
                        })
                        .join()
                        .unwrap()
                })
            };
            let (first_caller, first_owner) = request_from_transient_thread();
            let (next_caller, next_owner) = request_from_transient_thread();
            owner.shutdown();
            assert_ne!(first_caller, first_owner);
            assert_ne!(next_caller, next_owner);
            assert_eq!(first_owner, next_owner);
        }

        #[test]
        fn native_owner_joins_before_shutdown_returns_and_cannot_restart() {
            let owner = Owner::new(|requests: Receiver<(SyncSender<()>, Sender<()>)>| {
                let (reply, exited) = requests.recv().unwrap();
                reply.send(()).unwrap();
                while let Ok((reply, _)) = requests.recv() {
                    reply.send(()).unwrap();
                }
                let _ = exited.send(());
            });
            let (exited, observed) = mpsc::channel();
            owner.request(|reply| (reply, exited)).unwrap();
            owner.shutdown();
            assert_eq!(observed.try_recv(), Ok(()));

            let result = owner.request(|reply| (reply, mpsc::channel().0));
            assert!(matches!(
                result,
                Err(HidError::IoError { error }) if error.kind() == io::ErrorKind::BrokenPipe
            ));
        }
    }
}
