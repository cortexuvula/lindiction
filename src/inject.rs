use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Injector {
    delay_ms: u32,
}

impl Injector {
    pub fn new(delay_ms: u32) -> Self {
        Self { delay_ms }
    }

    fn build_args(&self, text: &str) -> Vec<String> {
        vec![
            "type".to_string(),
            "--clearmodifiers".to_string(),
            "--delay".to_string(),
            self.delay_ms.to_string(),
            "--".to_string(),
            text.to_string(),
        ]
    }

    pub async fn inject(&self, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        let output = tokio::process::Command::new("xdotool")
            .args(self.build_args(text))
            .output()
            .await
            .context("failed to spawn xdotool")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("xdotool exited with {}: {}", output.status, stderr.trim());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_args_shape() {
        let inj = Injector::new(5);
        let args = inj.build_args("hello");
        assert_eq!(
            args,
            vec!["type", "--clearmodifiers", "--delay", "5", "--", "hello"]
        );
    }

    #[test]
    fn test_build_args_separator_precedes_text() {
        // If text starts with `-`, it must appear AFTER the `--` separator
        // so xdotool doesn't interpret it as a flag.
        let inj = Injector::new(5);
        let args = inj.build_args("-flag-looking-text");
        let sep = args.iter().position(|a| a == "--").unwrap();
        let text = args.iter().position(|a| a == "-flag-looking-text").unwrap();
        assert!(sep < text, "separator must come before text");
    }

    #[test]
    fn test_build_args_custom_delay() {
        let inj = Injector::new(12);
        let args = inj.build_args("x");
        assert!(args.contains(&"12".to_string()));
    }

    #[test]
    fn test_build_args_empty_text() {
        let inj = Injector::new(5);
        let args = inj.build_args("");
        // empty string is still the last arg; inject() short-circuits, but build_args does not
        assert_eq!(args.last(), Some(&"".to_string()));
    }

    #[tokio::test]
    async fn test_inject_empty_or_whitespace_is_noop() {
        let inj = Injector::new(5);
        assert!(inj.inject("").await.is_ok());
        assert!(inj.inject("   ").await.is_ok());
        assert!(inj.inject("\n").await.is_ok());
        assert!(inj.inject("\t  \n").await.is_ok());
    }
}
