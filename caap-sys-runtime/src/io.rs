use std::io::{BufRead, Read, Write};

use crate::ffi_value::{val_to_display_string, SysArgs, SysError, SysResult, SysValue};

pub fn invoke(name: &str, args: SysArgs) -> SysResult {
    match name {
        "print" => write_and_flush(
            std::io::stdout(),
            &val_to_display_string(args.require_value(0, "io.print")?),
            "io.print",
        ),
        "println" => write_and_flush(
            std::io::stdout(),
            &format!(
                "{}\n",
                val_to_display_string(args.require_value(0, "io.println")?)
            ),
            "io.println",
        ),
        "write" => {
            let text = args.require_str(0, "io.write")?;
            write_and_flush(std::io::stdout(), &text, "io.write")
        }
        "write_bytes" => {
            let bytes = args.require_bytes(0, "io.write_bytes")?;
            let mut out = std::io::stdout();
            out.write_all(&bytes)
                .map_err(|error| SysError::from_io("io.write_bytes", error))?;
            out.flush()
                .map_err(|error| SysError::from_io("io.write_bytes", error))?;
            Ok(SysValue::Null)
        }
        "eprint" => write_and_flush(
            std::io::stderr(),
            &val_to_display_string(args.require_value(0, "io.eprint")?),
            "io.eprint",
        ),
        "eprintln" => write_and_flush(
            std::io::stderr(),
            &format!(
                "{}\n",
                val_to_display_string(args.require_value(0, "io.eprintln")?)
            ),
            "io.eprintln",
        ),
        "flush_stdout" => flush_writer(std::io::stdout(), "io.flush_stdout"),
        "flush_stderr" => flush_writer(std::io::stderr(), "io.flush_stderr"),
        "read_line" => {
            let mut line = String::new();
            match std::io::stdin().lock().read_line(&mut line) {
                Ok(0) => Ok(SysValue::Null),
                Ok(_) => Ok(SysValue::Str(line)),
                Err(e) => Err(SysError::from_io("io.read_line", e)),
            }
        }
        "read_all" => {
            let mut text = String::new();
            std::io::stdin()
                .lock()
                .read_to_string(&mut text)
                .map_err(|e| SysError::from_io("io.read_all", e))?;
            Ok(SysValue::Str(text))
        }
        _ => Err(format!("io: unknown export '{name}'").into()),
    }
}

fn write_and_flush(mut writer: impl Write, text: &str, ctx: &str) -> SysResult {
    writer
        .write_all(text.as_bytes())
        .map_err(|error| format!("{ctx}: {error}"))?;
    flush_writer(writer, ctx)
}

fn flush_writer(mut writer: impl Write, ctx: &str) -> SysResult {
    writer.flush().map_err(|error| format!("{ctx}: {error}"))?;
    Ok(SysValue::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "closed",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "closed",
            ))
        }
    }

    #[test]
    fn write_and_flush_reports_writer_errors() {
        let error = write_and_flush(FailingWriter, "hello", "io.test").unwrap_err();
        assert!(error.contains("io.test"));
        assert!(error.contains("closed"));
    }

    #[test]
    fn flush_writer_reports_flush_errors() {
        let error = flush_writer(FailingWriter, "io.flush_test").unwrap_err();
        assert!(error.contains("io.flush_test"));
        assert!(error.contains("closed"));
    }
}
