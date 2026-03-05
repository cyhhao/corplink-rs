use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use log::{Log, Metadata, Record};

const MAX_LINES: usize = 500;

/// Shared ring buffer holding recent log lines.
static LOG_BUFFER: std::sync::OnceLock<Mutex<VecDeque<String>>> = std::sync::OnceLock::new();

fn buffer() -> &'static Mutex<VecDeque<String>> {
    LOG_BUFFER.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_LINES)))
}

/// Return a snapshot of recent log lines.
pub fn recent_logs() -> Vec<String> {
    buffer().lock().unwrap().iter().cloned().collect()
}

/// Custom logger: writes to stderr + ring buffer + optional log file.
struct DualLogger {
    env_logger: env_logger::Logger,
    log_file: Option<Mutex<std::fs::File>>,
}

impl Log for DualLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.env_logger.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if !self.env_logger.enabled(record.metadata()) {
            return;
        }
        // Format the line
        let now = chrono::Local::now().format("%H:%M:%S%.3f");
        let line = format!("{} [{}] {}", now, record.level(), record.args());

        // 1. stderr (via env_logger)
        self.env_logger.log(record);

        // 2. ring buffer
        {
            let mut buf = buffer().lock().unwrap();
            if buf.len() >= MAX_LINES {
                buf.pop_front();
            }
            buf.push_back(line.clone());
        }

        // 3. file
        if let Some(ref f) = self.log_file {
            if let Ok(mut f) = f.lock() {
                let _ = writeln!(f, "{}", line);
            }
        }
    }

    fn flush(&self) {
        self.env_logger.flush();
        if let Some(ref f) = self.log_file {
            if let Ok(mut f) = f.lock() {
                let _ = f.flush();
            }
        }
    }
}

/// Initialize the dual logger. Call this once instead of `env_logger::init()`.
pub fn init(log_dir: PathBuf) {
    // Ensure log dir exists
    let _ = std::fs::create_dir_all(&log_dir);

    let log_file_path = log_dir.join("corplink.log");
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
        .ok()
        .map(|f| Mutex::new(f));

    let env_logger = env_logger::Builder::from_default_env().build();
    let max_level = env_logger.filter();

    let dual = DualLogger {
        env_logger,
        log_file,
    };

    log::set_boxed_logger(Box::new(dual)).unwrap();
    log::set_max_level(max_level);
}
