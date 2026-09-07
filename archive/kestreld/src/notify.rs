#![allow(dead_code)]

//! Telegram transport plus the HTML templates every outbound message renders through.
//!
//! Two rules hold the whole module together:
//!   1. **Every message is HTML** (`parse_mode=HTML` on the wire), so every string that came
//!      from outside the daemon — a model's thesis, a market name, an error class — must pass
//!      through [`esc`] before it lands in a template.
//!   2. **Templates are pure functions.** They take numbers, return a `String`, touch no clock
//!      and no network, so the exact bytes an operator will see are pinned by snapshot tests.
//!
//! Transport stays best-effort exactly as before: no token / no chat id is a silent no-op, a
//! failed POST is retried once and then dropped. A notification can never fail a trading task.

use reqwest::Client;
use std::time::Duration;
use tracing::debug;

/// Telegram `parse_mode` for every outbound message.
///
/// HTML, not MarkdownV2: its reserved set is three characters and closed, while MarkdownV2
/// requires escaping eighteen punctuation marks anywhere in the text — a rule a model-written
/// thesis breaks in the first sentence.
pub const PARSE_MODE: &str = "HTML";

/// Escape the three characters Telegram's HTML parser reserves.
///
/// `&` is replaced FIRST — otherwise the ampersands introduced by the `<`/`>` rules would be
/// escaped a second time and the operator would read `&amp;lt;`.
///
/// This is not cosmetic. Telegram rejects the ENTIRE `sendMessage` with 400 "can't parse
/// entities" when a message contains an unbalanced tag, so a single thesis containing `<b>`
/// would silence the alert channel until the model stopped writing it.
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Inline monospace for one value. Escapes its argument — `<code>` is a tag, not a shield.
pub fn code(s: &str) -> String {
    format!("<code>{}</code>", esc(s))
}

/// Compact duration for a header line: `45s`, `12m`, `3h 04m`, `2d 06h`.
pub fn fmt_dur(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
        s => format!("{}d {:02}h", s / 86_400, (s % 86_400) / 3600),
    }
}

/// Left-aligned label column used by every multi-line body, so the values line up inside the
/// monospace block instead of drifting with label length.
pub(crate) const LABEL_W: usize = 9;

/// One `label   value` line, escaped, for a `<pre>` body.
pub(crate) fn row(label: &str, value: &str) -> String {
    format!("{:<LABEL_W$}{}", esc(label), esc(value))
}

/// The operator-facing message templates. Pure; snapshot-tested.
pub mod tpl {
    use super::{code, esc, fmt_dur};

    /// 🟢 Daemon came up. `uptime_s` is process age at send time — a few seconds on a clean
    /// boot, which is exactly the signal that distinguishes a restart from a first start.
    pub fn boot(equity: f64, markets: usize, uptime_s: u64) -> String {
        format!(
            "🟢 <b>kestrel up</b>\nequity {}\nmarkets {}\nuptime {}",
            code(&format!("${equity:.2}")),
            code(&markets.to_string()),
            code(&fmt_dur(uptime_s)),
        )
    }

    /// 🛑 Kill switch latched for the rest of the UTC day. Carries the three numbers that
    /// decide it (`equity`, the floor it broke, the day-open it is measured against) so the
    /// operator never has to open the dashboard to know how far through the budget it went.
    pub fn kill_switch(equity: f64, floor: f64, day_open: f64) -> String {
        let dd_pct = if day_open.abs() > 1e-9 { (equity - day_open) / day_open * 100.0 } else { 0.0 };
        format!(
            "🛑 <b>KILL SWITCH</b>\nequity {} vs floor {}\nday open {} ({})\nentries halted until {}",
            code(&format!("${equity:.2}")),
            code(&format!("${floor:.2}")),
            code(&format!("${day_open:.2}")),
            code(&format!("{dd_pct:+.2}%")),
            code("00:00 UTC"),
        )
    }

    /// ⚠️ The self-probe could not reach the daemon's own API N times running.
    pub fn wedge(endpoint: &str, consecutive_fails: u32) -> String {
        format!(
            "⚠️ <b>API wedge detected</b>\nendpoint {}\nfails {} consecutive\nstate unreachable — daemon may need a restart",
            code(endpoint),
            code(&consecutive_fails.to_string()),
        )
    }

    /// 📡 No market data arrived on either ws stream for `age_s`.
    pub fn stream_death(age_s: i64, stream: &str) -> String {
        format!(
            "📡 <b>WS feed stale</b>\nage {}\nstream {}\nno frames — reconnect backoff running",
            code(&format!("{age_s}s")),
            code(stream),
        )
    }

    /// ⏸ The day's entry budget is spent. Fired at most once per UTC day (`DayOnce`).
    pub fn daily_cap(count: usize, cap: usize, resumes_at: &str) -> String {
        format!(
            "⏸ <b>Daily cap reached</b>\nentries {}\nresumes {}\nreviews and exits continue as normal",
            code(&format!("{count}/{cap}")),
            code(resumes_at),
        )
    }

