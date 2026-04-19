use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub model_path: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    pub xdotool_delay_ms: u32,
}

impl Config {
    pub fn load() -> Self {
        let model_path = std::env::var("LINDICTION_MODEL")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("models/ggml-tiny.en.bin"));
        Self {
            model_path,
            sample_rate: 16_000,
            channels: 1,
            xdotool_delay_ms: 5,
        }
    }

    pub fn with_model_path(mut self, path: PathBuf) -> Self {
        self.model_path = path;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_model_path() {
        // Ensure env var is unset for this test
        std::env::remove_var("LINDICTION_MODEL");
        let config = Config::load();
        assert_eq!(config.model_path, PathBuf::from("models/ggml-tiny.en.bin"));
    }

    #[test]
    fn test_env_override() {
        std::env::set_var("LINDICTION_MODEL", "/tmp/custom.bin");
        let config = Config::load();
        assert_eq!(config.model_path, PathBuf::from("/tmp/custom.bin"));
        std::env::remove_var("LINDICTION_MODEL");
    }

    #[test]
    fn test_audio_defaults() {
        std::env::remove_var("LINDICTION_MODEL");
        let config = Config::load();
        assert_eq!(config.sample_rate, 16_000);
        assert_eq!(config.channels, 1);
    }

    #[test]
    fn test_xdotool_delay_default() {
        std::env::remove_var("LINDICTION_MODEL");
        let config = Config::load();
        assert_eq!(config.xdotool_delay_ms, 5);
    }
}
