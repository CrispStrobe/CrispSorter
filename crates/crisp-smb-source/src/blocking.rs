//! Dedicated runtime thread for the synchronous CloudDrive contract. Safe to
//! construct/drop/call from a Tauri Tokio task: no nested block_on or runtime drop.
use crate::{Config, Error, Result, Session};
use serde_json::Value;
use std::sync::{mpsc, Arc};
use tokio::runtime::Runtime;

type Job = Box<dyn FnOnce(&Runtime, &Session) + Send>;
pub struct BlockingSession {
    sender: Option<mpsc::SyncSender<Job>>,
    session: Arc<Session>,
}
impl BlockingSession {
    pub fn new(config: Config) -> Result<Self> {
        let session = Arc::new(Session::new(config)?);
        let worker_session = session.clone();
        let (sender, receiver) = mpsc::sync_channel::<Job>(8);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("crisp-smb".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _ = ready_tx.send(false);
                        return;
                    }
                };
                let _ = ready_tx.send(true);
                for job in receiver {
                    job(&runtime, &worker_session);
                }
                runtime.block_on(worker_session.close());
            })
            .map_err(|_| Error::new("Unavailable"))?;
        if !ready_rx.recv().unwrap_or(false) {
            return Err(Error::new("Unavailable"));
        }
        Ok(Self {
            sender: Some(sender),
            session,
        })
    }
    fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Runtime, &Session) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.sender
            .as_ref()
            .ok_or_else(|| Error::new("Cancelled"))?
            .send(Box::new(move |rt, s| {
                let _ = sender.send(f(rt, s));
            }))
            .map_err(|_| Error::new("Unavailable"))?;
        receiver.recv().map_err(|_| Error::new("Unavailable"))?
    }
    pub fn request(&self, op: &str, path: &str, other: &str, handle: u64) -> Result<Value> {
        let (op, path, other) = (op.to_owned(), path.to_owned(), other.to_owned());
        self.call(move |rt, s| rt.block_on(s.request(&op, &path, &other, handle)))
    }
    pub fn read_chunk(&self, handle: u64, offset: u64, length: u32) -> Result<Vec<u8>> {
        self.call(move |rt, s| rt.block_on(s.read_chunk(handle, offset, length)))
    }
    pub fn write_chunk(&self, handle: u64, bytes: Vec<u8>) -> Result<()> {
        self.call(move |rt, s| rt.block_on(s.write_chunk(handle, bytes)))
    }
    pub fn cancel(&self) {
        self.session.cancel();
    }
}
impl Drop for BlockingSession {
    fn drop(&mut self) {
        self.session.cancel();
        self.sender.take(); // worker closes handles and drops its own runtime
    }
}