    /// 🟩 A paper position opened. `thesis` is model output — escaped, never trusted.
    pub fn opened(market: &str, side: &str, px: f64, leverage: f64, thesis: &str) -> String {
        format!(
            "🟩 <b>OPEN</b> {} {}\nentry {} lev {}\n<i>{}</i>",
            code(market),
            code(side),
            code(&format!("{px:.4}")),
            code(&format!("{leverage:.0}x")),
            esc(thesis),
        )
    }

    /// 🟥 The reviewer closed a position early.
    pub fn veto_close(market: &str, px: f64) -> String {
        format!("🟥 <b>VETO CLOSE</b> {}\nmark {}", code(market), code(&format!("{px:.4}")))
    }

    /// A bracket fired: stop, target, or the time stop. `net` is the exit's realized minus its
    /// own fee — the number that actually moved the book.
    pub fn triggered(reason: &str, market: &str, px: f64, net: f64) -> String {
        let lead = match reason {
            "tp" => "🎯",
            "sl" => "🔻",
            _ => "⏱",
        };
        format!(
            "{lead} <b>{}</b> {}\nexit {} net {}",
            esc(&reason.to_uppercase()),
            code(market),
            code(&format!("{px:.4}")),
            code(&format!("{net:+.2}")),
        )
    }
}

#[derive(Debug, Clone)]
pub struct Notify {
    token_env: String,
    chat_id: String,
    client: Client,
    base_url: String,
}

impl Notify {
    pub fn new(token_env: String, chat_id: String) -> Self {
        let client = Client::builder().timeout(Duration::from_secs(5)).build().expect("client");
        Self { token_env, chat_id, client, base_url: "https://api.telegram.org".to_string() }
    }

    pub fn new_with_base(token_env: String, chat_id: String, base_url: String) -> Self {
        let client = Client::builder().timeout(Duration::from_secs(5)).build().expect("client");
        Self { token_env, chat_id, client, base_url }
    }

