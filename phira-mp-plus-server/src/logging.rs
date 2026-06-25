use anyhow::Result;
use std::io::{self, Write};
use std::path::Path;
use tokio::sync::mpsc;
use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

enum ChannelWriter {
    Sink,
    Channel(mpsc::UnboundedSender<String>, Vec<u8>),
}

impl Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Sink => Ok(buf.len()),
            Self::Channel(_, pending) => {
                pending.extend_from_slice(buf);
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Self::Channel(tx, pending) = self {
            if !pending.is_empty() {
                let message = String::from_utf8_lossy(pending).into_owned();
                pending.clear();
                let _ = tx.send(message);
            }
        }
        Ok(())
    }
}

impl Drop for ChannelWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

struct ChannelWriterFactory(Option<mpsc::UnboundedSender<String>>);

impl<'a> fmt::MakeWriter<'a> for ChannelWriterFactory {
    type Writer = ChannelWriter;

    fn make_writer(&'a self) -> Self::Writer {
        match &self.0 {
            Some(tx) => ChannelWriter::Channel(tx.clone(), Vec::new()),
            None => ChannelWriter::Sink,
        }
    }
}

struct StdoutWriter(bool);

impl<'a> fmt::MakeWriter<'a> for StdoutWriter {
    type Writer = Box<dyn Write + Send + Sync>;

    fn make_writer(&'a self) -> Self::Writer {
        if self.0 {
            Box::new(io::sink())
        } else {
            Box::new(io::stdout())
        }
    }
}

pub fn init(
    file_name: &str,
    tui_tx: Option<mpsc::UnboundedSender<String>>,
) -> Result<WorkerGuard> {
    let log_dir = Path::new("log");
    if log_dir.exists() && !log_dir.is_dir() {
        anyhow::bail!("'log' exists and is not a directory");
    }
    std::fs::create_dir_all(log_dir)?;

    let (file_writer, guard) = tracing_appender::non_blocking(
        tracing_appender::rolling::hourly(log_dir, file_name),
    );
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive("hyper=info".parse()?)
        .add_directive("rustls=info".parse()?)
        .add_directive("isahc=info".parse()?)
        .add_directive("h2=info".parse()?)
        .add_directive("reqwest=info".parse()?)
        .add_directive("wasmtime=info".parse()?);

    let has_tui = tui_tx.is_some();
    let file_layer = fmt::layer()
        .with_writer(file_writer)
        .with_ansi(false)
        .with_filter(LevelFilter::TRACE);
    let stdout_layer = fmt::layer()
        .with_writer(StdoutWriter(has_tui))
        .with_ansi(false)
        .with_filter(filter.clone());
    let tui_layer = fmt::layer()
        .with_writer(ChannelWriterFactory(tui_tx))
        .with_ansi(false)
        .with_filter(filter);

    tracing_subscriber::registry()
        .with(file_layer)
        .with(stdout_layer)
        .with(tui_layer)
        .init();

    Ok(guard)
}
