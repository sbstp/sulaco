use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead};
use tokio::process;
use tokio::sync::mpsc;

use crate::unix;
use crate::Event;

enum StdStream {
    Stdout,
    Stderr,
}

pub struct RunningProcess {
    pub name: String,
    pub handle: process::Child,
    pub pid: u32,
    pub instance_id: u64,
    pub sender: mpsc::Sender<Event>,
}

impl RunningProcess {
    pub fn spawn(name: &str, sender: mpsc::Sender<Event>, mut cmd: process::Command) -> anyhow::Result<RunningProcess> {
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        unsafe {
            cmd.pre_exec(|| {
                unix::new_process_group();
                Ok(())
            });
        };

        let mut child = cmd.spawn()?;

        let stdout = child.stdout.take().expect("stdout not piped");
        let stderr = child.stderr.take().expect("stderr not piped");

        tokio::spawn(Self::stream_stdio(name.to_string(), stdout, StdStream::Stdout));
        tokio::spawn(Self::stream_stdio(name.to_string(), stderr, StdStream::Stderr));

        let pid = child.id().expect("child has no pid");

        Ok(RunningProcess {
            name: name.to_string(),
            handle: child,
            pid,
            instance_id: Self::next_instance_id(),
            sender,
        })
    }

    fn next_instance_id() -> u64 {
        static INSTANCE_ID_COUNTER: AtomicU64 = AtomicU64::new(0);
        INSTANCE_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
    }

    async fn stream_stdio(tag: String, reader: impl AsyncRead + Unpin, stream: StdStream) {
        let br = tokio::io::BufReader::new(reader);
        let mut lines = br.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match stream {
                StdStream::Stdout => println!("[{}] {}", tag, line),
                StdStream::Stderr => eprintln!("[{}] {}", tag, line),
            }
        }
    }

    pub fn terminate(&self, timeout: Duration, stop_signal: unix::Signal) {
        eprintln!(
            "[~sulaco] sending {} to [{}] (pid={})",
            stop_signal, self.name, self.pid
        );
        unix::kill(self.pid, stop_signal);

        let sender = self.sender.clone();
        let pid = self.pid;
        let instance_id = self.instance_id;
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            let _ = sender.send(Event::ForceShutdown { pid, instance_id }).await;
        });
    }

    pub fn force_terminate(&self) {
        eprintln!("[~sulaco] sending SIGKILL to [{}] (pid={})", self.name, self.pid);
        unix::kill(self.pid, unix::Signal::Sigkill);
    }
}
