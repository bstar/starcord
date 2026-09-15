//! Putting a captcha in front of the person, when Discord asks for one.
//!
//! Discord answers a login it is not sure about with `400` and a body naming
//! an hCaptcha site key. A browser client renders the widget, the person
//! clicks the pictures, and the request is sent again with the answer in
//! `X-Captcha-Key`. A terminal cannot render the widget, but the widget is a
//! script that runs on any page, and hCaptcha serves Discord's site key to a
//! page on `127.0.0.1` exactly as it does to one on discord.com — checked
//! against `checksiteconfig` on 2026-09-14.
//!
//! So: listen on a loopback port, open the page in the person's browser, and
//! wait for the page to post the answer back. Nothing is solved here; that is
//! the point of a captcha, and the point of this file is only to carry it to
//! somebody who can.
//!
//! The server is as small as a server can be. It answers two requests, a `GET`
//! of the page and a `POST` of the answer, both under a path nobody can guess,
//! and closes the moment it has the answer. The page loads its script from
//! hCaptcha; nothing else leaves the machine.

use std::net::SocketAddr;
use std::time::Duration;

use rand_core::RngCore as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use crate::discord::http::{CaptchaAnswer, CaptchaChallenge};

/// How long a person gets. hCaptcha itself expires an unanswered challenge
/// well inside this; the ticket it is for is Discord's to time out.
pub const SOLVE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The one captcha service this can carry. Discord also has Turnstile
/// behind a flag; a response naming it is reported, not rendered.
const HCAPTCHA: &str = "hcaptcha";

#[derive(Debug, thiserror::Error)]
pub enum CaptchaError {
    #[error("discord asked for a {0} captcha, which this client cannot show")]
    Unsupported(String),
    #[error("could not listen for the captcha answer: {0}")]
    Listen(std::io::Error),
    #[error("nobody answered the captcha in time")]
    TimedOut,
}

/// A page waiting to be opened.
pub struct Solver {
    listener: TcpListener,
    /// Unguessable, so that only the page this handed out can post an answer.
    path: String,
    page: String,
}

impl Solver {
    /// Bind the loopback port and build the page. Nothing is opened yet: the
    /// caller announces the URL first, so that a browser which fails to start
    /// still leaves a link on the screen to type by hand.
    pub async fn new(challenge: &CaptchaChallenge) -> Result<Self, CaptchaError> {
        if challenge.service != HCAPTCHA {
            return Err(CaptchaError::Unsupported(challenge.service.clone()));
        }
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(CaptchaError::Listen)?;

        let mut nonce = [0u8; 16];
        rand_core::OsRng.fill_bytes(&mut nonce);
        let path = format!("/{}", hex(&nonce));
        let page = page(challenge, &path);
        Ok(Self {
            listener,
            path,
            page,
        })
    }

    /// Where the browser should go.
    pub fn url(&self) -> String {
        let port = self
            .listener
            .local_addr()
            .map(|a: SocketAddr| a.port())
            .unwrap_or(0);
        format!("http://127.0.0.1:{port}{}/", self.path)
    }

    /// Serve the page until an answer arrives, or the clock runs out.
    pub async fn wait(self, challenge: &CaptchaChallenge) -> Result<CaptchaAnswer, CaptchaError> {
        let serve = async {
            loop {
                let Ok((stream, _)) = self.listener.accept().await else {
                    continue;
                };
                match handle(stream, &self.path, &self.page).await {
                    Ok(Some(key)) => return key,
                    Ok(None) => {}
                    Err(e) => tracing::debug!("a captcha page request failed: {e}"),
                }
            }
        };
        let key = tokio::time::timeout(SOLVE_TIMEOUT, serve)
            .await
            .map_err(|_| CaptchaError::TimedOut)?;
        Ok(CaptchaAnswer {
            key,
            rqtoken: challenge.rqtoken.clone(),
            session_id: challenge.session_id.clone(),
        })
    }
}

