//! SCP wire-protocol engine.
//!
//! When an SFTP-only attempt fails (the `sftp` subsystem is missing/refused,
//! common on ultra-minimal BusyBox routers and network gear) we fall back to
//! the classic `scp` protocol: an SSH exec channel running the remote `scp`
//! binary in `-f` (sender) or `-t` (receiver) mode, speaking the rcp/SCP
//! framing protocol over the channel's stdin/stdout.
//!
//! This module implements BOTH directions over a single authenticated russh
//! handle: directory listings (a throwaway `ls -l`), file download, file
//! upload, recursive directory download/upload, read/write of small text
//! files, plus the shell commands used for file-system operations that the
//! SCP protocol has no primitive for (delete / mkdir / rename / chmod /
//! touch). Everything is shell-quoted because remote names are untrusted.
//!
//! # Wire protocol (from OpenSSH scp.c)
//!
//! Both ends open with a single `\0` greeting byte (BusyBox may omit it, so we
//! probe with a short timeout on the receive side and always send it on the
//! send side). Every command line / control record / payload chunk is then
//! acknowledged by a single `\0` byte (or `\01`/`\02` followed by a message for
//! an error) on the reverse direction.
//!
//! The sender (`scp -f`) writes:
//!
//! ```text
//! C<mode:4o> <size> <basename>\n     file start
//! <size bytes of data>                payload
//! \0                                  end-of-file marker (must be ACKed)
//! D<mode:4o> 0 <basename>\n          directory start
//! ... nested C / D records ...
//! E\n                                 directory end
//! ```

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use russh::client::Msg;
use russh::{Channel, ChannelMsg};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

use crate::i18n::t;
use crate::ssh::{RemoteEntry, SessionEvent};
use crate::sftp::sftp::{base_name, local_file_name_utf8, sanitize_filename};
use crate::sftp::emit_transfer;

