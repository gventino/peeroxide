//! Creates the release signing key and signs release packages (see docs/releasing.md).
//!
//!   release-sign keygen [--force] [--no-password] [--secret-key PATH] [--public-key PATH]
//!   release-sign sign <file> [--secret-key PATH] [--public-key PATH]
//!
//! The secret key defaults to %USERPROFILE%\.peeroxide\release.key (or $PEEROXIDE_RELEASE_KEY)
//! and must never be committed. The public key defaults to crates/update/release-key.pub, which
//! is compiled into the app. `--no-password` is for throwaway test keys only.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use minisign::{KeyPair, PublicKeyBox, SecretKey, SecretKeyBox};

const PUBLIC_KEY: &str = "crates/update/release-key.pub";

fn default_secret_key() -> PathBuf {
    if let Some(path) = std::env::var_os("PEEROXIDE_RELEASE_KEY") {
        return path.into();
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .unwrap_or_else(|| ".".into());
    Path::new(&home).join(".peeroxide").join("release.key")
}

struct Options {
    command: String,
    file: Option<PathBuf>,
    secret: PathBuf,
    public: PathBuf,
    force: bool,
    no_password: bool,
}

fn parse() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or("expected `keygen` or `sign <file>`")?;
    let mut o = Options {
        command,
        file: None,
        secret: default_secret_key(),
        public: PUBLIC_KEY.into(),
        force: false,
        no_password: false,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--force" => o.force = true,
            "--no-password" => o.no_password = true,
            "--secret-key" => o.secret = args.next().ok_or("--secret-key needs a path")?.into(),
            "--public-key" => o.public = args.next().ok_or("--public-key needs a path")?.into(),
            other if o.file.is_none() && !other.starts_with("--") => o.file = Some(other.into()),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    Ok(o)
}

fn keygen(o: &Options) -> Result<(), String> {
    if o.secret.exists() && !o.force {
        return Err(format!(
            "{} already exists. Every installed copy trusts the key it was built with, so a new \
             key means testers must install the next version by hand. Pass --force if that's \
             what you want.",
            o.secret.display()
        ));
    }
    let kp = if o.no_password {
        KeyPair::generate_unencrypted_keypair()
    } else {
        KeyPair::generate_encrypted_keypair(None)
    }
    .map_err(|e| e.to_string())?;
    if let Some(dir) = o.secret.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let secret = kp.sk.to_box(None).map_err(|e| e.to_string())?.into_string();
    std::fs::write(&o.secret, secret).map_err(|e| e.to_string())?;
    let public = kp.pk.to_box().map_err(|e| e.to_string())?.into_string();
    std::fs::write(&o.public, &public).map_err(|e| e.to_string())?;
    println!(
        "Secret key: {} (back it up somewhere safe; never commit it)",
        o.secret.display()
    );
    println!(
        "Public key: {} (commit this; the app is built with it)",
        o.public.display()
    );
    Ok(())
}

fn load_secret(path: &Path) -> Result<SecretKey, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "no signing key at {} ({e}). Create one with `just release-keygen`.",
            path.display()
        )
    })?;
    let sk_box = || SecretKeyBox::from_string(&text).map_err(|e| e.to_string());
    match SecretKey::from_unencrypted_box(sk_box()?) {
        Ok(sk) => Ok(sk),
        // Encrypted: minisign asks for the password.
        Err(_) => SecretKey::from_box(sk_box()?, None).map_err(|e| e.to_string()),
    }
}

fn sign(o: &Options) -> Result<(), String> {
    let file = o.file.as_ref().ok_or("sign needs a file")?;
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("the file needs a plain name")?;
    let data = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let sk = load_secret(&o.secret)?;
    let signature = minisign::sign(
        None,
        &sk,
        Cursor::new(&data),
        Some(&format!("file:{name}")),
        Some("signature from the Peeroxide release key"),
    )
    .map_err(|e| e.to_string())?
    .into_string();
    let sig_path = PathBuf::from(format!("{}.minisig", file.display()));
    std::fs::write(&sig_path, &signature).map_err(|e| e.to_string())?;

    // Same check the app makes, with the public key it is built with: catches a secret key that
    // doesn't match the committed public key before anyone downloads the release.
    let public = std::fs::read_to_string(&o.public).map_err(|e| e.to_string())?;
    PublicKeyBox::from_string(&public)
        .and_then(|b| b.into_public_key())
        .map_err(|_| {
            format!(
                "{} holds no public key; run `just release-keygen`",
                o.public.display()
            )
        })?;
    peeroxide_update::verify_file(file, &signature, name, &public).map_err(|e| {
        format!(
            "the signature doesn't verify with {}: {e}. Is the right key in use?",
            o.public.display()
        )
    })?;
    println!("Signed {name} -> {}", sig_path.display());
    Ok(())
}

fn main() -> ExitCode {
    let result = parse().and_then(|o| match o.command.as_str() {
        "keygen" => keygen(&o),
        "sign" => sign(&o),
        other => Err(format!("unknown command {other}; expected keygen or sign")),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("release-sign: {e}");
            ExitCode::FAILURE
        }
    }
}
