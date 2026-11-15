use crate::acp::types::{
    ReadTextFileRequest, ReadTextFileResponse, RpcError, WriteTextFileRequest,
    WriteTextFileResponse,
};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct FileWriteRecord {
    pub path: String,
    pub old_text: Option<String>,
    pub new_text: String,
}

fn normalize_fs_path(raw: &str, default_cwd: &Path) -> PathBuf {
    let stripped = raw.strip_prefix("file://").unwrap_or(raw);
    let p = Path::new(stripped);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        default_cwd.join(p)
    }
}

pub async fn handle_read_text_file(
    req: ReadTextFileRequest,
    default_cwd: &Path,
) -> Result<ReadTextFileResponse, RpcError> {
    let path = normalize_fs_path(&req.path, default_cwd);
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(RpcError::resource_not_found(&req.path));
        }
        Err(e) => {
            return Err(RpcError::internal_error(format!(
                "Failed to read {}: {e}",
                path.display()
            )));
        }
    };

    if req.line.is_none() && req.limit.is_none() {
        return Ok(ReadTextFileResponse { content });
    }

    let start_line = req.line.unwrap_or(1).saturating_sub(1);
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    if start_line >= lines.len() {
        return Ok(ReadTextFileResponse {
            content: String::new(),
        });
    }

    let slice = match req.limit {
        Some(limit) => &lines[start_line..lines.len().min(start_line.saturating_add(limit))],
        None => &lines[start_line..],
    };

    Ok(ReadTextFileResponse {
        content: slice.concat(),
    })
}

pub async fn handle_write_text_file(
    req: WriteTextFileRequest,
    default_cwd: &Path,
) -> Result<(WriteTextFileResponse, FileWriteRecord), RpcError> {
    let path = normalize_fs_path(&req.path, default_cwd);
    let old_text = tokio::fs::read_to_string(&path).await.ok();

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = tokio::fs::create_dir_all(parent).await
    {
        return Err(RpcError::internal_error(format!(
            "Failed to create directory {}: {e}",
            parent.display()
        )));
    }

    if let Err(e) = tokio::fs::write(&path, &req.content).await {
        return Err(RpcError::internal_error(format!(
            "Failed to write {}: {e}",
            path.display()
        )));
    }

    let record = FileWriteRecord {
        path: path.to_string_lossy().into_owned(),
        old_text,
        new_text: req.content,
    };

    Ok((WriteTextFileResponse {}, record))
}
