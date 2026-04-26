//! LAN address detection + QR code generation for the join URL.
//!
//! Inside Docker the container's IP isn't reachable from phones on the same
//! Wi-Fi. The launcher script passes the host's LAN IP via `ADVENTURER_LAN_IP`;
//! if unset, we fall back to `local-ip-address` (works for native runs).

use std::net::IpAddr;

use anyhow::{Context, Result};
use qrcode::{render::svg, QrCode};

/// Returns the IP a phone on the same LAN should use to reach this server.
/// Priority:
///   1. `ADVENTURER_LAN_IP` env var (launcher injects this)
///   2. local-ip-address crate (first non-loopback IPv4)
///   3. 127.0.0.1 (last resort, useful for local-only testing)
pub fn detect_lan_ip() -> IpAddr {
    if let Ok(s) = std::env::var("ADVENTURER_LAN_IP") {
        if let Ok(ip) = s.parse::<IpAddr>() {
            return ip;
        }
        tracing::warn!(env = %s, "ADVENTURER_LAN_IP not parseable; falling back");
    }
    match local_ip_address::local_ip() {
        Ok(ip) => ip,
        Err(e) => {
            tracing::warn!(?e, "local_ip detection failed; using 127.0.0.1");
            "127.0.0.1".parse().unwrap()
        }
    }
}

/// Build the URL the QR code encodes. If `ADVENTURER_PUBLIC_URL` is set
/// (e.g. `https://adventurer.superterran.net`) it's used verbatim with `/join`
/// appended — useful when the server is fronted by a Cloudflare Tunnel /
/// reverse proxy that provides HTTPS (which iPad Safari and others require
/// for `getUserMedia()` / mic access). Otherwise we fall back to the
/// detected LAN IP + port.
pub fn join_url(ip: IpAddr, port: u16) -> String {
    if let Ok(public) = std::env::var("ADVENTURER_PUBLIC_URL") {
        let trimmed = public.trim_end_matches('/');
        return format!("{trimmed}/join");
    }
    format!("http://{ip}:{port}/join")
}

/// Render a QR encoding `payload` as SVG. Sized for the modal — ~320 px.
pub fn qr_svg(payload: &str) -> Result<String> {
    let code = QrCode::new(payload.as_bytes()).context("encode QR")?;
    let svg = code
        .render::<svg::Color>()
        .min_dimensions(320, 320)
        .quiet_zone(true)
        .dark_color(svg::Color("#0e1116"))
        .light_color(svg::Color("#ffffff"))
        .build();
    Ok(svg)
}
