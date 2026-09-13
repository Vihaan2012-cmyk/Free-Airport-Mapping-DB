//! Small HTTP wrapper with retries and polite pacing (shared by all online sources).

use anyhow::{anyhow, Context, Result};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Http {
    agent: ureq::Agent,
    min_interval: Duration,
    last: Mutex<Option<Instant>>,
    user_agent: String,
}

impl Http {
    pub fn new(timeout_secs: u64, min_interval_ms: u64) -> Self {
        let cfg = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(timeout_secs)))
            .http_status_as_error(false)
            .build();
        Self {
            agent: cfg.new_agent(),
            min_interval: Duration::from_millis(min_interval_ms),
            last: Mutex::new(None),
            user_agent: format!("amdbgen/{} (+https://github.com/amdbgen)", env!("CARGO_PKG_VERSION")),
        }
    }

    fn pace(&self) {
        let mut last = self.last.lock().unwrap();
        if let Some(t) = *last {
            let el = t.elapsed();
            if el < self.min_interval {
                std::thread::sleep(self.min_interval - el);
            }
        }
        *last = Some(Instant::now());
    }

    fn with_retries<T>(&self, what: &str, mut f: impl FnMut() -> Result<T>) -> Result<T> {
        let mut delay = Duration::from_secs(2);
        let mut last_err = None;
        for attempt in 0..4 {
            self.pace();
            match f() {
                Ok(v) => return Ok(v),
                Err(e) => {
                    // A 4xx other than "slow down" / timeout is a definite answer: no retry.
                    let msg = format!("{e:#}");
                    let definite = msg.contains("HTTP 4") && !msg.contains("HTTP 408") && !msg.contains("HTTP 429");
                    if definite {
                        log::info!("{what}: {msg}");
                        return Err(e);
                    }
                    log::warn!("{what}: attempt {} failed: {msg}", attempt + 1);
                    last_err = Some(e);
                    std::thread::sleep(delay);
                    delay *= 2;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("{what}: failed")))
    }

    pub fn get_text(&self, url: &str) -> Result<String> {
        self.with_retries(url, || {
            let mut resp = self.agent.get(url).header("User-Agent", &self.user_agent).call().context("request")?;
            let status = resp.status().as_u16();
            if status >= 400 {
                return Err(anyhow!("HTTP {status}"));
            }
            resp.body_mut().with_config().limit(512 * 1024 * 1024).read_to_string().context("read body")
        })
    }

    pub fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        self.with_retries(url, || {
            let mut resp = self.agent.get(url).header("User-Agent", &self.user_agent).call().context("request")?;
            let status = resp.status().as_u16();
            if status >= 400 {
                return Err(anyhow!("HTTP {status}"));
            }
            resp.body_mut().with_config().limit(2 * 1024 * 1024 * 1024).read_to_vec().context("read body")
        })
    }

    /// One attempt GET, no retries; errors carry the HTTP status so callers can react.
    pub fn get_text_once(&self, url: &str) -> Result<String> {
        self.pace();
        let mut resp = self.agent.get(url).header("User-Agent", &self.user_agent).call().context("request")?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().with_config().limit(512 * 1024 * 1024).read_to_string().context("read body")?;
        if status >= 400 {
            return Err(anyhow!("HTTP {status}: {}", text.chars().take(160).collect::<String>()));
        }
        Ok(text)
    }

    /// One attempt, no retries: for callers that rotate between mirrors.
    pub fn post_form_text_once(&self, url: &str, form: &[(&str, &str)]) -> Result<String> {
        self.pace();
        let mut resp = self
            .agent
            .post(url)
            .header("User-Agent", &self.user_agent)
            .send_form(form.iter().copied())
            .context("request")?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().with_config().limit(512 * 1024 * 1024).read_to_string().context("read body")?;
        if status >= 400 {
            return Err(anyhow!("HTTP {status}"));
        }
        Ok(text)
    }

    pub fn post_form_text(&self, url: &str, form: &[(&str, &str)]) -> Result<String> {
        self.with_retries(url, || {
            let mut resp = self
                .agent
                .post(url)
                .header("User-Agent", &self.user_agent)
                .send_form(form.iter().copied())
                .context("request")?;
            let status = resp.status().as_u16();
            let text = resp.body_mut().with_config().limit(512 * 1024 * 1024).read_to_string().context("read body")?;
            if status >= 400 {
                return Err(anyhow!("HTTP {status}: {}", text.chars().take(200).collect::<String>()));
            }
            Ok(text)
        })
    }
}
