//! Serves one signed release package the way GitHub would, on this computer, for testing the
//! self-update without publishing anything (see docs/releasing.md). Run it with `--help` for the
//! options.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;

/// Serves one signed release package the way GitHub would, on this computer, to test the
/// self-update without publishing anything. Then start an older build with
/// PEEROXIDE_UPDATE_URL set to the address it prints (see docs/releasing.md).
#[derive(Parser)]
struct Cli {
    /// The package, e.g. dist/peeroxide-X.Y.Z-windows-x64.zip; its .minisig must be next to it.
    zip: PathBuf,
    /// The port to serve on, on 127.0.0.1.
    #[arg(default_value_t = 8765)]
    port: u16,
}

fn main() -> anyhow::Result<()> {
    let Cli { zip, port } = Cli::parse();
    let name = zip
        .file_name()
        .with_context(|| format!("{} is not a file", zip.display()))?
        .to_string_lossy()
        .into_owned();
    let version = name
        .strip_prefix("peeroxide-")
        .and_then(|rest| rest.split('-').next())
        .map(str::to_owned)
        .context("expected a file named peeroxide-X.Y.Z-<platform>.zip")?;
    let sig_name = format!("{name}.minisig");
    let package = std::fs::read(&zip).with_context(|| format!("reading {}", zip.display()))?;
    let sig_path = zip.with_file_name(&sig_name);
    let signature = std::fs::read(&sig_path).with_context(|| {
        format!(
            "reading {} (sign the package with `just package`)",
            sig_path.display()
        )
    })?;
    let base = format!("http://127.0.0.1:{port}");
    let releases = format!(
        r#"[{{"tag_name":"v{version}-test","draft":false,"prerelease":true,"html_url":"{base}/","assets":[
            {{"name":"{name}","size":{},"browser_download_url":"{base}/dl/{name}"}},
            {{"name":"{sig_name}","size":{},"browser_download_url":"{base}/dl/{sig_name}"}}]}}]"#,
        package.len(),
        signature.len()
    );
    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("listening on port {port}"))?;
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
    Ok(())
}