/// One connection. `Ok(Some)` is the answer; `Ok(None)` was a request for the
/// page, or for something else, and the wait goes on.
async fn handle(mut stream: TcpStream, path: &str, page: &str) -> std::io::Result<Option<String>> {
    // A request is small: a line, a few headers, and for the answer a body of
    // a couple of kilobytes. Anything bigger is not from the page.
    const LIMIT: usize = 64 * 1024;
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let (head_end, body_len) = loop {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .map_err(|_| std::io::Error::other("the request stalled"))??;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = find(&buf, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..at]).into_owned();
            break (at + 4, content_length(&head));
        }
        if buf.len() > LIMIT {
            return Ok(None);
        }
    };
    while buf.len() < head_end + body_len {
        if buf.len() > LIMIT {
            return Ok(None);
        }
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .map_err(|_| std::io::Error::other("the body stalled"))??;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut line = head.lines().next().unwrap_or("").split_whitespace();
    let method = line.next().unwrap_or("");
    let target = line.next().unwrap_or("");

    let page_path = format!("{path}/");
    let answer_path = format!("{path}/answer");
    if method == "GET" && (target == page_path || target == path) {
        respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            page.as_bytes(),
        )
        .await?;
        return Ok(None);
    }
    if method == "POST" && target == answer_path {
        let body = &buf[head_end..(head_end + body_len).min(buf.len())];
        let key = String::from_utf8_lossy(body).trim().to_string();
        if key.is_empty() || key.chars().any(|c| c.is_control() || c.is_whitespace()) {
            respond(
                &mut stream,
                "400 Bad Request",
                "text/plain",
                b"that is not an answer",
            )
            .await?;
            return Ok(None);
        }
        respond(&mut stream, "200 OK", "text/plain", b"ok").await?;
        return Ok(Some(key));
    }
    respond(&mut stream, "404 Not Found", "text/plain", b"nothing here").await?;
    Ok(None)
}

async fn respond(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}

