use std::sync::Arc;

use anyhow::Context;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;

use state::AppState;
use unix::Termination;

mod process;
mod spec;
mod state;
mod unix;

#[derive(Debug)]
pub enum Event {
    ForceShutdown { pid: u32, instance_id: u64 },
    ProcessTerminated(Termination),
    ServiceRestart { name: Arc<String> },
    Shutdown,
    TermSignal,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
    tokio::spawn(listen_sigchld(sender.clone()));
    tokio::spawn(handle_signal(sender.clone(), SignalKind::interrupt()));
    tokio::spawn(handle_signal(sender.clone(), SignalKind::terminate()));

    let conf = spec::ConfigFile::load(&args[1]).context("could not load config file")?;
    let conf = Arc::new(conf);

    let mut state = AppState::new(&conf, &sender);

    if conf.init.is_empty() {
        state.services.start_all();
    } else {
        state.inits.start_all();
    }

    while !state.should_exit() {
        let event = receiver.recv().await.expect("event channel closed unexpectedly");

        match event {
            Event::ProcessTerminated(term) => {
                if state.inits.owns_pid(term.pid()) {
                    state.inits.terminated(term);
                    if !conf.init.is_empty() && state.inits.all_done() && !state.shutdown {
                        eprintln!("[~sulaco] all init processed done, starting services");
                        state.services.start_all();
                    }
                } else if state.services.owns_pid(term.pid()) {
                    state.services.terminated(term).await;
                } else {
                    eprintln!("[~sulaco] zombie reaped (pid={})...", term.pid());
                }
            }
            Event::ForceShutdown { pid, instance_id } => {
                state.inits.force_terminate(pid, instance_id);
                state.services.force_terminate(pid, instance_id);
            }
            Event::ServiceRestart { name } => {
                state.services.start(&name);
            }
            Event::Shutdown => {
                state.shutdown();
            }
            Event::TermSignal => {
                eprintln!("[~sulaco] term signal received, shutting down...");
                state.shutdown();
            }
        }
    }

    Ok(())
}

// Listen to the SIGCHLD signal and call waitpid when the signal is received.
// After that, the termination event, if any, is sent through the channel.
// The waitpid function will both reap zombies and catch termination of children
// processes. Events referring to zombies can be ignored but events tied to a
// child process we know might need to be handled, for instance restarting
// a killed child process or failed child process.
async fn listen_sigchld(sender: mpsc::Sender<Event>) {
    let mut signals = signal(SignalKind::child()).expect("could not create SIGCHLD signal handler");

    while signals.recv().await.is_some() {
        // WNOHANG makes the call to waitpid nonblocking. When we receive a
        // SIGCHLD signal, we call waitpid multiple times because it's possible
        // that multiple children have exited for a single signal reception.
        // We stop looking for exited children when waitpid returns an error
        // or 0, signaling it would hang.
        loop {
            match unix::wait_termination() {
                unix::WaitTermination::Termination(term) => {
                    if sender.send(Event::ProcessTerminated(term)).await.is_err() {
                        return;
                    }
                }
                unix::WaitTermination::Retry => continue,
                unix::WaitTermination::Stop => break,
            }
        }
    }
}

// Listen to signals sent to sulaco process and send the signal down the
// channel when one is received.
async fn handle_signal(sender: mpsc::Sender<Event>, sig: SignalKind) {
    let mut signals = signal(sig).unwrap();
    while signals.recv().await.is_some() {
        if sender.send(Event::TermSignal).await.is_err() {
            break;
        }
    }
}