pub(crate) type SftpHandle = russh::client::Handle<crate::sftp::SftpClientHandler>;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Single-quote a string for safe interpolation into a remote `/bin/sh`
/// command. Remote names come from the *server's* listing and are therefore
/// untrusted.
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Parse `ls -l` output into [`RemoteEntry`]s.
///
/// The `-l` format is the most portable across GNU and BusyBox. Each entry is:
/// `<mode> <links> <owner> <group> <size> <month> <day> <time|year> <name>`,
/// so the name is everything after the first eight tokens. A directory path is
/// passed so full remote paths can be reconstructed exactly.
fn parse_ls(lines: &str, dir: &str) -> Vec<RemoteEntry> {
    let mut out = Vec::new();
    let base = dir.trim_end_matches('/');
    for line in lines.lines() {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("total ") {
            if rest.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 8 {
            continue;
        }
        let mode = fields[0];
        let size_s = fields[4];
        let name = fields[8..].join(" ");
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        // Symlink: `name -> target`. Keep the base name (deref is dangerous).
        let name = name.split(" -> ").next().unwrap_or(&name).to_string();
        let is_dir = mode.starts_with('d');
        let is_link = mode.starts_with('l');
        let size = if is_dir {
            0
        } else {
            size_s.parse::<u64>().unwrap_or(0)
        };
        let full_path = if base.is_empty() {
            format!("/{name}")
        } else {
            format!("{base}/{name}")
        };
        out.push(RemoteEntry {
            name,
            full_path,
            is_dir: is_dir || is_link,
            size,
            modified: 0,
            mode: 0,
        });
    }
    out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    out
}

// ---------------------------------------------------------------------------
// Channel I/O
// ---------------------------------------------------------------------------

/// Minimal buffered reader over an SSH exec channel, so the SCP protocol can
/// read byte/line/stream without holding the whole payload in memory.
struct ScpIo {
    ch: Channel<Msg>,
    pending: Vec<u8>,
    eof_seen: bool,
    exit_code: Option<u32>,
    /// When set, stderr (ExtendedData) is folded into the data stream. Used
    /// only by shell capture (`ls`), where the error text lives on stderr. For
    /// the SCP protocol it stays false so informational stderr can't corrupt
    /// the byte stream.
    include_stderr: bool,
}

impl ScpIo {
    fn new(ch: Channel<Msg>) -> Self {
        Self {
            ch,
            pending: Vec::new(),
            eof_seen: false,
            exit_code: None,
            include_stderr: false,
        }
    }

    fn capture(mut self) -> Self {
        self.include_stderr = true;
        self
    }

    async fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.ch.data(bytes).await.context("scp channel write")?;
        Ok(())
    }

    async fn eof(&mut self) -> Result<()> {
        self.ch.eof().await.context("scp channel eof")?;
        Ok(())
    }

    async fn close(&mut self) {
        let _ = self.ch.close().await;
    }

    async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if !self.pending.is_empty() {
                let n = buf.len().min(self.pending.len());
                buf[..n].copy_from_slice(&self.pending[..n]);
                self.pending.drain(..n);
                return Ok(n);
            }
            if self.eof_seen {
                return Ok(0);
            }
            match self.ch.wait().await {
                None => {
                    self.eof_seen = true;
                    return Ok(0);
                }
                Some(ChannelMsg::Data { data }) => {
                    if data.is_empty() {
                        continue;
                    }
                    let n = buf.len().min(data.len());
                    buf[..n].copy_from_slice(&data[..n]);
                    if n < data.len() {
                        self.pending.extend_from_slice(&data[n..]);
                    }
                    return Ok(n);
                }
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    if self.include_stderr && !data.is_empty() {
                        let n = buf.len().min(data.len());
                        buf[..n].copy_from_slice(&data[..n]);
                        if n < data.len() {
                            self.pending.extend_from_slice(&data[n..]);
                        }
                        return Ok(n);
                    }
                    continue;
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    self.exit_code = Some(exit_status);
                    continue;
                }
                Some(ChannelMsg::Close) => {
                    self.eof_seen = true;
                    return Ok(0);
                }
                Some(_) => continue,
            }
        }
    }

    /// Read exactly `n` bytes; error on EOF mid-read.
    async fn read_exact(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(n);
        let mut buf = [0u8; 64 * 1024];
        while out.len() < n {
            let want = (n - out.len()).min(buf.len());
            let got = self.read(&mut buf[..want]).await?;
            if got == 0 {
                return Err(anyhow!(
                    "SCP: file ended early ({} of {} bytes)",
                    out.len(),
                    n
                ));
            }
            out.extend_from_slice(&buf[..got]);
        }
        Ok(out)
    }

    /// Read a single ACK byte; returns Ok(()) on `\0`, or Err carrying the
    /// remote's message on `\01`/`\02`.
    async fn read_ack(&mut self, max_msg: usize) -> Result<()> {
        let mut resp = [0u8; 1];
        match self.read(&mut resp).await {
            Ok(0) => Err(anyhow!("SCP: peer closed while awaiting ACK")),
            Ok(_) => match resp[0] {
                0 => Ok(()),
                1 | 2 => {
                    let mut msg = Vec::with_capacity(128);
                    loop {
                        if msg.len() >= max_msg {
                            break;
                        }
                        let mut b = [0u8; 1];
                        match self.read(&mut b).await {
                            Ok(0) => break,
                            Ok(_) => {
                                if b[0] == b'\n' {
                                    break;
                                }
                                msg.push(b[0]);
                            }
                            Err(e) => return Err(anyhow!("scp error ack read: {e}")),
                        }
                    }
                    let msg = String::from_utf8_lossy(&msg).trim().to_string();
                    let msg = if msg.is_empty() {
                        t("远端 SCP 错误", "remote scp error").to_string()
                    } else {
                        msg
                    };
                    Err(anyhow!("{msg}"))
                }
                other => Err(anyhow!("SCP: unexpected response byte 0x{other:02x}")),
            },
            Err(e) => Err(anyhow!("scp ack read: {e}")),
        }
    }

    /// Read one SCP control line (terminated by `\n`).
    async fn read_ctl_line(&mut self, max: usize) -> Result<String> {
        let mut buf = Vec::with_capacity(64);
        loop {
            if buf.len() >= max {
                return Err(anyhow!("SCP control line too long"));
            }
            let mut b = [0u8; 1];
            match self.read(&mut b).await {
                Ok(0) => return Err(anyhow!("SCP: unexpected EOF in control stream")),
                Ok(_) => {
                    if b[0] == b'\n' {
                        break;
                    }
                    buf.push(b[0]);
                }
                Err(e) => return Err(anyhow!("scp control read: {e}")),
            }
        }
        String::from_utf8(buf).context("non-UTF-8 SCP control line")
    }
}

