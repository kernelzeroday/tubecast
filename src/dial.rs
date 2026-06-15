use anyhow::{Context, Result};
use std::collections::HashSet;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::Instant;
use youtube_lounge_rs::LoungeClient;

const SSDP_ADDR: &str = "239.255.255.250:1900";

const M_SEARCH: &str = "\
M-SEARCH * HTTP/1.1\r\n\
HOST: 239.255.255.250:1900\r\n\
MAN: \"ssdp:discover\"\r\n\
ST: urn:dial-multiscreen-org:service:dial:1\r\n\
MX: 3\r\n\
\r\n";

pub struct DialDevice {
    pub name: String,
    pub screen_id: String,
    pub lounge_token: String,
    pub location: String,
}

/// Discover YouTube-capable devices on the LAN via SSDP + DIAL.
pub async fn discover(timeout: Duration) -> Result<Vec<DialDevice>> {
    let locations = ssdp_search(timeout).await?;
    let mut devices = Vec::new();
    for loc in &locations {
        if let Ok(Some(dev)) = probe_dial(loc).await {
            devices.push(dev);
        }
    }
    Ok(devices)
}

async fn ssdp_search(timeout: Duration) -> Result<Vec<String>> {
    let sock = UdpSocket::bind("0.0.0.0:0").await.context("bind UDP socket")?;
    let addr: std::net::SocketAddr = SSDP_ADDR.parse().unwrap();

    sock.send_to(M_SEARCH.as_bytes(), addr).await.context("send M-SEARCH")?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = sock.send_to(M_SEARCH.as_bytes(), addr).await;

    let mut locations = Vec::new();
    let mut seen = HashSet::new();
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 4096];

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, sock.recv_from(&mut buf)).await {
            Ok(Ok((len, _))) => {
                let resp = String::from_utf8_lossy(&buf[..len]);
                if let Some(loc) = parse_header(&resp, "LOCATION") {
                    if seen.insert(loc.clone()) {
                        locations.push(loc);
                    }
                }
            }
            _ => break,
        }
    }
    Ok(locations)
}

/// Probe a single DIAL device: fetch its device description, then query the
/// YouTube app endpoint for screenId and loungeToken.
async fn probe_dial(location: &str) -> Result<Option<DialDevice>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let resp = client.get(location).send().await?;
    let app_url = resp
        .headers()
        .get("Application-URL")
        .or_else(|| resp.headers().get("application-url"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_end_matches('/').to_string());
    let body = resp.text().await?;

    let app_url = match app_url {
        Some(u) => u,
        None => return Ok(None),
    };
    let name = xml_text(&body, "friendlyName").unwrap_or_default();

    let yt_url = format!("{app_url}/YouTube");
    let yt_resp = client
        .get(&yt_url)
        .header("Origin", "https://www.youtube.com")
        .send()
        .await;
    let yt_body = match yt_resp {
        Ok(r) if r.status().is_success() => r.text().await.unwrap_or_default(),
        _ => return Ok(None),
    };

    let screen_id = match xml_text(&yt_body, "screenId") {
        Some(id) if !id.is_empty() => id,
        _ => return Ok(None),
    };

    // The DIAL token is the TV's own session token. Get a proper remote-control
    // token by refreshing via the YouTube API with the discovered screen_id.
    let lounge_token = match LoungeClient::refresh_lounge_token(&screen_id).await {
        Ok(screen) => screen.lounge_token,
        Err(_) => {
            match xml_text(&yt_body, "loungeToken") {
                Some(t) if !t.is_empty() => t,
                _ => return Ok(None),
            }
        }
    };

    Ok(Some(DialDevice {
        name,
        screen_id,
        lounge_token,
        location: location.to_string(),
    }))
}

fn parse_header(response: &str, name: &str) -> Option<String> {
    let needle = format!("{name}:");
    for line in response.lines() {
        let trimmed = line.trim();
        if trimmed.len() > needle.len()
            && trimmed[..needle.len()].eq_ignore_ascii_case(&needle)
        {
            return Some(trimmed[needle.len()..].trim().to_string());
        }
    }
    None
}

fn xml_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let text = xml[start..end].trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(decode_xml_entities(&text))
    }
}

fn decode_xml_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ssdp_location() {
        let resp = "HTTP/1.1 200 OK\r\n\
                    LOCATION: http://192.168.1.42:8888/dial/abc/dd.xml\r\n\
                    ST: urn:dial-multiscreen-org:service:dial:1\r\n\r\n";
        assert_eq!(
            parse_header(resp, "LOCATION"),
            Some("http://192.168.1.42:8888/dial/abc/dd.xml".into())
        );
    }

    #[test]
    fn parse_header_case_insensitive() {
        let resp = "location: http://example.com\r\n";
        assert_eq!(
            parse_header(resp, "LOCATION"),
            Some("http://example.com".into())
        );
    }

    #[test]
    fn parse_header_missing() {
        assert_eq!(parse_header("HTTP/1.1 200 OK\r\n", "LOCATION"), None);
    }

    #[test]
    fn extract_xml_tags() {
        let xml = "<root><screenId>abc123</screenId><loungeToken>tok</loungeToken></root>";
        assert_eq!(xml_text(xml, "screenId"), Some("abc123".into()));
        assert_eq!(xml_text(xml, "loungeToken"), Some("tok".into()));
        assert_eq!(xml_text(xml, "missing"), None);
    }

    #[test]
    fn xml_text_empty_returns_none() {
        assert_eq!(xml_text("<screenId></screenId>", "screenId"), None);
    }

    #[test]
    fn xml_text_decodes_entities() {
        let xml = "<friendlyName>Playlet on 50&quot; TCL Roku TV</friendlyName>";
        assert_eq!(
            xml_text(xml, "friendlyName"),
            Some("Playlet on 50\" TCL Roku TV".into())
        );
    }

    #[test]
    fn decodes_all_xml_entities() {
        assert_eq!(decode_xml_entities("a&amp;b"), "a&b");
        assert_eq!(decode_xml_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_xml_entities("&quot;hi&quot;"), "\"hi\"");
        assert_eq!(decode_xml_entities("it&apos;s"), "it's");
    }

    #[test]
    fn xml_text_trims_whitespace() {
        assert_eq!(
            xml_text("<friendlyName>  My TV  </friendlyName>", "friendlyName"),
            Some("My TV".into())
        );
    }

    #[test]
    fn parses_full_dial_youtube_response() {
        let xml = r#"<?xml version="1.0"?>
<service dialVer="1.7" xmlns="urn:dial-multiscreen-org:schemas:dial">
  <name>YouTube</name>
  <options allowStop="true"/>
  <state>running</state>
  <link rel="run" href="run"/>
  <additionalData>
    <theme>cl</theme>
    <deviceId>XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX</deviceId>
    <screenId>screen123abc</screenId>
    <loungeToken>AGdO5p_token_here</loungeToken>
  </additionalData>
</service>"#;
        assert_eq!(xml_text(xml, "screenId"), Some("screen123abc".into()));
        assert_eq!(xml_text(xml, "loungeToken"), Some("AGdO5p_token_here".into()));
        assert_eq!(xml_text(xml, "state"), Some("running".into()));
    }
}
