use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::ffi::OsStr;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ffi_value::{SysArgs, SysError, SysResult, SysValue};

// ── Handle tables ─────────────────────────────────────────────────────────────

const MAX_FS_HANDLES: usize = 1024;
/// Dir handles live in a separate ID range so they never collide with file
/// handles (`1..=MAX`, then `DIR_HANDLE_BASE..`).
const DIR_HANDLE_BASE: i64 = MAX_FS_HANDLES as i64 + 1;

/// Open file/dir handle tables for one runtime session.
///
/// State is owned explicitly by the caller (the FFI layer holds one in a
/// `thread_local`; the interpreter holds one per `HostServiceRegistry`) and
/// passed into every handle-based operation. This keeps the operation
/// semantics a single source of truth while letting each consumer scope
/// resource lifetime as it needs.
pub struct FsState {
    file_handles: std::collections::BTreeMap<i64, FsFileHandle>,
    dir_handles: std::collections::BTreeMap<i64, PathBuf>,
    // Freed IDs are recycled lowest-first so handle values stay small and
    // bounded; the high-water marks hand out fresh IDs until a free slot exists.
    free_file: BinaryHeap<Reverse<i64>>,
    free_dir: BinaryHeap<Reverse<i64>>,
    next_file: i64,
    next_dir: i64,
}

struct FsFileHandle {
    path: PathBuf,
    file: std::fs::File,
}

#[derive(Clone, Copy, Debug, Default)]
struct FsOpenFileFlags {
    read: bool,
    write: bool,
    append: bool,
    create: bool,
    create_new: bool,
    truncate: bool,
}

impl Default for FsState {
    fn default() -> Self {
        Self::new()
    }
}

impl FsState {
    pub fn new() -> Self {
        Self {
            file_handles: std::collections::BTreeMap::new(),
            dir_handles: std::collections::BTreeMap::new(),
            free_file: BinaryHeap::new(),
            free_dir: BinaryHeap::new(),
            next_file: 1,
            next_dir: DIR_HANDLE_BASE,
        }
    }

    // O(log n): reuse the lowest freed file-handle ID, else hand out the next
    // fresh one. IDs are recycled after close and never exceed the cap.
    fn alloc_file(&mut self) -> Result<i64, SysError> {
        if let Some(Reverse(id)) = self.free_file.pop() {
            return Ok(id);
        }
        if self.next_file <= MAX_FS_HANDLES as i64 {
            let id = self.next_file;
            self.next_file += 1;
            return Ok(id);
        }
        Err(SysError::resource_exhausted(
            "fs: too many open file handles (limit 1024)",
        ))
    }

    // O(log n): same policy as alloc_file over the separate dir-handle range.
    fn alloc_dir(&mut self) -> Result<i64, SysError> {
        if let Some(Reverse(id)) = self.free_dir.pop() {
            return Ok(id);
        }
        if self.next_dir < DIR_HANDLE_BASE + MAX_FS_HANDLES as i64 {
            let id = self.next_dir;
            self.next_dir += 1;
            return Ok(id);
        }
        Err(SysError::resource_exhausted(
            "fs: too many open dir handles (limit 1024)",
        ))
    }

    // Return a closed handle's ID to its free pool for reuse. Only called after
    // a successful map removal, so an ID is never double-recycled.
    fn recycle_file(&mut self, id: i64) {
        self.free_file.push(Reverse(id));
    }

    fn recycle_dir(&mut self, id: i64) {
        self.free_dir.push(Reverse(id));
    }
}

// ── Public invoke ─────────────────────────────────────────────────────────────

