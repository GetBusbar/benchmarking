// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// ENGINE v1.2: EVERY NAME RESOLVES TO THE MOCK, AND THE GATEWAY TRUSTS THE MOCK'S HTTPS.
//
// Some gateways pin a real cloud endpoint (HTTPS Bedrock runtime hosts, googleapis.com, the AWS
// SDK's defaults). v1.1 could not answer them, so those cells were declared `untestable` by hand.
// v1.2 answers them without a hostname list:
//
// 1. SETUP KEEPS THE REAL INTERNET. The gateway's image is pulled and its source build runs first,
//    exactly once, before anything below changes.
// 2. THE RUN'S CA IS TRUSTED. The mock (started with `--tls-port 443`) serves its per-run CA at
//    `GET /__mock/ca`. A bundle - the box's system roots plus that CA - is written next to the
//    gateway's manifest. A docker gateway gets it mounted over `/etc/ssl/certs/ca-certificates.crt`
//    and every common trust variable pointed at it; a native gateway gets the variables pointed at
//    the bundle on disk. Applied by `RealLauncher::spawn` from the one process-wide `Trust` set here,
//    so the initial launch and every restart are the same launch.
// 3. THE BOX RESOLVER POINTS AT THE MOCK. The mock (started with `--dns <ip>:53`) answers every A
//    query with its own address. `/etc/resolv.conf` is replaced with `nameserver <mock ip>` before
//    the gateway starts - docker copies the host's resolv.conf into a `--network host` container at
//    create time, and a native gateway reads it directly - and put back when the run ends.
//
// The engine's own traffic is by IP and never resolves a name, so nothing it measures changes. A
// plain-HTTP gateway configured with `http://127.0.0.1:8000` is driven exactly as in v1.1.
//
// `OTB_CATCHALL_DNS=0` skips steps 2 and 3 (a local run with no root, or a mock without the flags);
// it is said on stderr, never silent.
//
// ONLY PROVABLE ON A BOX: the resolver switch needs root (`sudo -n`) and a Linux resolver, and the
// trust mount needs docker. Both are kept to a few obvious lines and verified at run time: the
// responder is queried directly, then the box's own resolver (in-process and through `getent`) must
// answer a made-up name with the mock's address, or the run refuses to launch.

use crate::launch::{LaunchKind, LaunchSpec, Launcher, Mount};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// The trust variables a gateway's TLS stack may read, across the runtimes in the field: OpenSSL and
/// Go (`SSL_CERT_FILE`), Python requests/httpx (`REQUESTS_CA_BUNDLE`), Node (`NODE_EXTRA_CA_CERTS`)
/// and curl (`CURL_CA_BUNDLE`).
///
/// NOT `AWS_CA_BUNDLE` (v1.2.1). The AWS SDK for Go v2 treats that variable as an instruction to add
/// the bundle to the client's transport, and fails outright when a gateway supplies its own
/// `*http.Client` ("unable to add custom RootCAs HTTPClient, has no WithTransportOptions") - which
/// broke gomodel's Bedrock cells in v1.2 although they reach the mock over plain HTTP. The bundle is
/// already mounted over the system trust store, which the AWS SDKs read by default, so the variable
/// added nothing but that failure.
pub const TRUST_VARS: [&str; 4] = [
    "SSL_CERT_FILE",
    "REQUESTS_CA_BUNDLE",
    "NODE_EXTRA_CA_CERTS",
    "CURL_CA_BUNDLE",
];

/// Where a container's system bundle lives, and so where the run's bundle is mounted.
pub const CONTAINER_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";

/// The box's own system roots (Ubuntu), which the run's CA is appended to.
pub const HOST_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";

/// The bundle's file name, beside the gateway's manifest.
pub const BUNDLE_FILE: &str = ".otb-run-ca-bundle.crt";

/// The name the box resolver is asked about to prove the switch took. Never a real host.
pub const PROBE_NAME: &str = "otb-catchall-probe.example.com";

/// The run's trust bundle, as the launcher applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trust {
    /// Absolute path of the bundle on the host.
    pub bundle: String,
}

static TRUST: std::sync::OnceLock<Trust> = std::sync::OnceLock::new();

/// Declare the run's trust bundle. Set once, before the first launch; a second call is ignored, for
/// the same reason `launch::set_commands_dir` ignores one: a restart must not launch differently.
pub fn set_trust(t: Trust) {
    let _ = TRUST.set(t);
}

/// The trust bundle every launch applies, if one was declared.
pub fn trust() -> Option<&'static Trust> {
    TRUST.get()
}

