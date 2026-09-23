//! Serves one signed release package the way GitHub would, on this computer, for testing the
//! self-update without publishing anything (see docs/releasing.md).
//!
//!   serve-release <dist/peeroxide-X.Y.Z-windows-x64.zip> [port]
//!
//! Then start an older build with PEEROXIDE_UPDATE_URL set to the address it prints.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(zip) = args.next().map(PathBuf::from) else {
        eprintln!("usage: serve-release <peeroxide-X.Y.Z-windows-x64.zip> [port]");
        return ExitCode::FAILURE;
    };
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(8765);
    let name = zip.file_name().unwrap().to_string_lossy().into_owned();
    let Some(version) = name
        .strip_prefix("peeroxide-")
        .and_then(|rest| rest.split('-').next())
        .map(str::to_owned)
    else {
        eprintln!("expected a file named peeroxide-X.Y.Z-<platform>.zip");
        return ExitCode::FAILURE;
    };
    let sig_name = format!("{name}.minisig");
    let (package, signature) = match (
        std::fs::read(&zip),
        std::fs::read(zip.with_file_name(&sig_name)),
    ) {
        (Ok(p), Ok(s)) => (p, s),
        _ => {
            eprintln!("need both {name} and {sig_name} (sign it with `just package`)");
            return ExitCode::FAILURE;
        }
    };
    let base = format!("http://127.0.0.1:{port}");
    let releases = format!(
        r#"[{{"tag_name":"v{version}-test","draft":false,"prerelease":true,"html_url":"{base}/","assets":[
            {{"name":"{name}","size":{},"browser_download_url":"{base}/dl/{name}"}},
            {{"name":"{sig_name}","size":{},"browser_download_url":"{base}/dl/{sig_name}"}}]}}]"#,
        package.len(),
        signature.len()
    );
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("port {port}: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("Serving {name} as release v{version}-test.");
    println!("Start the older build with:  PEEROXIDE_UPDATE_URL={base}/releases");
    println!("Ctrl+C to stop.");
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut request = String::new();
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(s) => s,
            Err(_) => continue,
        });
        if reader.read_line(&mut request).is_err() {
            continue;
        }
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
        }
        let path = request.split_whitespace().nth(1).unwrap_or("/");
        let path = path.split('?').next().unwrap_or(path);
        let (status, body): (u16, &[u8]) = if path == "/releases" {
            (200, releases.as_bytes())
        } else if path == format!("/dl/{name}") {
            (200, &package)
        } else if path == format!("/dl/{sig_name}") {
            (200, &signature)
        } else {
            (404, b"not found")
        };
        println!("{status} {path}");
        let head = format!(
            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
    }
    ExitCode::SUCCESS
}
