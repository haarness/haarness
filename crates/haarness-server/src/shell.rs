//! Stream shell output to disk while retaining only a bounded tail in memory.
use haarness_core::{CoreError, CoreResult, ToolOutput};
use serde_json::json;
use std::{collections::BTreeMap, path::Path, process::Stdio, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, process::Command};
use tokio_util::sync::CancellationToken;

struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(self.0) {
            let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL);
        }
        #[cfg(windows)]
        let _ = std::process::Command::new("taskkill").args(["/F", "/T", "/PID", &self.0.to_string()]).output();
    }
}

/// `env` adds or overrides variables for this one invocation (`shell.env`
/// hooks); the parent environment is otherwise inherited unchanged.
pub(crate) async fn run(command: &str, workdir: &Path, timeout_ms: u64, cancel: &CancellationToken, directory: &Path, env: &BTreeMap<String, String>) -> CoreResult<ToolOutput> {
    let error = |e: std::io::Error| CoreError::Tool(e.to_string());
    let mut command = haarness_ripgrep::shell_command(command).map_err(error)?;
    command.current_dir(workdir).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    command.envs(env.iter().filter(|(key, value)| !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0')));
    #[cfg(unix)]
    { use std::os::unix::process::CommandExt; command.process_group(0); }
    tokio::fs::create_dir_all(directory).await.map_err(error)?;
    let path = directory.join(format!("tool_{}.txt", uuid::Uuid::now_v7()));
    let mut file = tokio::fs::File::create(&path).await.map_err(error)?;
    let mut child = Command::from(command).kill_on_drop(true).spawn().map_err(error)?;
    let _group = ProcessGroup(child.id().ok_or_else(|| CoreError::Tool("shell process did not start".into()))?);
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut tail = Vec::new();
    let mut stdout_tail = Vec::new();
    let mut total = 0usize;
    let mut lines = 0usize;
    let execution = async {
        // Heap buffers keep this future small enough to await unboxed.
        let mut out = vec![0u8; 8192];
        let mut err = vec![0u8; 8192];
        let mut out_done = false;
        let mut err_done = false;
        let mut status = None;
        while !out_done || !err_done || status.is_none() {
            let bytes = tokio::select! {
                value = stdout.read(&mut out), if !out_done => {
                    let n = value.map_err(error)?; out_done = n == 0;
                    stdout_tail.extend_from_slice(&out[..n]);
                    if stdout_tail.len() > 50 * 1024 { stdout_tail.drain(..stdout_tail.len() - 50 * 1024); }
                    &out[..n]
                },
                value = stderr.read(&mut err), if !err_done => {
                    let n = value.map_err(error)?; err_done = n == 0; &err[..n]
                },
                value = child.wait(), if status.is_none() => { status = Some(value.map_err(error)?); continue; }
            };
            total += bytes.len();
            lines += bytes.iter().filter(|&&b| b == b'\n').count();
            file.write_all(bytes).await.map_err(error)?;
            tail.extend_from_slice(bytes);
            if tail.len() > 50 * 1024 { tail.drain(..tail.len() - 50 * 1024); }
        }
        file.flush().await.map_err(error)?;
        Ok::<_, CoreError>(status.expect("wait completed").code().unwrap_or(-1))
    };
    let deadline = async {
        if timeout_ms == 0 { std::future::pending::<()>().await; }
        else { tokio::time::sleep(Duration::from_millis(timeout_ms)).await; }
    };
    let result = tokio::select! {
        biased;
        () = cancel.cancelled() => Err(CoreError::Cancelled),
        () = deadline => Err(CoreError::Tool(format!("command timed out after {timeout_ms} ms; output saved to {}", path.display()))),
        result = execution => result,
    };
    let exit = result?;
    let truncated = total > 50 * 1024 || lines > 2000;
    let text = String::from_utf8_lossy(&tail);
    let preview = text.lines().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    let stdout_text = String::from_utf8_lossy(&stdout_tail);
    let stdout_preview = stdout_text.lines().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    let mut metadata = json!({"exit":exit,"truncated":truncated,"stdout": if stdout_text.lines().count() > 2000 { stdout_preview.as_str() } else { stdout_text.as_ref() }});
    let notice = if truncated {
        metadata["outputPath"] = json!(path);
        format!("Full output saved to: {}\n", path.display())
    } else {
        drop(file);
        let _ = tokio::fs::remove_file(&path).await;
        String::new()
    };
    Ok(ToolOutput { text: format!("exit: {exit}\n{notice}{}", if truncated { preview.as_str() } else { text.as_ref() }), metadata })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn fixture() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("haarness-shell-parity-{}",uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap(); path
    }
    #[tokio::test]
    async fn streaming_preserves_exit_and_stdout_while_bounding_large_output() {
        let root=fixture();
        let output=run("printf stdout; printf stderr >&2; exit 7",&root,0,&CancellationToken::new(),&root,&BTreeMap::new()).await.unwrap();
        assert_eq!(output.metadata["exit"],7);
        assert_eq!(output.metadata["stdout"],"stdout");
        assert!(output.text.contains("stderr"));
        let env=BTreeMap::from([("HOOK_SHELL_VAR".to_owned(),"from-hook".to_owned()),("BAD=KEY".to_owned(),"skipped".to_owned())]);
        let output=run("printf '%s' \"$HOOK_SHELL_VAR\"",&root,0,&CancellationToken::new(),&root,&env).await.unwrap();
        assert_eq!(output.metadata["stdout"],"from-hook");
        let output=run("awk 'BEGIN {for(i=0;i<50000;i++) print i}'",&root,600_001,&CancellationToken::new(),&root,&BTreeMap::new()).await.unwrap();
        assert_eq!(output.metadata["truncated"],true);
        assert!(output.text.len()<52*1024);
        assert!(output.text.contains("49999"));
        let saved=output.metadata["outputPath"].as_str().unwrap();
        assert_eq!(std::fs::read_to_string(saved).unwrap().lines().count(),50000);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn zero_deadline_is_cancellable_and_timeout_kills_descendants() {
        for timeout in [0,3000] {
            let root=fixture(); let cancel=CancellationToken::new(); let token=cancel.clone();let work=root.clone();
            let task=tokio::spawn(async move { run("sh -c 'sleep 5; touch escaped' & printf ready > ready; wait",&work,timeout,&token,&work,&BTreeMap::new()).await });
            tokio::time::timeout(Duration::from_secs(5),async { while !root.join("ready").exists() {tokio::time::sleep(Duration::from_millis(10)).await;} }).await.unwrap();
            if timeout==0 {cancel.cancel();}
            let error=task.await.unwrap().unwrap_err();
            if timeout==0 {assert!(matches!(error,CoreError::Cancelled));} else {assert!(error.to_string().contains("timed out"));}
            tokio::time::sleep(Duration::from_millis(5500)).await;
            assert!(!root.join("escaped").exists());
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
