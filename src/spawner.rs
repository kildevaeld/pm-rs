use futures_lite::Future;

pub trait Spawner {
    fn spawn<F: Future + 'static + Send>(&self, future: F)
    where
        F::Output: Send;
}

#[cfg(feature = "smol")]
impl<'a> Spawner for smol::Executor<'a> {
    fn spawn<F: Future + 'static + Send>(&self, future: F)
    where
        F::Output: Send,
    {
        self.spawn(future).detach()
    }
}

#[cfg(feature = "smol")]
pub struct SmolGlobalSpawner;

#[cfg(feature = "smol")]
impl Spawner for SmolGlobalSpawner {
    fn spawn<F: Future + 'static + Send>(&self, future: F)
    where
        F::Output: Send,
    {
        smol::spawn(future).detach()
    }
}
