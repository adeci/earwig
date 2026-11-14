//! A Wayland connection served on its own thread for the input-method and
//! paste backends: it keeps dispatching between insertions (so the clipboard
//! stays served), and [`Worker::call`] runs closures on it.

use std::io::ErrorKind;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use rustix::event::{EventfdFlags, PollFd, PollFlags};
use rustix::io::Errno;
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};

/// How long a caller waits for the thread (connecting, or one call) before
/// giving up on a compositor that stopped answering.
const CALL_TIMEOUT: Duration = Duration::from_secs(3);

type Job<S> = Box<dyn FnOnce(&mut S, &mut EventQueue<S>) + Send>;

pub(super) struct Worker<S> {
    /// `None` only while dropping, so the thread sees the channel close.
    jobs: Option<flume::Sender<Job<S>>>,
    wake: Arc<OwnedFd>,
    /// Disconnects once the thread has closed its connection.
    closed: flume::Receiver<()>,
}

impl<S> Worker<S>
where
    S: Dispatch<WlRegistry, GlobalListContents> + 'static,
{
    /// Connect on a new thread, build the state with `init` (bind globals,
    /// create objects), and do one roundtrip before serving.
    pub(super) fn spawn<F>(name: &str, init: F) -> anyhow::Result<Self>
    where
        F: FnOnce(&GlobalList, &QueueHandle<S>) -> anyhow::Result<S> + Send + 'static,
    {
        let wake = Arc::new(
            rustix::event::eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
                .context("eventfd")?,
        );
        let (jobs, job_rx) = flume::unbounded::<Job<S>>();
        let (ready_tx, ready_rx) = flume::bounded(1);
        let (closed_tx, closed) = flume::bounded::<()>(0);
        let thread_wake = wake.clone();
        let thread_name = name.to_owned();
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                // Dropped last, after the connection.
                let _closed = closed_tx;
                let setup = || -> anyhow::Result<(EventQueue<S>, S)> {
                    let conn = Connection::connect_to_env().context("connecting to Wayland")?;
                    let (globals, mut queue) =
                        registry_queue_init::<S>(&conn).context("listing Wayland globals")?;
                    let mut state = init(&globals, &queue.handle())?;
                    queue.roundtrip(&mut state).context("Wayland roundtrip")?;
                    Ok((queue, state))
                };
                let (queue, state) = match setup() {
                    Ok(served) => {
                        let _ = ready_tx.send(Ok(()));
                        served
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(err));
                        return;
                    }
                };
                if let Err(err) = serve(queue, state, &job_rx, &thread_wake) {
                    log::warn!("{thread_name}: Wayland connection lost: {err:#}");
                }
            })
            .context("spawning the Wayland thread")?;
        match ready_rx.recv_timeout(CALL_TIMEOUT) {
            Ok(result) => result?,
            Err(flume::RecvTimeoutError::Timeout) => {
                anyhow::bail!("the compositor did not answer within {CALL_TIMEOUT:?}")
            }
            Err(flume::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("the Wayland thread exited during setup")
            }
        }
        Ok(Self {
            jobs: Some(jobs),
            wake,
            closed,
        })
    }

    /// Run `f` on the Wayland thread and wait for its result.
    pub(super) fn call<R, F>(&self, f: F) -> anyhow::Result<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut S, &mut EventQueue<S>) -> R + Send + 'static,
    {
        let (tx, rx) = flume::bounded(1);
        self.jobs
            .as_ref()
            .expect("jobs is only taken on drop")
            .send(Box::new(move |state, queue| {
                // The caller timed out: acting now would hit whatever has
                // focus by then.
                if !tx.is_disconnected() {
                    let _ = tx.send(f(state, queue));
                }
            }))
            .map_err(|_| anyhow!("the Wayland connection is closed"))?;
        self.wake();
        rx.recv_timeout(CALL_TIMEOUT).map_err(|err| match err {
            flume::RecvTimeoutError::Timeout => {
                anyhow!("the compositor did not answer within {CALL_TIMEOUT:?}")
            }
            flume::RecvTimeoutError::Disconnected => anyhow!("the Wayland connection is closed"),
        })
    }

    fn wake(&self) {
        // Only fails when the counter would overflow, which still wakes.
        let _ = rustix::io::write(&*self.wake, &1u64.to_ne_bytes());
    }
}

impl<S> Drop for Worker<S> {
    fn drop(&mut self) {
        self.jobs = None;
        let _ = rustix::io::write(&*self.wake, &1u64.to_ne_bytes());
        // Only one input method may hold the seat: an inserter replacing
        // this one can bind it once this connection is gone.
        let _ = self.closed.recv_timeout(CALL_TIMEOUT);
    }
}

fn serve<S: 'static>(
    mut queue: EventQueue<S>,
    mut state: S,
    jobs: &flume::Receiver<Job<S>>,
    wake: &OwnedFd,
) -> anyhow::Result<()> {
    loop {
        queue.dispatch_pending(&mut state)?;
        loop {
            match jobs.try_recv() {
                Ok(job) => job(&mut state, &mut queue),
                Err(flume::TryRecvError::Empty) => break,
                Err(flume::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        queue.flush()?;
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let (readable, woken) = {
            let conn = guard.connection_fd();
            let mut fds = [
                PollFd::new(&conn, PollFlags::IN),
                PollFd::new(wake, PollFlags::IN),
            ];
            loop {
                match rustix::event::poll(&mut fds, None) {
                    Ok(_) => break,
                    Err(Errno::INTR) => {}
                    Err(err) => return Err(err.into()),
                }
            }
            (!fds[0].revents().is_empty(), !fds[1].revents().is_empty())
        };
        if readable {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(err)) if err.kind() == ErrorKind::WouldBlock => {}
                Err(err) => return Err(err.into()),
            }
        } else {
            drop(guard);
        }
        if woken {
            let _ = rustix::io::read(wake, &mut [0u8; 8]);
        }
    }
}