    /// Send one message. `html` must already be template output — anything interpolated into
    /// it from outside the daemon has to have gone through [`esc`], or Telegram 400s the whole
    /// message. Best-effort: unconfigured is a silent no-op, one retry, then give up.
    pub async fn send(&self, html: &str) {
        if self.token_env.is_empty() || self.chat_id.is_empty() {
            return;
        }
        let token = match std::env::var(&self.token_env) {
            Ok(v) if !v.is_empty() => v,
            _ => return,
        };
        let url = format!("{}/bot{}/sendMessage", self.base_url.trim_end_matches('/'), token);
        let body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": html,
            "parse_mode": PARSE_MODE,
            "disable_web_page_preview": true,
        });
        // fire-and-forget 1 retry
        let mut tries = 0;
        loop {
            match self.client.post(&url).json(&body).send().await {
                Ok(resp) if resp.status().is_success() => break,
                Ok(resp) => {
                    debug!(status=%resp.status(), "telegram send failed");
                }
                Err(e) => {
                    debug!(error=%e, "telegram send error");
                }
            }
            tries += 1;
            if tries >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::post};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    #[test]
    fn escaping_is_injection_proof_and_orders_ampersand_first() {
        assert_eq!(esc("<b>bold</b>"), "&lt;b&gt;bold&lt;/b&gt;");
        assert_eq!(esc("R&D"), "R&amp;D");
        // the ampersand rule runs FIRST: escaping `<` must not be re-escaped into &amp;lt;
        assert_eq!(esc("a < b & c > d"), "a &lt; b &amp; c &gt; d");
        assert_eq!(esc("&lt;"), "&amp;lt;", "already-escaped input is escaped again, never unwrapped");
        // quotes and everything else are NOT reserved in Telegram HTML text nodes
        assert_eq!(esc("it's \"fine\""), "it's \"fine\"");
        assert_eq!(esc(""), "");
    }

    #[test]
    fn a_hostile_thesis_cannot_open_a_tag() {
        // A model writing markup (or an ampersand) must not be able to unbalance the message:
        // Telegram 400s the whole sendMessage on a stray tag, which would mute the channel.
        let hostile = "<b>pump</b> & <script>alert(1)</script> 5 > 3";
        let msg = tpl::opened("SOL", "long", 123.4567, 5.0, hostile);
        assert!(!msg.contains("<script"), "raw tag leaked: {msg}");
        assert!(msg.contains("&lt;b&gt;pump&lt;/b&gt; &amp;"), "thesis not escaped: {msg}");
        assert!(msg.contains("5 &gt; 3"));
        // the only tags left are the template's own, and they balance
        assert_eq!(msg.matches("<b>").count(), 1);
        assert_eq!(msg.matches("</b>").count(), 1);
        assert_eq!(msg.matches("<i>").count(), 1);
        assert_eq!(msg.matches("</i>").count(), 1);
        assert_eq!(msg.matches("<code>").count(), msg.matches("</code>").count());
        // a market name is venue-derived and gets the same treatment
        assert!(tpl::opened("xyz:A<B", "short", 1.0, 3.0, "t").contains("xyz:A&lt;B"));
    }

    #[test]
    fn boot_template_snapshot() {
        assert_eq!(
            tpl::boot(1012.3456, 42, 7),
            "🟢 <b>kestrel up</b>\nequity <code>$1012.35</code>\nmarkets <code>42</code>\nuptime <code>7s</code>"
        );
    }

    #[test]
    fn kill_template_snapshot() {
        // 880 vs day-open 1000 is -12.00%, the user-locked kill threshold
        assert_eq!(
            tpl::kill_switch(880.0, 880.0, 1000.0),
            "🛑 <b>KILL SWITCH</b>\nequity <code>$880.00</code> vs floor <code>$880.00</code>\nday open <code>$1000.00</code> (<code>-12.00%</code>)\nentries halted until <code>00:00 UTC</code>"
        );
        // a zero day-open cannot divide by zero into NaN%
        assert!(tpl::kill_switch(0.0, 0.0, 0.0).contains("<code>+0.00%</code>"));
    }

    #[test]
    fn operational_templates_carry_their_numbers() {
        let w = tpl::wedge("/api/snapshot", 2);
        assert!(w.starts_with("⚠️ <b>API wedge detected</b>"), "{w}");
        assert!(w.contains("<code>/api/snapshot</code>") && w.contains("<code>2</code>"), "{w}");

        let s = tpl::stream_death(312, "allMids+activeAssetCtx");
        assert!(s.starts_with("📡 <b>WS feed stale</b>"), "{s}");
        assert!(s.contains("<code>312s</code>") && s.contains("allMids+activeAssetCtx"), "{s}");

        let d = tpl::daily_cap(8, 8, "00:00 UTC");
        assert!(d.starts_with("⏸ <b>Daily cap reached</b>"), "{d}");
        assert!(d.contains("<code>8/8</code>") && d.contains("<code>00:00 UTC</code>"), "{d}");

        let v = tpl::veto_close("BTC", 61234.5);
        assert_eq!(v, "🟥 <b>VETO CLOSE</b> <code>BTC</code>\nmark <code>61234.5000</code>");

        // bracket exits: the cause picks the emoji, the net is signed
        assert_eq!(
            tpl::triggered("tp", "SOL", 123.4567, 4.2),
            "🎯 <b>TP</b> <code>SOL</code>\nexit <code>123.4567</code> net <code>+4.20</code>"
        );
        assert!(tpl::triggered("sl", "SOL", 1.0, -2.0).starts_with("🔻 <b>SL</b>"));
        assert!(tpl::triggered("time_stop", "SOL", 1.0, 0.0).starts_with("⏱ <b>TIME_STOP</b>"));
        assert!(tpl::triggered("sl", "SOL", 1.0, -2.0).contains("<code>-2.00</code>"));
    }

    #[test]
    fn duration_formatting_steps_units() {
        assert_eq!(fmt_dur(0), "0s");
        assert_eq!(fmt_dur(59), "59s");
        assert_eq!(fmt_dur(60), "1m");
        assert_eq!(fmt_dur(3599), "59m");
        assert_eq!(fmt_dur(3600), "1h 00m");
        assert_eq!(fmt_dur(3600 * 3 + 60 * 4), "3h 04m");
        assert_eq!(fmt_dur(86_400), "1d 00h");
        assert_eq!(fmt_dur(86_400 * 2 + 3600 * 6), "2d 06h");
    }

    #[tokio::test]
    async fn send_hits_mock_with_html_parse_mode() {
        let captured: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let cap = captured.clone();
        let app = Router::new().route("/botTESTTOKEN/sendMessage", post(move |Json(body): Json<serde_json::Value>| {
            let c = cap.clone();
            async move {
                *c.lock().unwrap() = Some(body);
                Json(serde_json::json!({"ok": true}))
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let base = format!("http://{addr}");
        unsafe { std::env::set_var("TG_BOT_TOKEN", "TESTTOKEN"); }
        let notify = Notify::new_with_base("TG_BOT_TOKEN".into(), "12345".into(), base);
        notify.send("<b>hello</b> world").await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let val = captured.lock().unwrap().clone().expect("captured");
        assert_eq!(val["chat_id"], "12345");
        assert_eq!(val["text"], "<b>hello</b> world");
        assert_eq!(val["parse_mode"], "HTML", "every message must render as HTML");
        assert_eq!(val["disable_web_page_preview"], true);
    }

    #[tokio::test]
    async fn send_noop_if_unset() {
        let notify = Notify::new("UNSET_TOKEN_ENV_XYZ".into(), "".into());
        // should not panic
        notify.send("test").await;
        let notify2 = Notify::new("TG_BOT_TOKEN".into(), "".into());
        notify2.send("test").await;
    }
}