pub fn invoke(state: &mut FsState, name: &str, args: SysArgs) -> SysResult {
    tracing::debug!(name, "fs invoke");
    match name {
        "exists" => fs_exists(args),
        "read_text" => fs_read_text(args),
        "write_text" => fs_write_text(args),
        "append_text" => fs_append_text(args),
        "is_file" => fs_is_file(args),
        "is_dir" => fs_is_dir(args),
        "metadata" => fs_metadata(args),
        "canonicalize" => fs_canonicalize(args),
        "list_dir" => fs_list_dir(args),
        "create_dir" => fs_create_dir(args),
        "create_dir_all" => fs_create_dir_all(args),
        "remove_file" => fs_remove_file(args),
        "remove_dir" => fs_remove_dir(args),
        "remove_dir_all" => fs_remove_dir_all(args),
        "rename" => fs_rename(args),
        "copy_file" => fs_copy_file(args),
        "read_link" => fs_read_link(args),
        "hard_link" => fs_hard_link(args),
        "symlink" => fs_symlink(args),
        "set_readonly" => fs_set_readonly(args),
        "set_permissions" => fs_set_permissions(args),
        "read_bytes" => fs_read_bytes(args),
        "write_bytes" => fs_write_bytes(args),
        "append_bytes" => fs_append_bytes(args),
        "file_read_bytes" => fs_file_read_bytes(state, args),
        "file_write_bytes" => fs_file_write_bytes(state, args),
        "open_file" => fs_open_file(state, args),
        "close_file" => fs_close_file(state, args),
        "file_read_all_text" => fs_file_read_all_text(state, args),
        "file_read_line" => fs_file_read_line(state, args),
        "file_write" => fs_file_write(state, args),
        "file_flush" => fs_file_flush(state, args),
        "file_seek" => fs_file_seek(state, args),
        "file_metadata" => fs_file_metadata(state, args),
        "open_dir" => fs_open_dir(state, args),
        "close_dir" => fs_close_dir(state, args),
        "dir_list" => fs_dir_list(state, args),
        _ => Err(format!("fs: unknown export '{name}'").into()),
    }
}

fn fs_exists(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.exists")?;
    Ok(SysValue::Bool(Path::new(&path).exists()))
}

fn fs_read_text(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.read_text")?;
    let text = std::fs::read_to_string(&path).map_err(|e| SysError::from_io("fs.read_text", e))?;
    Ok(SysValue::Str(text))
}

fn fs_write_text(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.write_text")?;
    let text = args.require_str(1, "fs.write_text")?;
    std::fs::write(&path, text.as_bytes()).map_err(|e| SysError::from_io("fs.write_text", e))?;
    Ok(SysValue::Null)
}

fn fs_append_text(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.append_text")?;
    let text = args.require_str(1, "fs.append_text")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| SysError::from_io("fs.append_text", e))?;
    file.write_all(text.as_bytes())
        .map_err(|e| SysError::from_io("fs.append_text", e))?;
    Ok(SysValue::Null)
}

fn fs_is_file(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.is_file")?;
    Ok(SysValue::Bool(Path::new(&path).is_file()))
}

fn fs_is_dir(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.is_dir")?;
    Ok(SysValue::Bool(Path::new(&path).is_dir()))
}

fn fs_metadata(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.metadata")?;
    metadata_value(Path::new(&path), "fs.metadata")
}

fn fs_canonicalize(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.canonicalize")?;
    let canon =
        std::fs::canonicalize(&path).map_err(|e| SysError::from_io("fs.canonicalize", e))?;
    Ok(SysValue::Str(
        canon
            .to_str()
            .ok_or("fs.canonicalize: non-UTF-8 path")?
            .to_string(),
    ))
}

fn fs_list_dir(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.list_dir")?;
    list_dir_value(Path::new(&path))
}

fn fs_create_dir(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.create_dir")?;
    std::fs::create_dir(&path).map_err(|e| SysError::from_io("fs.create_dir", e))?;
    Ok(SysValue::Null)
}

fn fs_create_dir_all(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.create_dir_all")?;
    std::fs::create_dir_all(&path).map_err(|e| SysError::from_io("fs.create_dir_all", e))?;
    Ok(SysValue::Null)
}

fn fs_remove_file(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.remove_file")?;
    std::fs::remove_file(&path).map_err(|e| SysError::from_io("fs.remove_file", e))?;
    Ok(SysValue::Null)
}

fn fs_remove_dir(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.remove_dir")?;
    std::fs::remove_dir(&path).map_err(|e| SysError::from_io("fs.remove_dir", e))?;
    Ok(SysValue::Null)
}

fn fs_remove_dir_all(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.remove_dir_all")?;
    std::fs::remove_dir_all(&path).map_err(|e| SysError::from_io("fs.remove_dir_all", e))?;
    Ok(SysValue::Null)
}

fn fs_rename(args: SysArgs) -> SysResult {
    let src = args.require_str(0, "fs.rename")?;
    let dst = args.require_str(1, "fs.rename")?;
    std::fs::rename(&src, &dst).map_err(|e| SysError::from_io("fs.rename", e))?;
    Ok(SysValue::Null)
}

fn fs_copy_file(args: SysArgs) -> SysResult {
    let src = args.require_str(0, "fs.copy_file")?;
    let dst = args.require_str(1, "fs.copy_file")?;
    std::fs::copy(&src, &dst).map_err(|e| SysError::from_io("fs.copy_file", e))?;
    Ok(SysValue::Null)
}

