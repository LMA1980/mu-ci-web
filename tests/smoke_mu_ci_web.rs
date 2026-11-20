#![cfg(not(target_arch = "wasm32"))]
#![deny(unsafe_code)]

use mu_ci_web::ansi::{parse_ansi_spans, strip_ansi_codes};
use mu_ci_web::log_parser::{parse_job_log, StepStatus};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
struct SmokeConfig {
    target_url: Option<String>,
    bearer_token: Option<String>,
    tls_ca_pem: Option<Vec<u8>>,
    tls_cert_pem: Option<Vec<u8>>,
    tls_key_pem: Option<Vec<u8>>,
    timeout: Duration,
}

fn read_trimmed_secret_file(var_name: &str, path_raw: &str) -> Result<String, String> {
    let path = path_raw.trim();
    if path.is_empty() {
        return Err(format!("{var_name} was set to an empty path"));
    }
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {var_name} at '{path}': {e}"))?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err(format!("{var_name} at '{path}' is empty"));
    }
    Ok(trimmed)
}

fn read_non_empty_file_bytes(var_name: &str, path_raw: &str) -> Result<Vec<u8>, String> {
    let path = path_raw.trim();
    if path.is_empty() {
        return Err(format!("{var_name} was set to an empty path"));
    }
    let bytes =
        fs::read(path).map_err(|e| format!("Failed to read {var_name} at '{path}': {e}"))?;
    if bytes.is_empty() {
        return Err(format!("{var_name} at '{path}' is empty"));
    }
    Ok(bytes)
}

fn load_smoke_config_with<F>(get_env: F) -> Result<SmokeConfig, String>
where
    F: Fn(&str) -> Option<String>,
{
    let target_url = get_env("MU_SMOKE_TARGET_URL")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let bearer_token = match get_env("MU_SMOKE_BEARER_TOKEN_FILE")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(path) => Some(read_trimmed_secret_file(
            "MU_SMOKE_BEARER_TOKEN_FILE",
            &path,
        )?),
        None => None,
    };

    let tls_ca_pem = match get_env("MU_SMOKE_TLS_CA_FILE")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(path) => Some(read_non_empty_file_bytes("MU_SMOKE_TLS_CA_FILE", &path)?),
        None => None,
    };

    let cert_path = get_env("MU_SMOKE_TLS_CERT_FILE")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let key_path = get_env("MU_SMOKE_TLS_KEY_FILE")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let (tls_cert_pem, tls_key_pem) = match (cert_path, key_path) {
        (Some(cert), Some(key)) => (
            Some(read_non_empty_file_bytes("MU_SMOKE_TLS_CERT_FILE", &cert)?),
            Some(read_non_empty_file_bytes("MU_SMOKE_TLS_KEY_FILE", &key)?),
        ),
        (Some(_), None) | (None, Some(_)) => {
            return Err(
                "MU_SMOKE_TLS_CERT_FILE and MU_SMOKE_TLS_KEY_FILE must both be set together"
                    .to_string(),
            );
        }
        (None, None) => (None, None),
    };

    let timeout_secs = match get_env("MU_SMOKE_TIMEOUT_SECS")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(raw) => {
            let secs = raw.parse::<u64>().map_err(|e| {
                format!("Invalid MU_SMOKE_TIMEOUT_SECS '{raw}': expected positive integer ({e})")
            })?;
            if secs == 0 {
                return Err("MU_SMOKE_TIMEOUT_SECS must be greater than zero".to_string());
            }
            secs
        }
        None => 10,
    };

    Ok(SmokeConfig {
        target_url,
        bearer_token,
        tls_ca_pem,
        tls_cert_pem,
        tls_key_pem,
        timeout: Duration::from_secs(timeout_secs),
    })
}