fn content_length(head: &str) -> usize {
    head.lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The page. Everything the widget needs is written into it, so the browser
/// makes one request here and the rest to hCaptcha.
///
/// `rqdata` goes in through `setData`, which is how Discord's own client
/// passes it: the challenge is bound to the request that provoked it, and a
/// widget without it produces an answer Discord will not accept.
fn page(challenge: &CaptchaChallenge, path: &str) -> String {
    let sitekey = js_string(&challenge.sitekey);
    let rqdata = match &challenge.rqdata {
        Some(rqdata) => js_string(rqdata),
        None => "null".to_string(),
    };
    let answer = js_string(&format!("{path}/answer"));
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>STAR/CORD — one more step</title>
<script src="https://js.hcaptcha.com/1/api.js?render=explicit&onload=starcordReady" async defer></script>
<style>
  body {{ margin: 0; min-height: 100vh; display: grid; place-items: center;
         background: #1e1e2e; color: #cdd6f4; font: 16px/1.5 system-ui, sans-serif; }}
  main {{ text-align: center; padding: 2rem; max-width: 32rem; }}
  h1 {{ font-size: 1.1rem; letter-spacing: .08em; color: #89b4fa; margin: 0 0 1rem; }}
  p {{ margin: .5rem 0; }}
  #status {{ color: #a6adc8; min-height: 1.5em; }}
  #widget {{ display: inline-block; margin: 1rem 0; }}
</style>
</head>
<body>
<main>
  <h1>STAR/CORD</h1>
  <p>Discord wants to check that a person is signing in.</p>
  <div id="widget"></div>
  <p id="status">loading the check…</p>
</main>
<script>
  var status = document.getElementById('status');
  function say(t) {{ status.textContent = t; }}
  function starcordReady() {{
    try {{
      var id = hcaptcha.render('widget', {{
        sitekey: {sitekey},
        theme: 'dark',
        callback: function (token) {{
          say('sending the answer back…');
          fetch({answer}, {{ method: 'POST', headers: {{ 'Content-Type': 'text/plain' }}, body: token }})
            .then(function (r) {{ say(r.ok ? 'done — go back to the terminal. You can close this tab.'
                                         : 'the terminal did not accept that; is starcord still running?'); }})
            .catch(function () {{ say('could not reach the terminal; is starcord still running?'); }});
        }},
        'error-callback': function (e) {{ say('the check failed: ' + e); }},
        'expired-callback': function () {{ say('that expired; solve it again'); }}
      }});
      var rqdata = {rqdata};
      if (rqdata) hcaptcha.setData(id, {{ rqdata: rqdata }});
      say('');
    }} catch (e) {{ say('could not show the check: ' + e); }}
  }}
</script>
</body>
</html>
"#
    )
}

/// A JSON string is a JavaScript string literal — once `<` is escaped, so
/// that a value containing `</script>` stays inside the script it is in.
fn js_string(s: &str) -> String {
    serde_json::to_string(s)
        .unwrap_or_else(|_| "\"\"".into())
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn challenge() -> CaptchaChallenge {
        CaptchaChallenge {
            service: HCAPTCHA.into(),
            sitekey: "a9b5fb07-92ff-493f-86fe-352a2803b3df".into(),
            rqdata: Some("rq/data+==".into()),
            rqtoken: Some("rqtoken".into()),
            session_id: Some("session".into()),
        }
    }

    #[tokio::test]
    async fn the_page_is_served_and_the_answer_comes_back() {
        let solver = Solver::new(&challenge()).await.unwrap();
        let url = solver.url();
        assert!(url.starts_with("http://127.0.0.1:"));

        let client = tokio::spawn(async move {
            let base = url.trim_end_matches('/').to_string();
            let addr = base
                .trim_start_matches("http://")
                .split('/')
                .next()
                .unwrap()
                .to_string();
            let path = &base[("http://".len() + addr.len())..];

            let mut s = TcpStream::connect(&addr).await.unwrap();
            s.write_all(format!("GET {path}/ HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut page = String::new();
            s.read_to_string(&mut page).await.unwrap();
            assert!(page.starts_with("HTTP/1.1 200"));
            assert!(page.contains("a9b5fb07-92ff-493f-86fe-352a2803b3df"));
            assert!(page.contains(r#"setData(id, { rqdata: rqdata })"#));

            let mut s = TcpStream::connect(&addr).await.unwrap();
            s.write_all(
                "POST /wrong/answer HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\n\r\nabc"
                    .to_string()
                    .as_bytes(),
            )
            .await
            .unwrap();
            let mut reply = String::new();
            s.read_to_string(&mut reply).await.unwrap();
            assert!(reply.starts_with("HTTP/1.1 404"));

            let mut s = TcpStream::connect(&addr).await.unwrap();
            s.write_all(
                format!(
                    "POST {path}/answer HTTP/1.1\r\nHost: x\r\nContent-Length: 9\r\n\r\nP1_answer"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
            let mut reply = String::new();
            s.read_to_string(&mut reply).await.unwrap();
            assert!(reply.starts_with("HTTP/1.1 200"));
        });

        let answer = solver.wait(&challenge()).await.unwrap();
        client.await.unwrap();
        assert_eq!(answer.key, "P1_answer");
        assert_eq!(answer.rqtoken.as_deref(), Some("rqtoken"));
        assert_eq!(answer.session_id.as_deref(), Some("session"));
    }

    #[tokio::test]
    async fn another_service_is_refused_before_anything_listens() {
        let mut c = challenge();
        c.service = "turnstile".into();
        assert!(matches!(
            Solver::new(&c).await,
            Err(CaptchaError::Unsupported(s)) if s == "turnstile"
        ));
    }

    #[test]
    fn the_page_escapes_what_it_embeds() {
        let mut c = challenge();
        c.sitekey = "</script><script>alert(1)".into();
        let p = page(&c, "/abc");
        assert!(!p.contains("</script><script>alert(1)"));
        assert!(p.contains(r#"sitekey: "\u003c/script\u003e"#));
    }
}