fn fs_read_link(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.read_link")?;
    let target = std::fs::read_link(&path).map_err(|e| SysError::from_io("fs.read_link", e))?;
    Ok(SysValue::Str(
        target
            .to_str()
            .ok_or("fs.read_link: non-UTF-8 path")?
            .to_string(),
    ))
}

fn fs_hard_link(args: SysArgs) -> SysResult {
    let src = args.require_str(0, "fs.hard_link")?;
    let dst = args.require_str(1, "fs.hard_link")?;
    std::fs::hard_link(&src, &dst).map_err(|e| SysError::from_io("fs.hard_link", e))?;
    Ok(SysValue::Null)
}

/// Create a symbolic link at `link` pointing to `target`. `target` is stored
/// verbatim in the link (it may be relative and need not exist), so only `link`
/// is a filesystem write — see the sandbox handling in `sys_policy::authorize_fs`.
#[cfg(unix)]
fn fs_symlink(args: SysArgs) -> SysResult {
    let target = args.require_str(0, "fs.symlink")?;
    let link = args.require_str(1, "fs.symlink")?;
    std::os::unix::fs::symlink(&target, &link).map_err(|e| SysError::from_io("fs.symlink", e))?;
    Ok(SysValue::Null)
}

#[cfg(not(unix))]
fn fs_symlink(_args: SysArgs) -> SysResult {
    Err(SysError::unsupported(
        "fs.symlink: not supported on this platform",
    ))
}

/// Set the Unix permission bits (mode) of `path`, e.g. `0o644`. Unlike
/// `set_readonly`, this replaces the full mode rather than toggling one bit.
#[cfg(unix)]
fn fs_set_permissions(args: SysArgs) -> SysResult {
    use std::os::unix::fs::PermissionsExt;
    let path = args.require_str(0, "fs.set_permissions")?;
    let mode = args.require_int(1, "fs.set_permissions")?;
    let mode = u32::try_from(mode).map_err(|_| {
        SysError::invalid_argument("fs.set_permissions: mode must be a non-negative int within u32")
    })?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| SysError::from_io("fs.set_permissions", e))?;
    Ok(SysValue::Null)
}

#[cfg(not(unix))]
fn fs_set_permissions(_args: SysArgs) -> SysResult {
    Err(SysError::unsupported(
        "fs.set_permissions: not supported on this platform",
    ))
}

fn fs_read_bytes(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.read_bytes")?;
    let bytes = std::fs::read(&path).map_err(|e| SysError::from_io("fs.read_bytes", e))?;
    Ok(SysValue::Bytes(bytes))
}

fn fs_write_bytes(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.write_bytes")?;
    let bytes = args.require_bytes(1, "fs.write_bytes")?;
    std::fs::write(&path, &bytes).map_err(|e| SysError::from_io("fs.write_bytes", e))?;
    Ok(SysValue::Null)
}

fn fs_append_bytes(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.append_bytes")?;
    let bytes = args.require_bytes(1, "fs.append_bytes")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| SysError::from_io("fs.append_bytes", e))?;
    file.write_all(&bytes)
        .map_err(|e| SysError::from_io("fs.append_bytes", e))?;
    Ok(SysValue::Null)
}

fn fs_file_read_bytes(state: &mut FsState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "fs.file_read_bytes")?;
    let max_bytes = args.require_int(1, "fs.file_read_bytes")?;
    let max_bytes = usize::try_from(max_bytes).map_err(|_| {
        SysError::invalid_argument("fs.file_read_bytes: max_bytes must be a non-negative int")
    })?;
    let fh = state
        .file_handles
        .get_mut(&handle)
        .ok_or_else(|| format!("fs.file_read_bytes: unknown handle {handle}"))?;
    // Grow the buffer to the actual data size via `take`, so a caller-supplied
    // `max_bytes` near `i64::MAX` never triggers a giant up-front allocation.
    let mut buf = Vec::new();
    (&fh.file)
        .take(max_bytes as u64)
        .read_to_end(&mut buf)
        .map_err(|e| SysError::from_io("fs.file_read_bytes", e))?;
    Ok(SysValue::Bytes(buf))
}

fn fs_file_write_bytes(state: &mut FsState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "fs.file_write_bytes")?;
    let bytes = args.require_bytes(1, "fs.file_write_bytes")?;
    let fh = state
        .file_handles
        .get_mut(&handle)
        .ok_or_else(|| format!("fs.file_write_bytes: unknown handle {handle}"))?;
    fh.file
        .write_all(&bytes)
        .map_err(|e| SysError::from_io("fs.file_write_bytes", e))?;
    Ok(SysValue::Null)
}

