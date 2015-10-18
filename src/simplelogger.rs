extern crate log;

use log::{LogRecord, LogLevel, LogMetadata, SetLoggerError, LogLevelFilter};

struct SimpleLogger;

impl log::Log for SimpleLogger {
    fn enabled(&self, metadata: &LogMetadata) -> bool {
        // Todo: implement this or use an existing library
        true
    }

    fn log(&self, record: &LogRecord) {
        if self.enabled(record.metadata()) {
            println!("{} - {}", record.level(), record.args());
        }
    }
}

pub fn init(level: &str) -> Result<(), SetLoggerError> {
    let log_level_filter = match level {
        "error" => LogLevelFilter::Error,
        "warn" => LogLevelFilter::Warn,
        "info" => LogLevelFilter::Info,
        "debug" => LogLevelFilter::Debug,
        _ => panic!("Unknown log level {}", level)
    };
    log::set_logger(|max_log_level| {
        max_log_level.set(log_level_filter);
        Box::new(SimpleLogger)
    })
}