fn load_smoke_config() -> Result<SmokeConfig, String> {
    load_smoke_config_with(|k| std::env::var(k).ok())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedAssets {
    css_href: String,
    js_src: String,
    wasm_src: String,
    version_tag: String,
}

fn extract_quoted_ref(html: &str, needle: &str) -> Result<String, String> {
    let start = html
        .find(needle)
        .ok_or_else(|| format!("dist/index.html missing expected reference '{needle}'"))?;
    let rest = &html[start..];
    let end = rest
        .find(['"', '\''])
        .ok_or_else(|| format!("Unterminated attribute quote after '{needle}'"))?;
    Ok(rest[..end].to_string())
}

fn parse_hashed_assets(index_html: &str) -> Result<ParsedAssets, String> {
    let css_href = extract_quoted_ref(index_html, "./style.css?v=")?;
    let js_src = extract_quoted_ref(index_html, "./mu-ci-web.js?v=")?;
    let wasm_src = extract_quoted_ref(index_html, "./mu-ci-web_bg.wasm?v=")?;

    let css_ver = css_href
        .split("?v=")
        .nth(1)
        .ok_or_else(|| "style.css missing ?v= hash".to_string())?;
    let js_ver = js_src
        .split("?v=")
        .nth(1)
        .ok_or_else(|| "mu-ci-web.js missing ?v= hash".to_string())?;
    let wasm_ver = wasm_src
        .split("?v=")
        .nth(1)
        .ok_or_else(|| "mu-ci-web_bg.wasm missing ?v= hash".to_string())?;

    if css_ver.is_empty() || css_ver != js_ver || js_ver != wasm_ver {
        return Err(format!(
            "Mismatched cache-busting asset hashes in index.html: css={css_ver}, js={js_ver}, wasm={wasm_ver}"
        ));
    }
    if !wasm_ver.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "Expected hexadecimal cache-busting hash in index.html, got '{wasm_ver}'"
        ));
    }
    if !index_html.contains("no-cache, no-store, must-revalidate") {
        return Err("index.html missing no-cache, no-store, must-revalidate meta tag".to_string());
    }

    let version_tag = wasm_ver.to_string();
    Ok(ParsedAssets {
        css_href,
        js_src,
        wasm_src,
        version_tag,
    })
}

fn resolve_dist_dir() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("MU_SMOKE_DIST_DIR") {
        let p = PathBuf::from(explicit.trim());
        if p.is_dir() {
            return Some(p);
        }
    }
    let manifest_dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("dist");
    if manifest_dist.is_dir() {
        return Some(manifest_dist);
    }
    let cwd_dist = PathBuf::from("dist");
    if cwd_dist.is_dir() {
        return Some(cwd_dist);
    }
    None
}

fn verify_dist_dir_integrity(dist_dir: &Path) -> Result<ParsedAssets, String> {
    let index_path = dist_dir.join("index.html");
    let html = fs::read_to_string(&index_path)
        .map_err(|e| format!("Failed reading {}: {e}", index_path.display()))?;
    let assets = parse_hashed_assets(&html)?;

    for (rel_ref, expected_wasm) in [
        (&assets.css_href, false),
        (&assets.js_src, false),
        (&assets.wasm_src, true),
    ] {
        let file_part = rel_ref
            .trim_start_matches("./")
            .split('?')
            .next()
            .unwrap_or("");
        let asset_path = dist_dir.join(file_part);
        let bytes = fs::read(&asset_path).map_err(|e| {
            format!(
                "Missing referenced dist asset {}: {e}",
                asset_path.display()
            )
        })?;
        if bytes.is_empty() {
            return Err(format!("Dist asset {} is empty", asset_path.display()));
        }
        if expected_wasm && !bytes.starts_with(b"\x00asm\x01\x00\x00\x00") {
            return Err(format!(
                "Dist WASM asset {} lacks valid \\0asm v1 magic header",
                asset_path.display()
            ));
        }
    }

    Ok(assets)
}

struct HttpResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn parse_http_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        format!("Unsupported URL scheme in stdlib smoke client: '{url}' (expected http://)")
    })?;
    let (authority, base_path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port) = if let Some((h, p)) = authority.rsplit_once(':') {
        let port_num = p
            .parse::<u16>()
            .map_err(|e| format!("Invalid TCP port in URL '{url}': {e}"))?;
        (h.to_string(), port_num)
    } else {
        (authority.to_string(), 80)
    };
    Ok((host, port, base_path.trim_end_matches('/').to_string()))
}