fn fs_set_readonly(args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.set_readonly")?;
    let readonly = match args.optional(1) {
        Some(SysValue::Bool(value)) => *value,
        Some(_) => {
            return Err(SysError::invalid_argument(
                "fs.set_readonly: arg 1 must be bool",
            ))
        }
        None => return Err(SysError::invalid_argument("fs.set_readonly: missing arg 1")),
    };
    let mut perms = std::fs::metadata(&path)
        .map_err(|e| SysError::from_io("fs.set_readonly", e))?
        .permissions();
    perms.set_readonly(readonly);
    std::fs::set_permissions(&path, perms).map_err(|e| SysError::from_io("fs.set_readonly", e))?;
    Ok(SysValue::Null)
}

fn fs_open_file(state: &mut FsState, args: SysArgs) -> SysResult {
    let spec = args.require_map(0, "fs.open_file")?;
    let path = spec.require_str("path", "fs.open_file spec.path")?;
    let flags = normalize_fs_open_file_flags(FsOpenFileFlags {
        read: optional_bool(&spec, "read", "fs.open_file")?,
        write: optional_bool(&spec, "write", "fs.open_file")?,
        append: optional_bool(&spec, "append", "fs.open_file")?,
        create: optional_bool(&spec, "create", "fs.open_file")?,
        create_new: optional_bool(&spec, "create_new", "fs.open_file")?,
        truncate: optional_bool(&spec, "truncate", "fs.open_file")?,
    })?;

    // Guard capacity BEFORE opening the OS file descriptor so a full handle
    // table never leaks an fd that cannot be returned to the caller.
    // alloc_file() does the real check; this is an O(1) fast path.
    if state.file_handles.len() >= MAX_FS_HANDLES {
        return Err(SysError::resource_exhausted(
            "fs: too many open file handles (limit 1024)",
        ));
    }

    let pb = PathBuf::from(&path);
    let file = fs_open_options(flags)
        .open(&path)
        .map_err(|e| SysError::from_io("fs.open_file", e))?;
    let h = state.alloc_file()?;
    state
        .file_handles
        .insert(h, FsFileHandle { path: pb, file });
    Ok(SysValue::Int(h))
}

fn fs_close_file(state: &mut FsState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "fs.close_file")?;
    state
        .file_handles
        .remove(&handle)
        .ok_or_else(|| format!("fs.close_file: unknown handle {handle}"))?;
    state.recycle_file(handle);
    Ok(SysValue::Null)
}

fn fs_file_read_all_text(state: &mut FsState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "fs.file_read_all_text")?;
    let fh = state
        .file_handles
        .get_mut(&handle)
        .ok_or_else(|| format!("fs.file_read_all_text: unknown handle {handle}"))?;
    let mut text = String::new();
    fh.file
        .read_to_string(&mut text)
        .map_err(|e| SysError::from_io("fs.file_read_all_text", e))?;
    Ok(SysValue::Str(text))
}

fn fs_file_read_line(state: &mut FsState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "fs.file_read_line")?;
    let fh = state
        .file_handles
        .get_mut(&handle)
        .ok_or_else(|| format!("fs.file_read_line: unknown handle {handle}"))?;
    // Read in 512-byte chunks instead of one byte at a time.
    // When a newline is found mid-chunk, seek back over the unconsumed bytes
    // so the next read picks up immediately after the newline.
    let mut result: Vec<u8> = Vec::new();
    let mut buf = [0_u8; 512];
    loop {
        let n = fh
            .file
            .read(&mut buf)
            .map_err(|e| SysError::from_io("fs.file_read_line", e))?;
        if n == 0 {
            break;
        }
        if let Some(nl) = buf[..n].iter().position(|&b| b == b'\n') {
            result.extend_from_slice(&buf[..=nl]);
            let leftover = (n - nl - 1) as i64;
            if leftover > 0 {
                fh.file
                    .seek(SeekFrom::Current(-leftover))
                    .map_err(|e| SysError::from_io("fs.file_read_line", e))?;
            }
            break;
        }
        result.extend_from_slice(&buf[..n]);
    }
    if result.is_empty() {
        return Ok(SysValue::Null);
    }
    let line = String::from_utf8(result)
        .map_err(|e| SysError::invalid_argument(format!("fs.file_read_line: {e}")))?;
    Ok(SysValue::Str(line))
}

fn fs_file_write(state: &mut FsState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "fs.file_write")?;
    let text = args.require_str(1, "fs.file_write")?;
    let fh = state
        .file_handles
        .get_mut(&handle)
        .ok_or_else(|| format!("fs.file_write: unknown handle {handle}"))?;
    fh.file
        .write_all(text.as_bytes())
        .map_err(|e| SysError::from_io("fs.file_write", e))?;
    Ok(SysValue::Null)
}