/// `spec` with the run's trust applied: the bundle mounted over the container's system bundle (or,
/// for a native gateway, left on disk) and every `TRUST_VARS` entry pointed at it. Pure.
pub fn apply_trust(spec: &LaunchSpec, trust: &Trust) -> LaunchSpec {
    let mut spec = spec.clone();
    match &mut spec.kind {
        LaunchKind::Docker { env, mounts, .. } => {
            env.extend(
                TRUST_VARS
                    .iter()
                    .map(|k| ((*k).to_string(), CONTAINER_BUNDLE.to_string())),
            );
            mounts.push(Mount {
                host_path: trust.bundle.clone(),
                container_path: CONTAINER_BUNDLE.to_string(),
                read_only: true,
            });
        }
        LaunchKind::Native { env, .. } => {
            env.extend(
                TRUST_VARS
                    .iter()
                    .map(|k| ((*k).to_string(), trust.bundle.clone())),
            );
        }
    }
    spec
}

/// The system roots followed by the run's CA, as one PEM bundle.
pub fn bundle_text(system: &str, ca_pem: &str) -> String {
    let mut out = system.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("# On the Bench mock run CA (engine v1.2)\n");
    out.push_str(ca_pem);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

// ── how a cell reached the mock ─────────────────────────────────────────────────────────────────

/// The mock's TLS request count for `egress`, from its `/__mock/state` document. `None` when the
/// document does not parse or carries no count for it (a v1.1 mock carries none at all).
pub fn tls_count_in(state: &[u8], egress: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_slice(state).ok()?;
    v.get("tls_requests")?.get(egress)?.as_u64()
}

/// Read the mock's TLS request count for `egress` now.
pub fn tls_count(mock: SocketAddr, egress: &str) -> Option<u64> {
    match crate::http::get(mock, "/__mock/state", &[], Duration::from_secs(10)) {
        crate::http::Outcome::Response(r) if r.status == 200 => tls_count_in(r.body(), egress),
        _ => None,
    }
}

/// `upstream_transport` for a cell, from the mock's TLS count for its egress dialect before and
/// after the probe:
/// - `"tls"`: a request on that dialect reached the mock's TLS listener while the cell was probed;
/// - `"http"`: the cell was served and none did, so the gateway reached the plain listener;
/// - `None`: unknown - the count could not be read, or nothing was served and nothing arrived over
///   TLS (there is no hop to describe).
pub fn transport_for(
    before: Option<u64>,
    after: Option<u64>,
    served: bool,
) -> Option<&'static str> {
    match (before, after) {
        (Some(b), Some(a)) if a > b => Some("tls"),
        (Some(_), Some(_)) if served => Some("http"),
        _ => None,
    }
}

// ── the catch-all DNS ───────────────────────────────────────────────────────────────────────────

/// A minimal A query for `name`.
fn a_query(id: u16, name: &str) -> Vec<u8> {
    let mut q = Vec::with_capacity(32 + name.len());
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.split('.') {
        q.push(label.len().min(63) as u8);
        q.extend_from_slice(&label.as_bytes()[..label.len().min(63)]);
    }
    q.extend_from_slice(&[0, 0, 1, 0, 1]);
    q
}

/// The first A record's address in a response to query `id`, if any.
fn first_a(resp: &[u8], id: u16) -> Option<Ipv4Addr> {
    if resp.len() < 12 || resp[0..2] != id.to_be_bytes() || resp[3] & 0x0f != 0 {
        return None;
    }
    let ancount = u16::from_be_bytes([resp[6], resp[7]]);
    let mut i = 12;
    // skip the question
    while *resp.get(i)? != 0 {
        i += 1 + usize::from(*resp.get(i)?);
    }
    i += 5;
    for _ in 0..ancount {
        // name: a pointer (2 bytes) or labels
        if resp.get(i)? & 0xc0 == 0xc0 {
            i += 2;
        } else {
            while *resp.get(i)? != 0 {
                i += 1 + usize::from(*resp.get(i)?);
            }
            i += 1;
        }
        let rtype = u16::from_be_bytes([*resp.get(i)?, *resp.get(i + 1)?]);
        let rdlen = usize::from(u16::from_be_bytes([*resp.get(i + 8)?, *resp.get(i + 9)?]));
        let rdata = resp.get(i + 10..i + 10 + rdlen)?;
        if rtype == 1 && rdlen == 4 {
            return Some(Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]));
        }
        i += 10 + rdlen;
    }
    None
}