fn http_get(
    host: &str,
    port: u16,
    path: &str,
    bearer_token: Option<&str>,
    timeout: Duration,
) -> Result<HttpResponse, String> {
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|e| format!("Invalid socket address '{host}:{port}': {e}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| format!("Failed connecting to {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("Failed setting read timeout: {e}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| format!("Failed setting write timeout: {e}"))?;

    let mut req = format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n");
    if let Some(tok) = bearer_token {
        req.push_str(&format!("Authorization: Bearer {tok}\r\n"));
    }
    req.push_str("\r\n");
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("Failed writing HTTP request to {path}: {e}"))?;
    stream
        .flush()
        .map_err(|e| format!("Failed flushing HTTP request to {path}: {e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("Failed reading HTTP response from {path}: {e}"))?;

    let split_pos = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| format!("Malformed HTTP response from {path}: missing CRLFCRLF"))?;
    let head = String::from_utf8_lossy(&raw[..split_pos]);
    let mut lines = head.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| "Empty HTTP response status line".to_string())?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("Invalid HTTP status line '{status_line}'"))?;

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    Ok(HttpResponse {
        status,
        headers,
        body: raw[split_pos + 4..].to_vec(),
    })
}

struct EphemeralWebServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl EphemeralWebServer {
    fn start(dist_dir: PathBuf) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("Failed binding ephemeral listener: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("Failed configuring nonblocking listener: {e}"))?;
        let addr = listener
            .local_addr()
            .map_err(|e| format!("Failed reading local_addr: {e}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);

        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req_text = String::from_utf8_lossy(&buf[..n]);
                        let raw_path = req_text
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/");
                        let clean_path = raw_path.split('?').next().unwrap_or("/");
                        let (rel_file, content_type, cache_control) = match clean_path {
                            "/" | "/index.html" => (
                                "index.html",
                                "text/html; charset=utf-8",
                                "no-cache, no-store, must-revalidate",
                            ),
                            "/mu-ci-web.js" => (
                                "mu-ci-web.js",
                                "application/javascript; charset=utf-8",
                                "no-cache, must-revalidate",
                            ),
                            "/mu-ci-web_bg.wasm" => (
                                "mu-ci-web_bg.wasm",
                                "application/wasm",
                                "no-cache, must-revalidate",
                            ),
                            "/style.css" => (
                                "style.css",
                                "text/css; charset=utf-8",
                                "no-cache, must-revalidate",
                            ),
                            _ => ("", "text/plain; charset=utf-8", "no-store"),
                        };

                        if rel_file.is_empty() {
                            let resp = "HTTP/1.1 404 Not Found\r\nCache-Control: no-store\r\nContent-Length: 9\r\nConnection: close\r\n\r\nNot Found";
                            let _ = stream.write_all(resp.as_bytes());
                        } else if let Ok(body) = fs::read(dist_dir.join(rel_file)) {
                            let hdr = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nCache-Control: {cache_control}\r\nX-Content-Type-Options: nosniff\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(hdr.as_bytes());
                            let _ = stream.write_all(&body);
                        } else {
                            let resp = "HTTP/1.1 404 Not Found\r\nCache-Control: no-store\r\nContent-Length: 9\r\nConnection: close\r\n\r\nNot Found";
                            let _ = stream.write_all(resp.as_bytes());
                        }
                        let _ = stream.flush();
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            addr,
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for EphemeralWebServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(100));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn assert_cache_control_tokens(header_val: &str, required: &[&str], path: &str) {
    let lower = header_val.to_ascii_lowercase();
    for token in required {
        assert!(
            lower.contains(token),
            "Expected Cache-Control for '{path}' to contain '{token}', got '{header_val}'"
        );
    }
}

#[test]
fn smoke_mu_ci_web_dist_and_http_cache_headers() {
    let cfg = load_smoke_config().expect("Smoke environment configuration must be valid");

    if let Some(dist_dir) = resolve_dist_dir() {
        let assets = verify_dist_dir_integrity(&dist_dir)
            .expect("Local dist/index.html and hashed assets must be valid");
        assert!(!assets.version_tag.is_empty());
    }

    let (base_url, _server_guard) = if let Some(ref target) = cfg.target_url {
        (target.clone(), None)
    } else {
        let dist_dir = resolve_dist_dir()
            .expect("dist/ directory must exist in repository for ephemeral local smoke server");
        let server = EphemeralWebServer::start(dist_dir)
            .expect("Ephemeral stdlib HTTP server must bind 127.0.0.1:0");
        (format!("http://{}", server.addr), Some(server))
    };

    let (host, port, base_path) =
        parse_http_url(&base_url).expect("Valid http:// target URL required");
    let root_path = if base_path.is_empty() {
        "/".to_string()
    } else {
        format!("{base_path}/")
    };

    let root_resp = http_get(
        &host,
        port,
        &root_path,
        cfg.bearer_token.as_deref(),
        cfg.timeout,
    )
    .expect("GET / must succeed");
    assert_eq!(root_resp.status, 200, "GET / must return 200 OK");
    let root_cc = root_resp
        .headers
        .get("cache-control")
        .expect("GET / response must include Cache-Control header");
    assert_cache_control_tokens(root_cc, &["no-cache", "no-store", "must-revalidate"], "/");

    let html = String::from_utf8(root_resp.body).expect("index.html response must be UTF-8");
    let assets =
        parse_hashed_assets(&html).expect("Served index.html must reference hashed assets");

    let js_path = format!("{base_path}/{}", assets.js_src.trim_start_matches("./"));
    let js_resp = http_get(
        &host,
        port,
        &js_path,
        cfg.bearer_token.as_deref(),
        cfg.timeout,
    )
    .expect("GET hashed JS must succeed");
    assert_eq!(js_resp.status, 200);
    assert!(!js_resp.body.is_empty());
    let js_cc = js_resp
        .headers
        .get("cache-control")
        .expect("Hashed JS response must include Cache-Control header");
    assert_cache_control_tokens(js_cc, &["no-cache", "must-revalidate"], &js_path);

    let wasm_path = format!("{base_path}/{}", assets.wasm_src.trim_start_matches("./"));
    let wasm_resp = http_get(
        &host,
        port,
        &wasm_path,
        cfg.bearer_token.as_deref(),
        cfg.timeout,
    )
    .expect("GET hashed WASM must succeed");
    assert_eq!(wasm_resp.status, 200);
    assert!(
        wasm_resp.body.starts_with(b"\x00asm\x01\x00\x00\x00"),
        "Served WASM must have valid magic header"
    );
    let wasm_cc = wasm_resp
        .headers
        .get("cache-control")
        .expect("Hashed WASM response must include Cache-Control header");
    assert_cache_control_tokens(wasm_cc, &["no-cache", "must-revalidate"], &wasm_path);
}

#[test]
fn smoke_mu_ci_web_fail_closed_credential_files_and_parsers() {
    let missing_err = load_smoke_config_with(|k| match k {
        "MU_SMOKE_BEARER_TOKEN_FILE" => Some("/nonexistent/mu-smoke-token".to_string()),
        _ => None,
    })
    .unwrap_err();
    assert!(missing_err.contains("MU_SMOKE_BEARER_TOKEN_FILE"));

    let missing_ca_err = load_smoke_config_with(|k| match k {
        "MU_SMOKE_TLS_CA_FILE" => Some("/nonexistent/mu-smoke-ca.pem".to_string()),
        _ => None,
    })
    .unwrap_err();
    assert!(missing_ca_err.contains("MU_SMOKE_TLS_CA_FILE"));

    let partial_mtls_err = load_smoke_config_with(|k| match k {
        "MU_SMOKE_TLS_CERT_FILE" => Some("/tmp/only-cert.pem".to_string()),
        _ => None,
    })
    .unwrap_err();
    assert!(partial_mtls_err.contains("MU_SMOKE_TLS_KEY_FILE"));

    let bad_timeout = load_smoke_config_with(|k| match k {
        "MU_SMOKE_TIMEOUT_SECS" => Some("0".to_string()),
        _ => None,
    })
    .unwrap_err();
    assert!(bad_timeout.contains("greater than zero"));

    let tmp_dir = std::env::temp_dir().join(format!("mu-ci-web-smoke-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp_dir);
    let tok_path = tmp_dir.join("token.txt");
    fs::write(&tok_path, "  smoke-bearer-123  \n").unwrap();
    let ok_cfg = load_smoke_config_with(|k| match k {
        "MU_SMOKE_BEARER_TOKEN_FILE" => Some(tok_path.to_string_lossy().into_owned()),
        _ => None,
    })
    .unwrap();
    assert_eq!(ok_cfg.bearer_token.as_deref(), Some("smoke-bearer-123"));
    assert!(ok_cfg.tls_ca_pem.is_none());
    assert!(ok_cfg.tls_cert_pem.is_none());
    assert!(ok_cfg.tls_key_pem.is_none());
    assert_eq!(ok_cfg.timeout, Duration::from_secs(10));
    let _ = fs::remove_dir_all(&tmp_dir);

    // Exercise pure library helpers to validate ANSI & log parsing without DOM dependencies.
    let spans = parse_ansi_spans("\x1b[32mPASS\x1b[0m smoke");
    assert_eq!(strip_ansi_codes("\x1b[32mPASS\x1b[0m smoke"), "PASS smoke");
    assert!(!spans.is_empty());

    let sample_log = concat!(
        "--------------------------------------------------------------------------------\n",
        "Step [1/1]: Verify Formatting\n",
        "Command:    cargo fmt --check\n",
        "Started At: 2026-01-01T00:00:00Z\n",
        "--------------------------------------------------------------------------------\n",
        "[ActionRunner-rs] Step 'Verify Formatting' completed successfully.\n"
    );
    let parsed = parse_job_log(sample_log);
    assert_eq!(parsed.steps.len(), 1);
    assert_eq!(parsed.steps[0].status, StepStatus::Passed);
}
