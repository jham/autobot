//! The Home Assistant Supervisor API, used when autobot runs as an add-on.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::config::MqttConfig;

#[derive(Deserialize)]
struct Reply<T> {
    result: String,
    data: Option<T>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct MqttService {
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
}

/// Undo `Transfer-Encoding: chunked`.
fn dechunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = body.windows(2).position(|w| w == b"\r\n")?;
        let size = usize::from_str_radix(
            std::str::from_utf8(&body[..line_end])
                .ok()?
                .split(';')
                .next()?
                .trim(),
            16,
        )
        .ok()?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(body.get(..size)?);
        body = body.get(size + 2..)?;
    }
}

/// Ask the Supervisor for the MQTT broker's address and credentials (needs `services: mqtt:need`).
pub async fn mqtt_service(token: &str) -> Result<MqttConfig> {
    let mut s = TcpStream::connect("supervisor:80")
        .await
        .context("connecting to the Supervisor")?;
    let req = format!(
        "GET /services/mqtt HTTP/1.1\r\nHost: supervisor\r\nAuthorization: Bearer {token}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).await?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("bad HTTP reply from the Supervisor")?;
    let (head, body) = (
        String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase(),
        &raw[split + 4..],
    );
    let body = if head.contains("transfer-encoding: chunked") {
        dechunk(body).context("bad chunked reply from the Supervisor")?
    } else {
        body.to_vec()
    };
    let reply: Reply<MqttService> =
        serde_json::from_slice(&body).context("parsing the Supervisor's MQTT reply")?;
    let Some(svc) = reply.data.filter(|_| reply.result == "ok") else {
        bail!(
            "no MQTT service from the Supervisor ({}); is the Mosquitto add-on installed?",
            reply.message.unwrap_or_default()
        );
    };
    Ok(MqttConfig {
        host: svc.host,
        port: svc.port,
        username: svc.username,
        password: svc.password,
        discovery_prefix: "homeassistant".into(),
        base_topic: "autobot".into(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn dechunks() {
        assert_eq!(
            super::dechunk(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n").as_deref(),
            Some(&b"Wikipedia"[..])
        );
    }
}
