#[path = "impls/sftp.rs"]
pub(crate) mod sftp;
#[path = "impls/scp.rs"]
pub(crate) mod scp;
#[path = "struct/transfer.rs"]
mod transfer;

pub(crate) use sftp::*;
pub(crate) use transfer::{
    DownloadConflict, SftpCommand, SftpHandle, SftpHandles, SftpLastCwd,
};