fn fs_file_flush(state: &mut FsState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "fs.file_flush")?;
    let fh = state
        .file_handles
        .get_mut(&handle)
        .ok_or_else(|| format!("fs.file_flush: unknown handle {handle}"))?;
    fh.file
        .flush()
        .map_err(|e| SysError::from_io("fs.file_flush", e))?;
    Ok(SysValue::Null)
}

fn fs_file_seek(state: &mut FsState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "fs.file_seek")?;
    let offset = args.require_int(1, "fs.file_seek")?;
    let whence = match args.optional(2) {
        None | Some(SysValue::Null) => "start",
        Some(SysValue::Str(s)) => s.as_str(),
        Some(_) => return Err("fs.file_seek: whence must be a string".into()),
    };
    let seek_from = match whence {
        "start" if offset >= 0 => SeekFrom::Start(offset as u64),
        "current" => SeekFrom::Current(offset),
        "end" => SeekFrom::End(offset),
        "start" => return Err("fs.file_seek: start offset must be non-negative".into()),
        _ => return Err("fs.file_seek: whence must be start, current, or end".into()),
    };
    let fh = state
        .file_handles
        .get_mut(&handle)
        .ok_or_else(|| format!("fs.file_seek: unknown handle {handle}"))?;
    let pos = fh
        .file
        .seek(seek_from)
        .map_err(|e| SysError::from_io("fs.file_seek", e))?;
    u64_to_sys_int(pos, "fs.file_seek")
}

fn fs_file_metadata(state: &mut FsState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "fs.file_metadata")?;
    // Use File::metadata() on the open fd rather than re-stating the cached
    // path.  This eliminates the TOCTOU window between releasing the handle
    // lock and calling std::fs::symlink_metadata() on the stored path string.
    let fh = state
        .file_handles
        .get(&handle)
        .ok_or_else(|| format!("fs.file_metadata: unknown handle {handle}"))?;
    let meta = fh
        .file
        .metadata()
        .map_err(|e| SysError::from_io("fs.file_metadata", e))?;
    let path_str = fh
        .path
        .to_str()
        .ok_or("fs.file_metadata: non-UTF-8 path")?
        .to_string();
    metadata_value_from_parts(&path_str, &meta, "fs.file_metadata")
}

fn fs_open_dir(state: &mut FsState, args: SysArgs) -> SysResult {
    let path = args.require_str(0, "fs.open_dir")?;
    let pb = PathBuf::from(&path);
    let h = state.alloc_dir()?;
    state.dir_handles.insert(h, pb);
    Ok(SysValue::Int(h))
}

fn fs_close_dir(state: &mut FsState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "fs.close_dir")?;
    state
        .dir_handles
        .remove(&handle)
        .ok_or_else(|| format!("fs.close_dir: unknown handle {handle}"))?;
    state.recycle_dir(handle);
    Ok(SysValue::Null)
}

fn fs_dir_list(state: &mut FsState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "fs.dir_list")?;
    let path = state
        .dir_handles
        .get(&handle)
        .cloned()
        .ok_or_else(|| format!("fs.dir_list: unknown handle {handle}"))?;
    list_dir_value(&path)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn metadata_value(path: &Path, ctx: &str) -> SysResult {
    let meta = std::fs::symlink_metadata(path).map_err(|e| SysError::from_io(ctx, e))?;
    let path_str = path
        .to_str()
        .ok_or_else(|| format!("{ctx}: non-UTF-8 path"))?
        .to_string();
    metadata_value_from_parts(&path_str, &meta, ctx)
}

fn metadata_value_from_parts(path_str: &str, meta: &std::fs::Metadata, ctx: &str) -> SysResult {
    let is_file = meta.is_file();
    let is_dir = meta.is_dir();
    let is_symlink = meta.file_type().is_symlink();
    let mut m = HashMap::new();
    m.insert("path".into(), SysValue::Str(path_str.to_string()));
    m.insert(
        "kind".into(),
        SysValue::Str(fs_entry_kind(is_file, is_dir, is_symlink).into()),
    );
    m.insert("is_file".into(), SysValue::Bool(is_file));
    m.insert("is_dir".into(), SysValue::Bool(is_dir));
    m.insert("is_symlink".into(), SysValue::Bool(is_symlink));
    m.insert("exists".into(), SysValue::Bool(true));
    m.insert("size".into(), u64_to_sys_int(meta.len(), ctx)?);
    m.insert(
        "readonly".into(),
        SysValue::Bool(meta.permissions().readonly()),
    );
    m.insert(
        "modified_unix_ns".into(),
        system_time_unix_ns(meta.modified().ok()),
    );
    m.insert(
        "accessed_unix_ns".into(),
        system_time_unix_ns(meta.accessed().ok()),
    );
    m.insert(
        "created_unix_ns".into(),
        system_time_unix_ns(meta.created().ok()),
    );
    Ok(SysValue::Map(m))
}

