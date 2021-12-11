use crate::command::Command;
use crate::Error;
use async_broadcast::{broadcast, Receiver};
use async_channel::{unbounded, Sender};
use async_lock::RwLock;
use async_oneshot::{oneshot, Sender as OneshotSender};
use async_process::{Child, ExitStatus, Stdio};
use futures_lite::stream::StreamExt;
use futures_lite::{io::BufReader, AsyncBufReadExt, Stream};
use generational_arena::{Arena, Index};
use log::debug;
use runtime::Runtime;
use signal_child::signal::Signal;
use std::{fmt, sync::Arc};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pid(Index);

impl fmt::Debug for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (idx, gen) = self.0.into_raw_parts();
        f.debug_tuple("Pid").field(&idx).field(&gen).finish()
    }
}

pub trait Factory: Send + Sync {
    fn call(&self) -> Command;
}

impl<F> Factory for F
where
    F: Fn() -> Command + Send + Sync,
{
    fn call(&self) -> Command {
        (self)()
    }
}

pub struct ProxyChild {}

#[derive(Debug)]
enum EntryState {
    Stopped,
    Running(u32),
    Error(Error),
    Status(ExitStatus),
}

impl EntryState {
    fn is_stopped(&self) -> bool {
        match self {
            EntryState::Stopped | EntryState::Status(_) | EntryState::Error(_) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running(u32),
    Crated,
    Exited(ExitStatus),
    Error,
}

impl From<EntryState> for Status {
    fn from(state: EntryState) -> Status {
        match state {
            EntryState::Error(_) => Status::Error,
            EntryState::Running(id) => Status::Running(id),
            EntryState::Status(status) => Status::Exited(status),
            EntryState::Stopped => Status::Crated,
        }
    }
}

impl<'a> From<&'a EntryState> for Status {
    fn from(state: &'a EntryState) -> Status {
        match state {
            EntryState::Error(_) => Status::Error,
            EntryState::Running(id) => Status::Running(*id),
            EntryState::Status(status) => Status::Exited(*status),
            EntryState::Stopped => Status::Crated,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessStatus {
    pub pid: Pid,
    pub status: Status,
}

impl ProcessStatus {
    pub fn is_stopped(&self) -> bool {
        match &self.status {
            Status::Crated | Status::Exited(_) | Status::Error => true,
            _ => false,
        }
    }
}

struct Entry {
    command: Command,
    state: EntryState,
}

enum Request {
    Create(Entry, OneshotSender<Pid>),
    Start(Pid),
    Signal(Pid, Signal, OneshotSender<Result<(), std::io::Error>>),
    Remove(Pid),
    Exited(Pid, ExitStatus),
    Error(Pid, std::io::Error),
    Status(OneshotSender<Vec<ProcessStatus>>),
    Destroy(OneshotSender<()>),
}

struct ManagerInner {
    sx: Sender<Request>,
    rx: Receiver<ProcessStatus>,
}

impl Drop for ManagerInner {
    fn drop(&mut self) {
        futures_lite::future::block_on(async move {
            //
            let (sx, rx) = oneshot();
            self.sx.send(Request::Destroy(sx)).await.ok();

            rx.await
        })
        .ok();
    }
}

#[derive(Clone)]
pub struct Manager(Arc<ManagerInner>);

// #[cfg(feature = "smol")]
// impl Manager {
//     pub fn smol() -> Manager {
//         Manager::new(SmolGlobalSpawner)
//     }

//     pub fn smol_with<W: ProcessWriter + 'static>(writer: W) -> Manager {
//         Manager::new_with(SmolGlobalSpawner, writer)
//     }
// }

pub trait ProcessWriter: Send + Sync {
    fn stdout(&self, pid: &Pid, line: String);
    fn stderr(&self, pid: &Pid, line: String);
}

struct NullWriter;

impl ProcessWriter for NullWriter {
    #[allow(unused)]
    fn stdout(&self, pid: &Pid, line: String) {}
    #[allow(unused)]
    fn stderr(&self, pid: &Pid, line: String) {}
}

impl Manager {
    pub fn new<E: Runtime + 'static>(executor: E) -> Manager {
        Manager::new_with(executor, NullWriter)
    }
    pub fn new_with<E: Runtime + 'static, W: ProcessWriter + 'static>(
        executor: E,
        writer: W,
    ) -> Manager {
        let executor = Arc::new(executor);

        let (sx, mut events) = unbounded();
        let (bx, rx) = broadcast(10);
        let sx_clone = sx.clone();

        let writer = Arc::new(writer);

        executor.clone().spawn(async move {
            let arena = Arc::new(RwLock::new(Arena::new()));
            while let Some(next) = events.next().await {
                match next {
                    Request::Create(entry, mut reply) => {
                        let idx = arena.write().await.insert(entry);
                        debug!("create entry {:?}", Pid(idx));
                        bx.broadcast(ProcessStatus {
                            pid: Pid(idx),
                            status: (&arena.read().await.get(idx).unwrap().state).into(),
                        })
                        .await
                        .ok();
                        reply.send(Pid(idx)).ok();
                    }
                    Request::Remove(idx) => {
                        let lock = arena.read().await;
                        let entry = lock.get(idx.0).unwrap();
                        if let EntryState::Running(id) = &entry.state {
                            let id = *id;

                            executor
                                .unblock(move || signal_child::signal(id as i32, Signal::SIGKILL))
                                .await
                                .ok();
                        }
                        let state = (&entry.state).into();
                        drop(lock);
                        arena.write().await.remove(idx.0);
                        bx.broadcast(ProcessStatus {
                            pid: idx,
                            status: state,
                        })
                        .await
                        .ok();
                    }
                    Request::Start(idx) => {
                        let mut arena = arena.write().await;
                        let entry = arena.get_mut(idx.0).unwrap();

                        if !entry.state.is_stopped() {
                            continue;
                        }

                        let mut cmd = entry.command.clone().into_process();

                        cmd.stdin(Stdio::null())
                            .stdout(Stdio::piped())
                            .stderr(Stdio::piped());

                        let mut child = match cmd.spawn() {
                            Ok(child) => child,
                            Err(err) => {
                                entry.state = EntryState::Error(Error::new(Pid(idx.0), err.into()));
                                bx.broadcast(ProcessStatus {
                                    pid: idx,
                                    status: (&entry.state).into(),
                                })
                                .await
                                .ok();
                                continue;
                            }
                        };

                        let stdout = child.stdout.take().unwrap();
                        let stderr = child.stderr.take().unwrap();
                        let writer = writer.clone();

                        executor.spawn(async move {
                            let mut output = BufReader::new(stdout)
                                .lines()
                                .map(|line| {
                                    let line = match line {
                                        Ok(line) => line,
                                        Err(err) => return Err(err),
                                    };

                                    Ok((true, line))
                                })
                                .race(BufReader::new(stderr).lines().map(|line| {
                                    let line = match line {
                                        Ok(line) => line,
                                        Err(err) => return Err(err),
                                    };

                                    Ok((true, line))
                                }));

                            while let Some(next) = output.next().await {
                                let (stdout, line) = match next {
                                    Ok(ret) => ret,
                                    Err(_) => return,
                                };

                                if stdout {
                                    writer.stdout(&idx, line);
                                } else {
                                    writer.stderr(&idx, line);
                                }
                            }
                        });

                        debug!("started entry {:?} {:?}", idx, child.id());

                        entry.state = EntryState::Running(child.id());

                        bx.broadcast(ProcessStatus {
                            pid: idx,
                            status: (&entry.state).into(),
                        })
                        .await
                        .ok();

                        let sx = sx_clone.clone();
                        executor.spawn(async move {
                            let req = spawn_child(child, idx).await;
                            sx.send(req).await.ok()
                        });
                    }
                    Request::Signal(idx, signal, mut reply) => {
                        //
                        let arena = arena.read().await;
                        let entry = arena.get(idx.0).unwrap();

                        if let EntryState::Running(id) = &entry.state {
                            let id = *id;
                            let ret = executor
                                .unblock(move || signal_child::signal(id as i32, signal))
                                .await;

                            reply.send(ret).ok();
                        }
                    }
                    Request::Exited(idx, status) => {
                        let mut arena = arena.write().await;
                        let entry = arena.get_mut(idx.0).unwrap();
                        debug!("process exited {}", status);
                        entry.state = EntryState::Status(status);
                        bx.broadcast(ProcessStatus {
                            pid: idx,
                            status: (&entry.state).into(),
                        })
                        .await
                        .ok();
                    }
                    Request::Error(idx, err) => {
                        let mut arena = arena.write().await;
                        let entry = arena.get_mut(idx.0).unwrap();
                        debug!("process error: {}", err);
                        entry.state = EntryState::Error(Error::new(Pid(idx.0), err.into()));
                        bx.broadcast(ProcessStatus {
                            pid: idx,
                            status: (&entry.state).into(),
                        })
                        .await
                        .ok();
                    }
                    Request::Status(mut reply) => {
                        let status = arena
                            .read()
                            .await
                            .iter()
                            .map(|entry| ProcessStatus {
                                pid: Pid(entry.0),
                                status: match &entry.1.state {
                                    EntryState::Error(_) => Status::Error,
                                    EntryState::Running(id) => Status::Running(*id),
                                    EntryState::Status(exit) => Status::Exited(*exit),
                                    EntryState::Stopped => Status::Crated,
                                },
                            })
                            .collect();

                        reply.send(status).ok();
                    }
                    Request::Destroy(mut signal) => {
                        signal.send(()).ok();
                        break;
                    }
                }
            }

            debug!("closing runloop");
        });

        Manager(Arc::new(ManagerInner { sx, rx }))
    }

    pub async fn spawn(&self, cmd: Command) -> Pid {
        let pid = self.create(cmd).await;
        self.start(&pid).await;
        pid
    }

    pub async fn create(&self, command: Command) -> Pid {
        let entry = Entry {
            command,
            state: EntryState::Stopped,
        };

        let (sx, rx) = oneshot();
        self.0.sx.send(Request::Create(entry, sx)).await.ok();

        rx.await.unwrap()
    }

    pub async fn start(&self, pid: &Pid) {
        self.0.sx.send(Request::Start(*pid)).await.ok();
    }

    pub async fn stop(&self, pid: &Pid) -> Result<(), Error> {
        self.signal(pid, Signal::SIGKILL).await
    }

    pub async fn stop_all(&self) {
        let ps = self.list().await;

        for p in ps {
            self.stop(&p.pid).await.ok();
        }
    }

    pub async fn remove(&self, pid: &Pid) -> Result<(), Error> {
        self.0.sx.send(Request::Remove(*pid)).await.ok();
        Ok(())
    }

    pub async fn signal(&self, pid: &Pid, signal: Signal) -> Result<(), Error> {
        let (sx, rx) = oneshot();
        self.0.sx.send(Request::Signal(*pid, signal, sx)).await.ok();

        match rx.await {
            Ok(out) => out.map_err(|err| Error::new(*pid, err.into())),
            Err(_) => Ok(()),
        }
    }

    pub async fn list(&self) -> Vec<ProcessStatus> {
        let (sx, rx) = oneshot();
        self.0.sx.send(Request::Status(sx)).await.ok();

        match rx.await {
            Ok(out) => out,
            Err(_) => vec![],
        }
    }

    pub async fn stat(&self, pid: &Pid) -> Option<ProcessStatus> {
        self.list().await.into_iter().find(|m| &m.pid == pid)
    }

    pub fn watch_pid(&self, pid: Pid) -> impl Stream<Item = ProcessStatus> + 'static + Send + Sync {
        self.watch().filter_map(
            move |event| {
                if event.pid == pid {
                    Some(event)
                } else {
                    None
                }
            },
        )
    }

    // pub async fn wait_pid(&self, pid: Pid) -> Result<ExitStatus, Error> {
    //     while let Some(next) = self.watch_pid(pid).next().await {
    //         match next {
    //             Event::Errored(err) =>
    //         }
    //     }

    //     Ok(())
    // }

    pub fn watch(&self) -> impl Stream<Item = ProcessStatus> + 'static + Send + Sync {
        self.0.rx.clone()
    }

    pub async fn wait(self) {
        let done = self.list().await.iter().all(|a| a.is_stopped());

        if done {
            return;
        }
        while let Some(_) = self.watch().next().await {
            let status = self.list().await;
            let done = status.iter().all(|a| a.is_stopped());

            if done {
                break;
            }
        }
    }
}

async fn spawn_child(mut child: Child, idx: Pid) -> Request {
    let status = child.status().await;
    let req = match status {
        Ok(ret) => Request::Exited(idx, ret),
        Err(err) => Request::Error(idx, err),
    };
    req
}