/// Ask the responder at `dns` directly for `name`'s A record.
pub fn query_responder(dns: SocketAddr, name: &str) -> Result<Ipv4Addr, String> {
    let sock = UdpSocket::bind(if dns.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })
        .map_err(|e| format!("cannot open a UDP socket: {e}"))?;
    sock.set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let id = 0x07b1;
    sock.send_to(&a_query(id, name), dns)
        .map_err(|e| format!("cannot send to {dns}: {e}"))?;
    let mut buf = [0u8; 512];
    let (n, _) = sock
        .recv_from(&mut buf)
        .map_err(|e| format!("no answer from {dns}: {e}"))?;
    first_a(&buf[..n], id).ok_or_else(|| format!("{dns} answered {name} with no A record"))
}

/// The resolv.conf the box gets while a gateway runs.
pub fn resolv_conf(ip: Ipv4Addr) -> String {
    format!("# engine v1.2 catch-all DNS: every name resolves to the mock; restored at the end of the run\nnameserver {ip}\n")
}

/// Swap `/etc/resolv.conf` for `$1`'s content, keeping the original (usually systemd-resolved's
/// symlink) beside it. The original is kept only if no earlier run's is already there, so a crashed
/// run's leftover is never mistaken for the box's own.
const ENGAGE: &str = r#"set -e
saved=/etc/resolv.conf.otb-saved
if [ ! -e "$saved" ] && [ ! -L "$saved" ]; then mv /etc/resolv.conf "$saved"; fi
printf '%s' "$1" > /etc/resolv.conf.otb-new
chmod 644 /etc/resolv.conf.otb-new
mv -f /etc/resolv.conf.otb-new /etc/resolv.conf"#;

const RESTORE: &str = r#"set -e
saved=/etc/resolv.conf.otb-saved
if [ -e "$saved" ] || [ -L "$saved" ]; then mv -f "$saved" /etc/resolv.conf; fi"#;

fn sudo_sh(script: &str, arg: &str) -> Result<(), String> {
    let out = Command::new("sudo")
        .args(["-n", "sh", "-c", script, "sh", arg])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run sudo: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "sudo -n sh exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// The box resolver, switched to the mock. Restored by `restore` or, failing that, on drop.
#[derive(Debug)]
pub struct Isolation {
    restored: bool,
}

impl Isolation {
    /// Put the box's own resolver back. Idempotent.
    pub fn restore(&mut self) -> Result<(), String> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        sudo_sh(RESTORE, "")
    }
}

impl Drop for Isolation {
    fn drop(&mut self) {
        if let Err(e) = self.restore() {
            eprintln!("egress: could not restore /etc/resolv.conf: {e}");
        }
    }
}

/// Does the box resolver now answer a made-up name with `ip`? In this process (the engine's own
/// resolver reads resolv.conf) and through `getent` (glibc, which every native gateway and most
/// container images use) when it exists.
fn verify_box_resolver(ip: Ipv4Addr) -> Result<(), String> {
    use std::net::ToSocketAddrs;
    let got: Vec<std::net::IpAddr> = (PROBE_NAME, 443)
        .to_socket_addrs()
        .map_err(|e| format!("{PROBE_NAME} did not resolve: {e}"))?
        .map(|a| a.ip())
        .collect();
    if got.is_empty() || got.iter().any(|a| *a != std::net::IpAddr::V4(ip)) {
        return Err(format!("{PROBE_NAME} resolved to {got:?}, not only {ip}"));
    }
    if let Ok(out) = Command::new("getent")
        .args(["ahostsv4", PROBE_NAME])
        .stdin(Stdio::null())
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        let first = text.split_whitespace().next().unwrap_or("");
        if !out.status.success() || first != ip.to_string() {
            return Err(format!(
                "glibc (getent ahostsv4 {PROBE_NAME}) answered {:?}, not {ip}: the box's name service \
                 does not go through /etc/resolv.conf",
                text.trim()
            ));
        }
    }
    Ok(())
}