/// Open an exec channel and run a remote command, returning its buffered I/O.
async fn exec_scp(handle: &SftpHandle, cmd: &str) -> Result<ScpIo> {
    let ch = handle
        .channel_open_session()
        .await
        .context("open exec channel")?;
    ch.exec(true, cmd.as_bytes())
        .await
        .with_context(|| format!("exec remote command: {cmd}"))?;
    Ok(ScpIo::new(ch))
}

// ---------------------------------------------------------------------------
// One-shot shell commands
// ---------------------------------------------------------------------------

/// Run a remote shell command and capture stdout plus the exit code.
pub(crate) async fn run_shell_capture(handle: &SftpHandle, cmd: &str) -> Result<(String, u32)> {
    let io = exec_scp(handle, cmd).await?;
    let mut io = io.capture();
    let mut stdout = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = io.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        stdout.extend_from_slice(&buf[..n]);
    }
    let code = io.exit_code.unwrap_or(0);
    Ok((String::from_utf8_lossy(&stdout).into_owned(), code))
}

/// Run a remote command and return `Ok(())` only on exit status 0.
pub(crate) async fn run_shell(handle: &SftpHandle, cmd: &str) -> Result<()> {
    let (out, code) = run_shell_capture(handle, cmd).await?;
    if code == 0 {
        Ok(())
    } else {
        let msg = out.trim();
        Err(anyhow!(if msg.is_empty() {
            format!("remote command failed (exit {code})")
        } else {
            format!("remote command failed (exit {code}): {msg}")
        }))
    }
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

/// List a remote directory over `ls -l` (portable to GNU and BusyBox).
pub(crate) async fn scp_list_dir(handle: &SftpHandle, path: &str) -> Result<Vec<RemoteEntry>> {
    let cmd = format!("ls -l {}", sh_quote(path));
    let (out, code) = run_shell_capture(handle, &cmd).await?;
    if code != 0 {
        let msg = out.trim();
        return Err(anyhow!(if msg.is_empty() {
            t("无法列出目录", "cannot list directory").to_string()
        } else {
            msg.to_string()
        }));
    }
    Ok(parse_ls(&out, path))
}

/// List only the subdirectories of `path` (used for the left tree).
pub(crate) async fn scp_list_dirs_only(
    handle: &SftpHandle,
    path: &str,
) -> Result<Vec<(String, String)>> {
    let entries = scp_list_dir(handle, path).await?;
    Ok(entries
        .into_iter()
        .filter(|e| e.is_dir)
        .map(|e| (e.name, e.full_path))
        .collect())
}

/// Test whether a remote path is a directory (`test -d`).
pub(crate) async fn scp_is_dir(handle: &SftpHandle, path: &str) -> Result<bool> {
    let (_, code) = run_shell_capture(handle, &format!("test -d {}", sh_quote(path))).await?;
    Ok(code == 0)
}

/// Test whether a remote path exists (`test -e`).
pub(crate) async fn scp_exists(handle: &SftpHandle, path: &str) -> Result<bool> {
    let (_, code) = run_shell_capture(handle, &format!("test -e {}", sh_quote(path))).await?;
    Ok(code == 0)
}

// ---------------------------------------------------------------------------
// Download / upload over the SCP sender / receiver
// ---------------------------------------------------------------------------

/// Download a remote file to `local` using `scp -f`. Emits transfer progress.
pub(crate) async fn scp_download(
    handle: &SftpHandle,
    remote: &str,
    local: &str,
    name: &str,
    id: &str,
    events: &tokio::sync::mpsc::UnboundedSender<SessionEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<bool> {
    let cmd = format!("scp -f -- {}", sh_quote(remote));
    let mut io = exec_scp(handle, &cmd).await?;

    // The `-f` sender waits for our greeting before sending anything.
    io.write_all(&[0u8]).await?;

    // First record must be `C<mode> <size> <name>\n`. If the remote scp hit an
    // error (e.g. permission denied) it sends `\x01scp: ...\n` instead.
    let line = io.read_ctl_line(4096).await?;
    if line.starts_with('\u{1}') || line.starts_with('\u{2}') {
        let msg = line.chars().skip(1).collect::<String>().trim().to_string();
        return Err(anyhow!(if msg.is_empty() {
            t("远端 SCP 错误", "remote scp error").to_string()
        } else {
            msg
        }));
    }
    if !line.starts_with('C') {
        return Err(anyhow!(
            "{}",
            t("远端未运行 SCP(可能不是 SCP 服务器)", "remote is not an SCP server")
        ));
    }
    let size: u64 = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    io.write_all(&[0u8]).await?; // ACK the control record

    let mut local_file = tokio::fs::File::create(local)
        .await
        .with_context(|| format!("create local {local}"))?;
    emit_transfer(events, id, name, false, 0, size, 0, "");
    let mut done = 0u64;
    let mut last = Instant::now();
    let mut cancelled = false;
    let mut err: Option<anyhow::Error> = None;

    while done < size {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let want = ((size - done) as usize).min(64 * 1024);
        match io.read_exact(want).await {
            Ok(data) => {
                local_file
                    .write_all(&data)
                    .await
                    .context("write local file")?;
                done += data.len() as u64;
            }
            Err(e) => {
                err = Some(e);
                break;
            }
        }
        if last.elapsed() >= Duration::from_millis(150) {
            last = Instant::now();
            emit_transfer(events, id, name, false, done, size, 0, "");
        }
    }
    if err.is_none() && !cancelled && done == size {
        // Trailing `\0` end-of-file marker, then ACK the completed payload.
        let mut eof = [0u8; 1];
        match io.read(&mut eof).await {
            Ok(0) => {}
            Ok(_) if eof[0] == 0 => {}
            _ => err = Some(anyhow!("SCP: missing end-of-file marker")),
        }
        if err.is_none() {
            let _ = io.write_all(&[0u8]).await;
        }
    }

    let _ = io.eof().await;
    io.close().await;

    if let Some(e) = err {
        let _ = tokio::fs::remove_file(local).await;
        return Err(e);
    }
    if cancelled {
        let _ = tokio::fs::remove_file(local).await;
        emit_transfer(events, id, name, false, done, size, 4, t("已取消", "Cancelled"));
        return Ok(false);
    }
    emit_transfer(events, id, name, false, done, size, 1, "");
    Ok(true)
}

/// Recursively download a remote directory tree into `local_parent` via
/// `scp -r -f`. Mirrors the SFTP `download_dir` behaviour (root dir named after
/// the remote basename under `local_parent`).
pub(crate) async fn scp_download_dir(
    handle: &SftpHandle,
    remote_root: &str,
    local_parent: &str,
    events: &tokio::sync::mpsc::UnboundedSender<SessionEvent>,
) -> Result<()> {
    let cmd = format!("scp -r -f -- {}", sh_quote(remote_root));
    let mut io = exec_scp(handle, &cmd).await?;
    io.write_all(&[0u8]).await?; // greeting

    let root_name = sanitize_filename(&base_name(remote_root));
    let root_local = format!("{}/{}", local_parent.trim_end_matches('/'), root_name);
    tokio::fs::create_dir_all(&root_local)
        .await
        .with_context(|| format!("create local dir {root_local}"))?;

    // Stack of open local directories; index 0 is the root.
    let mut stack: Vec<String> = vec![root_local];
    loop {
        let line = match io.read_ctl_line(4096).await {
            Ok(l) => l,
            Err(_) => break, // channel closed by peer (End)
        };
        if line.is_empty() {
            break;
        }
        let c = line.as_bytes()[0];
        match c {
            b'C' => {
                let mut parts = line.split_whitespace();
                let _ = parts.next();
                let size: u64 = parts
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let name = parts.collect::<Vec<_>>().join(" ");
                let name = name.split(" -> ").next().unwrap_or(&name).to_string();
                io.write_all(&[0u8]).await?; // ACK C record
                let cur = stack.last().cloned().unwrap_or_default();
                let local = format!("{}/{}", cur, sanitize_filename(&name));
                let id = Uuid::new_v4().to_string();
                let data = match io.read_exact(size as usize).await {
                    Ok(d) => d,
                    Err(e) => {
                        return Err(e);
                    }
                };
                let mut f = tokio::fs::File::create(&local)
                    .await
                    .with_context(|| format!("create local {local}"))?;
                f.write_all(&data).await.context("write local file")?;
                f.flush().await.ok();
                let name_local = sanitize_filename(&name);
                emit_transfer(events, &id, &name_local, false, data.len() as u64, size, 1, "");
                // Trailing EOF marker + final ACK.
                let mut eof = [0u8; 1];
                let _ = io.read(&mut eof).await;
                let _ = io.write_all(&[0u8]).await;
            }
            b'D' => {
                let mut parts = line.split_whitespace();
                let _ = parts.next();
                let _ = parts.next();
                let name = parts.collect::<Vec<_>>().join(" ");
                io.write_all(&[0u8]).await?; // ACK D record
                let cur = stack.last().cloned().unwrap_or_default();
                let local = format!("{}/{}", cur, sanitize_filename(&name));
                tokio::fs::create_dir_all(&local)
                    .await
                    .with_context(|| format!("create local dir {local}"))?;
                stack.push(local);
            }
            b'E' => {
                io.write_all(&[0u8]).await?; // ACK E record
                if stack.len() > 1 {
                    stack.pop();
                } else {
                    break;
                }
            }
            b'T' => {
                // Timestamps (only sent with -p); ACK and skip.
                io.write_all(&[0u8]).await?;
            }
            _ => {
                return Err(anyhow!("unexpected SCP record: {line}"));
            }
        }
    }
    let _ = io.eof().await;
    io.close().await;
    Ok(())
}

/// Upload a local file to `remote` using `scp -t`. Emits transfer progress.
pub(crate) async fn scp_upload(
    handle: &SftpHandle,
    local: &Path,
    remote: &str,
    name: &str,
    id: &str,
    events: &tokio::sync::mpsc::UnboundedSender<SessionEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<bool> {
    let cmd = format!("scp -t -- {}", sh_quote(remote));
    let mut io = exec_scp(handle, &cmd).await?;

    let total = tokio::fs::metadata(local)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let mut local_file = tokio::fs::File::open(local)
        .await
        .with_context(|| format!("open local {}", local.display()))?;

    // OpenSSH's receiver greets with a `\0`; BusyBox's may not. Probe briefly.
    match timeout(Duration::from_millis(250), io.read_ack(4096)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(_) => {}
    }

    let mode = 0o644u32;
    let header = format!("C{mode:04o} {total} {}\n", sh_quote(name));
    io.write_all(header.as_bytes()).await?;
    io.read_ack(4096).await?;

    emit_transfer(events, id, name, true, 0, total, 0, "");
    let mut done = 0u64;
    let mut last = Instant::now();
    let mut err: Option<anyhow::Error> = None;
    let mut cancelled = false;

    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let n = local_file.read(&mut buf).await.context("read local file")?;
        if n == 0 {
            break;
        }
        io.write_all(&buf[..n]).await?;
        done += n as u64;
        if last.elapsed() >= Duration::from_millis(150) {
            last = Instant::now();
            emit_transfer(events, id, name, true, done, total, 0, "");
        }
    }
    if err.is_none() && !cancelled {
        io.write_all(&[0u8]).await?; // EOF marker
        if let Err(e) = io.read_ack(4096).await {
            err = Some(e);
        }
    }

    let _ = io.eof().await;
    io.close().await;

    if let Some(e) = err {
        return Err(e);
    }
    if cancelled {
        emit_transfer(events, id, name, true, done, total, 4, t("已取消", "Cancelled"));
        return Ok(false);
    }
    emit_transfer(events, id, name, true, done, total, 1, "");
    Ok(true)
}

/// Recursively upload a local directory tree into `remote_parent` over SCP:
/// shell `mkdir -p` for every directory, then `scp -t` per file (mirrors the
/// SFTP `upload_dir`).
pub(crate) async fn scp_upload_dir(
    handle: &SftpHandle,
    local_root: &Path,
    remote_parent: &str,
    events: &tokio::sync::mpsc::UnboundedSender<SessionEvent>,
) -> Result<()> {
    let root_name = local_file_name_utf8(local_root)?;
    let remote_root = format!("{}/{}", remote_parent.trim_end_matches('/'), root_name);

    // Pass 1: discover + mkdir every directory (best-effort per dir).
    let mut dirs: Vec<String> = vec![remote_root.clone()];
    let mut stack: Vec<(std::path::PathBuf, String)> = vec![(local_root.to_path_buf(), remote_root.clone())];
    while let Some((ldir, rdir)) = stack.pop() {
        let mut rd = tokio::fs::read_dir(&ldir)
            .await
            .with_context(|| format!("read local dir {}", ldir.display()))?;
        while let Some(entry) = rd.next_entry().await.context("read dir entry")? {
            let lpath = entry.path();
            let name = local_file_name_utf8(&lpath)?;
            let rchild = format!("{}/{}", rdir, name);
            if entry.file_type().await.context("file type")?.is_dir() {
                dirs.push(rchild.clone());
                stack.push((lpath, rchild));
            }
        }
    }
    for d in &dirs {
        let _ = scp_mkdir(handle, d).await;
    }

    // Pass 2: upload every file.
    let no_cancel = Arc::new(AtomicBool::new(false));
    let mut stack: Vec<(std::path::PathBuf, String)> = vec![(local_root.to_path_buf(), remote_root)];
    while let Some((ldir, rdir)) = stack.pop() {
        let mut rd = tokio::fs::read_dir(&ldir)
            .await
            .with_context(|| format!("read local dir {}", ldir.display()))?;
        while let Some(entry) = rd.next_entry().await.context("read dir entry")? {
            let lpath = entry.path();
            let name = local_file_name_utf8(&lpath)?;
            let rchild = format!("{}/{}", rdir, name);
            let ft = entry.file_type().await.context("file type")?;
            if ft.is_dir() {
                stack.push((lpath, rchild));
            } else if ft.is_file() {
                let id = Uuid::new_v4().to_string();
                scp_upload(handle, &lpath, &rchild, &name, &id, events, &no_cancel).await?;
            }
        }
    }
    Ok(())
}

/// Read a remote file's raw bytes via `scp -f` (used by the built-in editor).
pub(crate) async fn scp_read_text(handle: &SftpHandle, remote: &str) -> Result<Vec<u8>> {
    let cmd = format!("scp -f -- {}", sh_quote(remote));
    let mut io = exec_scp(handle, &cmd).await?;

    io.write_all(&[0u8]).await?; // greeting

    let line = io.read_ctl_line(4096).await?;
    if line.starts_with('\u{1}') || line.starts_with('\u{2}') {
        let msg = line.chars().skip(1).collect::<String>().trim().to_string();
        return Err(anyhow!(if msg.is_empty() {
            t("远端 SCP 错误", "remote scp error").to_string()
        } else {
            msg
        }));
    }
    if !line.starts_with('C') {
        return Err(anyhow!(
            "{}",
            t("远端未运行 SCP(可能不是 SCP 服务器)", "remote is not an SCP server")
        ));
    }
    let size: u64 = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if size > 4 * 1024 * 1024 {
        return Err(anyhow!(
            "{}",
            t("文件过大,无法通过 SCP 读取", "file too large to read over SCP")
        ));
    }
    io.write_all(&[0u8]).await?;
    let data = io.read_exact(size as usize).await?;
    let mut eof = [0u8; 1];
    let _ = io.read(&mut eof).await;
    let _ = io.write_all(&[0u8]).await;
    let _ = io.eof().await;
    io.close().await;
    Ok(data)
}

/// Write a text blob to a remote file via `scp -t` (used by the editor).
pub(crate) async fn scp_write_text(handle: &SftpHandle, remote: &str, content: &str) -> Result<()> {
    let cmd = format!("scp -t -- {}", sh_quote(remote));
    let mut io = exec_scp(handle, &cmd).await?;

    match timeout(Duration::from_millis(250), io.read_ack(4096)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(_) => {}
    }
    let name = base_name(remote);
    let data = content.as_bytes();
    let header = format!("C{:04o} {} {}\n", 0o644u32, data.len(), sh_quote(&name));
    io.write_all(header.as_bytes()).await?;
    io.read_ack(4096).await?;
    io.write_all(data).await?;
    io.write_all(&[0u8]).await?;
    io.read_ack(4096).await?;
    let _ = io.eof().await;
    io.close().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Shell-based FS operations (SCP has no delete/mkdir/rename/chmod primitive)
// ---------------------------------------------------------------------------

pub(crate) async fn scp_delete(handle: &SftpHandle, path: &str) -> Result<()> {
    run_shell(handle, &format!("rm -rf -- {}", sh_quote(path))).await
}

pub(crate) async fn scp_mkdir(handle: &SftpHandle, path: &str) -> Result<()> {
    run_shell(handle, &format!("mkdir -p -- {}", sh_quote(path))).await
}

pub(crate) async fn scp_touch(handle: &SftpHandle, path: &str) -> Result<()> {
    run_shell(handle, &format!("touch -- {}", sh_quote(path))).await
}

pub(crate) async fn scp_rename(handle: &SftpHandle, from: &str, to: &str) -> Result<()> {
    run_shell(handle, &format!("mv -f -- {} {}", sh_quote(from), sh_quote(to))).await
}

pub(crate) async fn scp_chmod(handle: &SftpHandle, path: &str, mode: u32) -> Result<()> {
    run_shell(handle, &format!("chmod {mode:o} -- {}", sh_quote(path))).await
}

// Import Uuid used in the transfer-id helpers above.
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ls_dirs_and_files() {
        let lines = "total 8\n\
            drwxr-xr-x 2 1000 1000 4096 Jan  1 00:00 bin\n\
            -rw-r--r-- 1 1000 1000    123 Jan  1 00:00 file.txt\n\
            -rw-r--r-- 1 1000 1000      0 Jan  1 00:00 empty\n\
            lrwxrwxrwx 1 1000 1000      4 Jan  1 00:00 link -> bin\n";
        let entries = parse_ls(lines, "/home/u");
        assert_eq!(entries.len(), 4);
        let dir = entries.iter().find(|e| e.name == "bin").unwrap();
        assert!(dir.is_dir);
        assert_eq!(dir.full_path, "/home/u/bin");
        let file = entries.iter().find(|e| e.name == "file.txt").unwrap();
        assert!(!file.is_dir);
        assert_eq!(file.size, 123);
        let link = entries.iter().find(|e| e.name == "link").unwrap();
        assert!(link.is_dir); // symlink to a dir counts as a dir for navigation
    }

    #[test]
    fn parse_ls_filters_dot_entries() {
        let lines = ".  ..  file\n";
        let entries = parse_ls(lines, "/");
        assert!(entries.iter().all(|e| e.name != "." && e.name != ".."));
    }

    #[test]
    fn sh_quote_escapes_single_quotes() {
        assert_eq!(sh_quote("a'b"), "'a'\\''b'");
        assert_eq!(sh_quote("plain"), "'plain'");
    }

    #[test]
    fn parse_ls_handles_names_with_spaces() {
        let lines = "-rw-r--r-- 1 1000 1000 9 Jan  1 00:00 my file.txt\n";
        let entries = parse_ls(lines, "/");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "my file.txt");
        assert_eq!(entries[0].full_path, "/my file.txt");
    }
}