fn list_dir_value(path: &Path) -> SysResult {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|e| SysError::from_io("fs.list_dir", e))? {
        let entry = entry.map_err(|e| SysError::from_io("fs.list_dir", e))?;
        let entry_path = entry.path();
        let meta = std::fs::symlink_metadata(&entry_path)
            .map_err(|e| SysError::from_io("fs.list_dir", e))?;
        let name = path_component_to_string(&entry.file_name(), "fs.list_dir")?;
        let path_str = entry_path
            .to_str()
            .ok_or("fs.list_dir: non-UTF-8 path")?
            .to_string();
        let mut m = HashMap::new();
        m.insert("name".into(), SysValue::Str(name.clone()));
        m.insert("path".into(), SysValue::Str(path_str));
        m.insert(
            "kind".into(),
            SysValue::Str(
                fs_entry_kind(meta.is_file(), meta.is_dir(), meta.file_type().is_symlink()).into(),
            ),
        );
        m.insert("is_file".into(), SysValue::Bool(meta.is_file()));
        m.insert("is_dir".into(), SysValue::Bool(meta.is_dir()));
        m.insert(
            "is_symlink".into(),
            SysValue::Bool(meta.file_type().is_symlink()),
        );
        entries.push((name, SysValue::Map(m)));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(SysValue::List(
        entries.into_iter().map(|(_, v)| v).collect(),
    ))
}

fn system_time_unix_ns(time: Option<SystemTime>) -> SysValue {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .map(SysValue::Int)
        .unwrap_or(SysValue::Null)
}

fn fs_entry_kind(is_file: bool, is_dir: bool, is_symlink: bool) -> &'static str {
    if is_symlink {
        "symlink"
    } else if is_dir {
        "dir"
    } else if is_file {
        "file"
    } else {
        "other"
    }
}

fn optional_bool(spec: &crate::ffi_value::SysMap, key: &str, ctx: &str) -> Result<bool, SysError> {
    match spec.0.get(key) {
        Some(SysValue::Bool(value)) => Ok(*value),
        Some(SysValue::Null) | None => Ok(false),
        Some(_) => Err(SysError::invalid_argument(format!(
            "{ctx} spec.{key}: field must be bool"
        ))),
    }
}

fn normalize_fs_open_file_flags(mut flags: FsOpenFileFlags) -> Result<FsOpenFileFlags, SysError> {
    if !flags.read && !flags.write && !flags.append {
        flags.read = true;
    }
    if flags.append && flags.truncate {
        return Err("fs.open_file: append and truncate cannot both be true".into());
    }
    if flags.create && flags.create_new {
        return Err("fs.open_file: create and create_new cannot both be true".into());
    }
    if flags.truncate && !flags.write {
        return Err("fs.open_file: truncate requires write".into());
    }
    if (flags.create || flags.create_new) && !(flags.write || flags.append) {
        return Err("fs.open_file: create requires write or append".into());
    }
    Ok(flags)
}

fn fs_open_options(flags: FsOpenFileFlags) -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    options
        .read(flags.read)
        .write(flags.write)
        .append(flags.append)
        .create(flags.create)
        .create_new(flags.create_new)
        .truncate(flags.truncate);
    options
}

fn u64_to_sys_int(value: u64, ctx: &str) -> SysResult {
    i64::try_from(value)
        .map(SysValue::Int)
        .map_err(|_| SysError::invalid_argument(format!("{ctx}: value exceeds CAAP SYS int range")))
}

