//! Redirect the process's stdout (fd 1) to a pipe so that output produced by
//! the *debugged program* (e.g. `io.println` at runtime) does not corrupt the
//! DAP protocol — which itself rides on the original stdout. The DAP loop keeps
//! writing to the saved original fd; captured program output is surfaced as DAP
//! `output` events. Unix-only (the target platform).

use std::fs::File;
use std::os::unix::io::{FromRawFd, RawFd};

fn close_fd(fd: RawFd) {
    if fd >= 0 {
        // SAFETY: `fd` is an owned raw descriptor in this function's failure path.
        unsafe {
            libc::close(fd);
        }
    }
}

/// Redirect fd 1 to a fresh pipe.
///
/// Returns `(dap_out, captured)`:
/// - `dap_out`: a `File` wrapping the *original* stdout — use this for all DAP
///   protocol writes.
/// - `captured`: the read end of the pipe; everything the program writes to
///   stdout arrives here.
pub fn redirect_stdout() -> std::io::Result<(File, File)> {
    unsafe {
        let saved = libc::dup(libc::STDOUT_FILENO);
        if saved < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut fds = [0 as RawFd; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            close_fd(saved);
            return Err(std::io::Error::last_os_error());
        }
        let (read_fd, write_fd) = (fds[0], fds[1]);
        if libc::dup2(write_fd, libc::STDOUT_FILENO) < 0 {
            let err = std::io::Error::last_os_error();
            close_fd(saved);
            close_fd(read_fd);
            close_fd(write_fd);
            return Err(err);
        }
        libc::close(write_fd);
        Ok((File::from_raw_fd(saved), File::from_raw_fd(read_fd)))
    }
}
