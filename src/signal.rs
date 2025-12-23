use tokio::sync::{OnceCell, broadcast};

pub type Shutdown = broadcast::Sender<()>;

static SHUTDOWN: OnceCell<Shutdown> = OnceCell::const_new();

pub fn shutdown_signal() -> impl Future<Output = ()> + Send + 'static {
    let mut rx = SHUTDOWN
        .get()
        .map(|tx| tx.subscribe())
        .expect("shutdown signal not initialized");
    async move {
        let _ = rx.recv().await;
    }
}

pub fn register_shutdown_listeners() {
    use futures::FutureExt;
    use tokio::signal::ctrl_c;

    let (tx, _) = broadcast::channel(1);

    let shutdown_fut = async move {
        ctrl_c().await.ok();
    };

    {
        let tx = tx.clone();
        tokio::spawn(shutdown_fut.then(|unit| async move {
            let exit_code = tx.send(unit);

            if let Err(err) = exit_code
                && tx.receiver_count() > 0
            {
                panic!("cannot send shutdown signal: {err}");
            }
        }));
    }

    if SHUTDOWN.set(tx).is_err() {
        panic!("shutdown signal already initialized");
    }
}