fn path_component_to_string(component: &OsStr, ctx: &str) -> Result<String, SysError> {
    component.to_str().map(str::to_string).ok_or_else(|| {
        SysError::invalid_argument(format!("{ctx}: path component is not valid UTF-8"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn metadata_time_projection_is_null_for_unrepresentable_times() {
        assert_eq!(system_time_unix_ns(None), SysValue::Null);
        assert_eq!(
            system_time_unix_ns(Some(UNIX_EPOCH - Duration::from_nanos(1))),
            SysValue::Null
        );
        assert_eq!(
            system_time_unix_ns(Some(
                UNIX_EPOCH + Duration::from_nanos((i64::MAX as u64) + 1)
            )),
            SysValue::Null
        );
    }

    #[test]
    fn metadata_projection_matches_host_fs_surface() {
        let path = std::env::temp_dir().join(format!("caap-sys-metadata-{}", std::process::id()));
        std::fs::write(&path, b"hello").unwrap();

        let value = metadata_value(&path, "fs.metadata").unwrap();

        std::fs::remove_file(&path).unwrap();
        let SysValue::Map(metadata) = value else {
            panic!("expected metadata map");
        };
        assert_eq!(metadata.get("kind"), Some(&SysValue::Str("file".into())));
        assert_eq!(metadata.get("is_file"), Some(&SysValue::Bool(true)));
        assert_eq!(metadata.get("is_dir"), Some(&SysValue::Bool(false)));
        assert_eq!(metadata.get("is_symlink"), Some(&SysValue::Bool(false)));
        assert_eq!(metadata.get("size"), Some(&SysValue::Int(5)));
    }

    #[cfg(unix)]
    #[test]
    fn metadata_projection_preserves_symlink_identity() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("caap-sys-metadata-link-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("target.txt");
        let link = root.join("link.txt");
        std::fs::write(&target, b"hello").unwrap();
        symlink(&target, &link).unwrap();

        let value = metadata_value(&link, "fs.metadata").unwrap();

        std::fs::remove_dir_all(&root).unwrap();
        let SysValue::Map(metadata) = value else {
            panic!("expected metadata map");
        };
        assert_eq!(metadata.get("kind"), Some(&SysValue::Str("symlink".into())));
        assert_eq!(metadata.get("is_symlink"), Some(&SysValue::Bool(true)));
    }

    #[test]
    fn open_file_rejects_malformed_bool_flags() {
        let mut state = FsState::new();
        let error = invoke(
            &mut state,
            "open_file",
            SysArgs(vec![SysValue::Map(HashMap::from([
                ("path".into(), SysValue::Str("demo.txt".into())),
                ("read".into(), SysValue::Str("yes".into())),
            ]))]),
        )
        .unwrap_err();
        assert!(error.contains("fs.open_file spec.read: field must be bool"));
    }

    #[test]
    fn open_file_flags_are_explicit() {
        let invalid = normalize_fs_open_file_flags(FsOpenFileFlags {
            create: true,
            ..Default::default()
        })
        .unwrap_err();
        assert!(invalid.contains("fs.open_file: create requires write or append"));

        let path = std::env::temp_dir().join(format!(
            "caap-sys-open-flags-{}-{}.txt",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let flags = normalize_fs_open_file_flags(FsOpenFileFlags {
            write: true,
            create: true,
            truncate: true,
            ..Default::default()
        })
        .unwrap();
        let mut file = fs_open_options(flags).open(&path).unwrap();
        file.write_all(b"payload").unwrap();
        let mut text = String::new();
        let error = file.read_to_string(&mut text).unwrap_err();
        std::fs::remove_file(path).ok();
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn write_bytes_read_bytes_round_trip_preserves_non_utf8() {
        let path = std::env::temp_dir().join(format!(
            "caap-sys-bytes-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path_str = path.to_str().unwrap().to_string();
        let mut state = FsState::new();
        let payload = vec![0u8, 159, 146, 150, 255]; // invalid UTF-8 on purpose

        invoke(
            &mut state,
            "write_bytes",
            SysArgs(vec![
                SysValue::Str(path_str.clone()),
                SysValue::Bytes(payload.clone()),
            ]),
        )
        .unwrap();
        let read = invoke(
            &mut state,
            "read_bytes",
            SysArgs(vec![SysValue::Str(path_str)]),
        )
        .unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(read, SysValue::Bytes(payload));
    }

    #[test]
    fn file_read_bytes_huge_max_does_not_over_allocate() {
        // A caller-supplied max_bytes near i64::MAX must NOT trigger a giant
        // up-front allocation; the read returns only the bytes available.
        let path = std::env::temp_dir().join(format!(
            "caap-sys-read-cap-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let payload = b"small payload";
        std::fs::write(&path, payload).unwrap();
        let path_str = path.to_str().unwrap().to_string();
        let mut state = FsState::new();

        let handle = invoke(
            &mut state,
            "open_file",
            SysArgs(vec![SysValue::Map(HashMap::from([
                ("path".into(), SysValue::Str(path_str)),
                ("read".into(), SysValue::Bool(true)),
            ]))]),
        )
        .unwrap();
        let SysValue::Int(handle) = handle else {
            panic!("expected handle int");
        };

        let read = invoke(
            &mut state,
            "file_read_bytes",
            SysArgs(vec![SysValue::Int(handle), SysValue::Int(i64::MAX)]),
        )
        .unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(read, SysValue::Bytes(payload.to_vec()));
    }

    #[test]
    fn hard_link_and_set_readonly_round_trip() {
        let root = std::env::temp_dir().join(format!(
            "caap-sys-fs-link-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("a.txt");
        let link = root.join("b.txt");
        std::fs::write(&src, b"data").unwrap();
        let mut state = FsState::new();

        invoke(
            &mut state,
            "hard_link",
            SysArgs(vec![
                SysValue::Str(src.to_str().unwrap().to_string()),
                SysValue::Str(link.to_str().unwrap().to_string()),
            ]),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "data");

        invoke(
            &mut state,
            "set_readonly",
            SysArgs(vec![
                SysValue::Str(src.to_str().unwrap().to_string()),
                SysValue::Bool(true),
            ]),
        )
        .unwrap();
        assert!(std::fs::metadata(&src).unwrap().permissions().readonly());
        // Clear readonly so cleanup can remove it.
        invoke(
            &mut state,
            "set_readonly",
            SysArgs(vec![
                SysValue::Str(src.to_str().unwrap().to_string()),
                SysValue::Bool(false),
            ]),
        )
        .unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn read_link_returns_symlink_target() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!(
            "caap-sys-fs-readlink-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("target.txt");
        let link = root.join("link.txt");
        std::fs::write(&target, b"x").unwrap();
        symlink(&target, &link).unwrap();

        let mut state = FsState::new();
        let value = invoke(
            &mut state,
            "read_link",
            SysArgs(vec![SysValue::Str(link.to_str().unwrap().to_string())]),
        )
        .unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(value, SysValue::Str(target.to_str().unwrap().to_string()));
    }

    #[test]
    fn u64_to_sys_int_rejects_values_outside_sys_int_range() {
        assert_eq!(u64_to_sys_int(42, "fs.test").unwrap(), SysValue::Int(42));
        let error = u64_to_sys_int((i64::MAX as u64) + 1, "fs.test").unwrap_err();
        assert!(error.contains("exceeds CAAP SYS int range"));
    }

    #[cfg(unix)]
    #[test]
    fn list_dir_rejects_non_utf8_entry_names() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let root = std::env::temp_dir().join(format!(
            "caap-sys-fs-non-utf8-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let invalid_name = PathBuf::from(OsString::from_vec(b"bad-\xFF".to_vec()));
        std::fs::write(root.join(invalid_name), b"x").unwrap();

        let mut state = FsState::new();
        let error = invoke(
            &mut state,
            "list_dir",
            SysArgs(vec![SysValue::Str(root.to_str().unwrap().to_string())]),
        )
        .unwrap_err();

        std::fs::remove_dir_all(root).unwrap();
        assert!(error.contains("path component is not valid UTF-8"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_creates_a_link_resolving_to_its_target() {
        let mut state = FsState::new();
        let root = std::env::temp_dir().join(format!("caap-sys-symlink-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("target.txt");
        let link = root.join("link.txt");
        std::fs::write(&target, b"payload").unwrap();

        invoke(
            &mut state,
            "symlink",
            SysArgs(vec![
                SysValue::Str(target.to_str().unwrap().to_string()),
                SysValue::Str(link.to_str().unwrap().to_string()),
            ]),
        )
        .unwrap();

        // The link reads through to the target, and metadata sees it as a symlink.
        assert_eq!(std::fs::read(&link).unwrap(), b"payload");
        let SysValue::Map(meta) = metadata_value(&link, "fs.metadata").unwrap() else {
            panic!("expected metadata map");
        };
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(meta.get("is_symlink"), Some(&SysValue::Bool(true)));
    }

    #[cfg(unix)]
    #[test]
    fn set_permissions_replaces_the_unix_mode() {
        use std::os::unix::fs::PermissionsExt;
        let mut state = FsState::new();
        let path = std::env::temp_dir().join(format!("caap-sys-chmod-{}", std::process::id()));
        std::fs::write(&path, b"x").unwrap();

        invoke(
            &mut state,
            "set_permissions",
            SysArgs(vec![
                SysValue::Str(path.to_str().unwrap().to_string()),
                SysValue::Int(0o600),
            ]),
        )
        .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        std::fs::remove_file(&path).unwrap();
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn set_permissions_rejects_out_of_range_mode() {
        let mut state = FsState::new();
        let error = invoke(
            &mut state,
            "set_permissions",
            SysArgs(vec![SysValue::Str("/tmp/x".into()), SysValue::Int(-1)]),
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            crate::ffi_value::SysErrorKind::InvalidArgument
        );
    }
}
