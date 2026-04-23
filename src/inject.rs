use anyhow::{Context, Result};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

use crate::config::InjectionMethod;

#[derive(Debug, Clone)]
pub struct Injector {
    method: InjectionMethod,
    delay_ms: u32,
    paste_shortcut: String,
}

impl Injector {
    pub fn new(method: InjectionMethod, delay_ms: u32, paste_shortcut: String) -> Self {
        Self {
            method,
            delay_ms,
            paste_shortcut,
        }
    }

    pub async fn inject(&self, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        match self.method {
            InjectionMethod::Type => self.inject_type(text).await,
            InjectionMethod::Paste => self.inject_paste(text).await,
        }
    }

    fn build_type_args(&self, text: &str) -> Vec<String> {
        vec![
            "type".to_string(),
            "--clearmodifiers".to_string(),
            "--delay".to_string(),
            self.delay_ms.to_string(),
            "--".to_string(),
            text.to_string(),
        ]
    }

    async fn inject_type(&self, text: &str) -> Result<()> {
        let output = tokio::process::Command::new("xdotool")
            .args(self.build_type_args(text))
            .output()
            .await
            .context("failed to spawn xdotool")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("xdotool exited with {}: {}", output.status, stderr.trim());
        }
        Ok(())
    }

    /// Clipboard-paste path: write `text` to the CLIPBOARD selection via
    /// `xclip`, send the configured paste shortcut, wait briefly for the
    /// target app to consume the clipboard, then kill xclip.
    ///
    /// Why the kill: `xclip -in` doesn't exit after setting the
    /// clipboard — it stays alive as the X selection owner to serve
    /// subsequent paste requests, forever by default. Waiting on it via
    /// `wait_with_output()` would deadlock the worker indefinitely,
    /// which shows up as "the daemon transcribes the first utterance
    /// fine but the second one is never processed." Instead, we send
    /// the paste keystroke, give the focused app ~150 ms to fetch the
    /// clipboard, and then force-kill xclip. The paste is already done
    /// by that point; xclip dying is invisible to the target app.
    async fn inject_paste(&self, text: &str) -> Result<()> {
        // Put xclip in its own process group so we can reliably kill the
        // forked daemon child later. Without this, `child.kill()` only
        // reaches the parent (which has already exited after daemonizing)
        // and the forked grandchild runs indefinitely.
        let mut child = tokio::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-in"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .context("failed to spawn xclip (is xclip installed? `sudo apt install xclip`)")?;
        let xclip_pid = child.id().context("xclip child pid was not available")? as i32;

        // Take() (rather than as_mut()) pulls ChildStdin out so that
        // dropping it closes the pipe — xclip needs EOF before it
        // claims the selection.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(text.as_bytes())
                .await
                .context("writing transcription to xclip stdin")?;
            drop(stdin);
        }

        // Give xclip a moment to actually claim the CLIPBOARD selection
        // before we trigger the paste. Without this, the focused app
        // can end up pasting the PREVIOUS clipboard contents (xclip
        // hasn't taken ownership yet when the keystroke fires).
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        // Kick off the paste. Capture the result — we want to surface
        // xdotool errors, but we MUST kill xclip before returning either
        // way so the worker isn't blocked on the next utterance.
        let xdotool_res = tokio::process::Command::new("xdotool")
            .args(["key", "--clearmodifiers", &self.paste_shortcut])
            .output()
            .await;

        // Let the target app fetch the clipboard from xclip before we
        // kill it. In practice the SelectionRequest / SelectionNotify
        // round-trip completes in <50 ms; 150 ms is slack.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        // SIGKILL the entire process group. We set process_group(0) at
        // spawn, so xclip's PID equals the PGID; negating it tells the
        // kernel to signal the whole group, catching any forked daemon
        // child. `child.kill()` alone only reaches the parent, which
        // has already exited after daemonizing, so the grandchild would
        // leak one process per utterance.
        unsafe {
            libc::kill(-xclip_pid, libc::SIGKILL);
        }
        let _ = child.wait().await;

        let xdotool_out = xdotool_res.context("failed to spawn xdotool")?;
        if !xdotool_out.status.success() {
            let stderr = String::from_utf8_lossy(&xdotool_out.stderr);
            anyhow::bail!(
                "xdotool key {} exited with {}: {}",
                self.paste_shortcut,
                xdotool_out.status,
                stderr.trim()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_type_args_shape() {
        let inj = Injector::new(InjectionMethod::Type, 5, "ctrl+v".to_string());
        let args = inj.build_type_args("hello");
        assert_eq!(
            args,
            vec!["type", "--clearmodifiers", "--delay", "5", "--", "hello"]
        );
    }

    #[test]
    fn test_build_type_args_separator_precedes_text() {
        // If text starts with `-`, it must appear AFTER the `--` separator
        // so xdotool doesn't interpret it as a flag.
        let inj = Injector::new(InjectionMethod::Type, 5, "ctrl+v".to_string());
        let args = inj.build_type_args("-flag-looking-text");
        let sep = args.iter().position(|a| a == "--").unwrap();
        let text = args.iter().position(|a| a == "-flag-looking-text").unwrap();
        assert!(sep < text, "separator must come before text");
    }

    #[test]
    fn test_build_type_args_custom_delay() {
        let inj = Injector::new(InjectionMethod::Type, 12, "ctrl+v".to_string());
        let args = inj.build_type_args("x");
        assert!(args.contains(&"12".to_string()));
    }

    #[test]
    fn test_build_type_args_empty_text() {
        let inj = Injector::new(InjectionMethod::Type, 5, "ctrl+v".to_string());
        let args = inj.build_type_args("");
        // empty string is still the last arg; inject() short-circuits, but build_args does not
        assert_eq!(args.last(), Some(&"".to_string()));
    }

    #[tokio::test]
    async fn test_inject_empty_or_whitespace_is_noop() {
        // Tests both methods — neither should spawn any external process
        // on empty/whitespace input.
        for method in [InjectionMethod::Type, InjectionMethod::Paste] {
            let inj = Injector::new(method, 5, "ctrl+v".to_string());
            assert!(inj.inject("").await.is_ok());
            assert!(inj.inject("   ").await.is_ok());
            assert!(inj.inject("\n").await.is_ok());
            assert!(inj.inject("\t  \n").await.is_ok());
        }
    }
}