/// Setup that needs the real internet: the image (docker) and the source build (pre-launch step),
/// once. The build is then removed from `spec`, so the launch that follows - and every restart, see
/// `suite.rs` - never repeats it under the catch-all DNS.
pub fn setup_with_internet(
    launcher: &mut impl Launcher,
    spec: &mut LaunchSpec,
) -> Result<(), String> {
    if let LaunchKind::Docker { image, .. } = &spec.kind {
        let present = Command::new("docker")
            .args(["image", "inspect", "--format", "{{.Id}}", image])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !present {
            let out = Command::new("docker")
                .args(["pull", image])
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("cannot run docker pull: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "docker pull {image} failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        }
    }
    if let Some(step) = spec.pre_launch.take() {
        launcher
            .run_pre_launch(&step)
            .map_err(|why| format!("pre-launch step {:?} failed: {why}", step.command))?;
    }
    Ok(())
}

/// Steps 2 and 3 of the module header, after `setup_with_internet`. `Ok(None)` when switched off by
/// `OTB_CATCHALL_DNS=0`.
pub fn isolate(mock: SocketAddr, gw_dir: &Path) -> Result<Option<Isolation>, String> {
    if std::env::var("OTB_CATCHALL_DNS").is_ok_and(|v| v.trim() == "0") {
        eprintln!("egress: OTB_CATCHALL_DNS=0 - no catch-all DNS and no run CA; the gateway keeps the box resolver (v1.1 behaviour)");
        return Ok(None);
    }
    let std::net::IpAddr::V4(ip) = mock.ip() else {
        return Err(format!(
            "the mock address {mock} is not IPv4, and the catch-all DNS answers A records only"
        ));
    };
    // The run's CA, from the mock.
    let ca_pem = match crate::http::get(mock, "/__mock/ca", &[], Duration::from_secs(10)) {
        crate::http::Outcome::Response(r)
            if r.status == 200
                && String::from_utf8_lossy(r.body()).contains("BEGIN CERTIFICATE") =>
        {
            String::from_utf8_lossy(r.body()).into_owned()
        }
        crate::http::Outcome::Response(r) => {
            return Err(format!(
                "the mock serves no run CA (GET /__mock/ca answered HTTP {}); start it with --tls-port 443 --dns {ip}:53, or set OTB_CATCHALL_DNS=0",
                r.status
            ))
        }
        other => return Err(format!("the mock's /__mock/ca could not be read: {other:?}")),
    };
    let system = std::fs::read_to_string(HOST_BUNDLE)
        .map_err(|e| format!("cannot read the box's system roots {HOST_BUNDLE}: {e}"))?;
    let bundle: PathBuf = gw_dir.join(BUNDLE_FILE);
    std::fs::write(&bundle, bundle_text(&system, &ca_pem))
        .map_err(|e| format!("cannot write {}: {e}", bundle.display()))?;
    let bundle = std::fs::canonicalize(&bundle).unwrap_or(bundle);
    set_trust(Trust {
        bundle: bundle.to_string_lossy().into_owned(),
    });
    // The responder, asked directly, before the box depends on it.
    let dns = SocketAddr::from((ip, 53));
    match query_responder(dns, PROBE_NAME) {
        Ok(a) if a == ip => {}
        Ok(a) => return Err(format!("the DNS responder at {dns} answered {a}, not the mock's {ip}")),
        Err(e) => {
            return Err(format!(
                "the mock's DNS responder at {dns} did not answer ({e}); start the mock with --dns {dns}, or set OTB_CATCHALL_DNS=0"
            ))
        }
    }
    sudo_sh(ENGAGE, &resolv_conf(ip))
        .map_err(|e| format!("could not switch /etc/resolv.conf: {e}"))?;
    let mut iso = Isolation { restored: false };
    if let Err(e) = verify_box_resolver(ip) {
        let _ = iso.restore();
        return Err(format!(
            "the box resolver did not take the catch-all DNS: {e}"
        ));
    }
    eprintln!(
        "egress: every name now resolves to the mock ({ip}); the gateway trusts the run CA via {}",
        bundle.display()
    );
    Ok(Some(iso))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::PortMapping;
    use crate::manifest::Runtime;

    fn spec(kind: LaunchKind) -> LaunchSpec {
        LaunchSpec {
            runtime: Runtime::Native {
                proc_match: "x".into(),
            },
            kind,
            cores: "0-3".into(),
            port: 3000,
            ready_budget: Duration::from_secs(1),
            boot_backoff: Duration::from_secs(1),
            pre_launch: None,
        }
    }

    #[test]
    fn a_docker_gateway_gets_the_bundle_mounted_over_its_system_roots_and_every_trust_var() {
        let s = spec(LaunchKind::Docker {
            image: "img".into(),
            env: vec![("A".into(), "1".into())],
            port: PortMapping::Host,
            mounts: vec![],
            command: vec![],
        });
        let t = Trust {
            bundle: "/home/ubuntu/b.crt".into(),
        };
        let LaunchKind::Docker { env, mounts, .. } = apply_trust(&s, &t).kind else {
            panic!("still docker");
        };
        assert_eq!(
            env[0],
            ("A".to_string(), "1".to_string()),
            "the gateway's own env is kept"
        );
        for k in TRUST_VARS {
            assert!(
                env.contains(&(k.to_string(), CONTAINER_BUNDLE.to_string())),
                "{k}"
            );
        }
        // v1.2.1: AWS_CA_BUNDLE breaks the AWS SDK for Go v2 behind a custom http.Client (gomodel's
        // Bedrock cells); the mounted system bundle already covers the AWS SDKs.
        assert!(
            !env.iter().any(|(k, _)| k == "AWS_CA_BUNDLE"),
            "AWS_CA_BUNDLE must not be injected"
        );
        assert_eq!(
            mounts,
            vec![Mount {
                host_path: "/home/ubuntu/b.crt".into(),
                container_path: CONTAINER_BUNDLE.into(),
                read_only: true
            }]
        );
        let inv = crate::launch::build_invocation(&apply_trust(&s, &t));
        assert!(inv
            .args
            .contains(&"/home/ubuntu/b.crt:/etc/ssl/certs/ca-certificates.crt:ro".to_string()));
    }

    #[test]
    fn a_native_gateway_gets_every_trust_var_pointed_at_the_bundle_on_disk() {
        let s = spec(LaunchKind::Native {
            binary: "/bin/gw".into(),
            args: vec![],
            env: vec![],
            env_unset: vec![],
        });
        let t = Trust {
            bundle: "/x/b.crt".into(),
        };
        let LaunchKind::Native { env, .. } = apply_trust(&s, &t).kind else {
            panic!("still native");
        };
        assert_eq!(env.len(), TRUST_VARS.len());
        assert!(env.iter().all(|(_, v)| v == "/x/b.crt"));
    }

    #[test]
    fn the_bundle_is_the_system_roots_then_the_run_ca() {
        let b = bundle_text(
            "SYS-A\nSYS-B",
            "-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----",
        );
        assert!(b.starts_with("SYS-A\nSYS-B\n"));
        assert!(b.ends_with("-----END CERTIFICATE-----\n"));
    }

    #[test]
    fn upstream_transport_is_tls_only_when_a_tls_request_arrived_on_the_egress_dialect() {
        assert_eq!(transport_for(Some(3), Some(4), true), Some("tls"));
        assert_eq!(
            transport_for(Some(3), Some(4), false),
            Some("tls"),
            "a refused TLS hop is still a TLS hop"
        );
        assert_eq!(transport_for(Some(3), Some(3), true), Some("http"));
        assert_eq!(
            transport_for(Some(3), Some(3), false),
            None,
            "nothing served, nothing arrived"
        );
        assert_eq!(
            transport_for(None, Some(3), true),
            None,
            "an unreadable count says nothing"
        );
        assert_eq!(transport_for(Some(1), None, true), None);
    }

    #[test]
    fn the_tls_count_is_read_off_the_mocks_state_document() {
        let doc = br#"{"recording":false,"dialects":{},"tls_requests":{"bedrock":5,"openai":0}}"#;
        assert_eq!(tls_count_in(doc, "bedrock"), Some(5));
        assert_eq!(tls_count_in(doc, "openai"), Some(0));
        // A v1.1 mock's document has no TLS counts, which is unknown, not zero.
        assert_eq!(
            tls_count_in(br#"{"recording":false,"dialects":{}}"#, "bedrock"),
            None
        );
        assert_eq!(tls_count_in(b"garbage", "bedrock"), None);
    }

    #[test]
    fn the_resolver_config_names_only_the_mock() {
        let c = resolv_conf(Ipv4Addr::LOCALHOST);
        let ns: Vec<&str> = c.lines().filter(|l| l.starts_with("nameserver")).collect();
        assert_eq!(ns, vec!["nameserver 127.0.0.1"]);
        assert!(
            !c.contains("search"),
            "no search domains: every name goes to the mock as asked"
        );
    }

    // The engine's preflight query against a stand-in responder that answers like the mock's.
    #[test]
    fn the_preflight_query_reads_the_responders_a_record() {
        let server = UdpSocket::bind("127.0.0.1:0").expect("bind");
        let addr = server.local_addr().expect("addr");
        std::thread::spawn(move || {
            let mut buf = [0u8; 512];
            let Ok((n, peer)) = server.recv_from(&mut buf) else {
                return;
            };
            let q = &buf[..n];
            let mut r = q[..2].to_vec();
            r.extend_from_slice(&[0x85, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]);
            r.extend_from_slice(&q[12..]);
            r.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 10, 1, 2, 3]);
            let _ = server.send_to(&r, peer);
        });
        assert_eq!(
            query_responder(addr, PROBE_NAME),
            Ok(Ipv4Addr::new(10, 1, 2, 3))
        );
    }
}
