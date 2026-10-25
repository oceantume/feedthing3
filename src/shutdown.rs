//! The stop signal everything that runs until shutdown watches: the server
//! and the scheduler.

use tokio::{
    signal::unix::{SignalKind, signal},
    sync::watch,
};

pub type Signal = watch::Receiver<bool>;

/// Turns true on Ctrl-C or SIGTERM (what Docker sends).
pub fn on_signal() -> Signal {
    let (tx, rx) = watch::channel(false);
    let mut terminate = signal(SignalKind::terminate()).expect("installing the SIGTERM handler");
    tokio::spawn(async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
        tracing::info!("shutting down");
        tx.send_replace(true);
    });
    rx
}

/// Resolves once the signal has fired.
pub async fn requested(mut signal: Signal) {
    // An error means the sender is gone, which only happens after it fired.
    let _ = signal.wait_for(|stop| *stop).await;
}
