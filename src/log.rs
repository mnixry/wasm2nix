use slog::{Drain as _, Logger};

#[derive(Clone, Copy, Debug)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    fn allows(self, lvl: slog::Level) -> bool {
        // Lower is "more severe" in slog; allow everything up to the selected
        // verbosity. Critical is treated like Error for our purposes.
        match self {
            LogLevel::Error => lvl <= slog::Level::Error,
            LogLevel::Warn => lvl <= slog::Level::Warning,
            LogLevel::Info => lvl <= slog::Level::Info,
            LogLevel::Debug => lvl <= slog::Level::Debug,
            LogLevel::Trace => true,
        }
    }
}

#[derive(Clone, Debug)]
struct StderrDrain {
    level: LogLevel,
}

impl slog::Drain for StderrDrain {
    type Ok = ();
    type Err = std::io::Error;

    fn log(&self, record: &slog::Record, _values: &slog::OwnedKVList) -> Result<(), Self::Err> {
        if !self.level.allows(record.level()) {
            return Ok(());
        }

        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        writeln!(stderr, "[{}] {}", record.level(), record.msg())?;
        Ok(())
    }
}

pub fn init_logger(level: LogLevel) -> Logger {
    let drain = StderrDrain { level }.fuse();
    Logger::root(drain, slog::o!())
}
