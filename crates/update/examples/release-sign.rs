//! Creates the release signing key and signs release packages (see docs/releasing.md). Run it
//! with `--help` for the commands and options.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use clap::{Args, Parser, Subcommand};
use minisign::{KeyPair, PublicKeyBox, SecretKey, SecretKeyBox};

const PUBLIC_KEY: &str = "crates/update/release-key.pub";

/// Creates the release signing key and signs release packages (see docs/releasing.md).
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the release signing key (asks for a password) and write its public key.
    Keygen {
        /// Replace an existing key. Every installed copy trusts the key it was built with, so
        /// testers then have to install the next version by hand.
        #[arg(long)]
        force: bool,
        /// Leave the secret key unencrypted. For throwaway test keys only.
        #[arg(long)]
        no_password: bool,
        #[command(flatten)]
        keys: Keys,
    },
    /// Sign a release package into FILE.minisig, then check the signature with the public key.
    Sign {
        /// The package, e.g. dist/peeroxide-X.Y.Z-windows-x64.zip.
        file: PathBuf,
        #[command(flatten)]
        keys: Keys,
    },
}

#[derive(Args)]
struct Keys {
    /// The secret key. Never commit it.
    #[arg(
        long,
        value_name = "PATH",
        env = "PEEROXIDE_RELEASE_KEY",
        default_value_os_t = default_secret_key()
    )]
    secret_key: PathBuf,
    /// The public key, which the app is built with.
    #[arg(long, value_name = "PATH", default_value = PUBLIC_KEY)]
    public_key: PathBuf,
}

/// `.peeroxide/release.key` in the user's home folder.
fn default_secret_key() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .unwrap_or_else(|| ".".into());
    Path::new(&home).join(".peeroxide").join("release.key")
}

fn keygen(keys: &Keys, force: bool, no_password: bool) -> anyhow::Result<()> {
    let (secret, public_path) = (&keys.secret_key, &keys.public_key);
    ensure!(
        !secret.exists() || force,
        "{} already exists. Every installed copy trusts the key it was built with, so a new key \
         means testers must install the next version by hand. Pass --force if that's what you \
         want.",
        secret.display()
    );
    let kp = if no_password {
        KeyPair::generate_unencrypted_keypair()
    } else {
        KeyPair::generate_encrypted_keypair(None)
    }
    .context("generating the key pair")?;
    if let Some(dir) = secret.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(secret, kp.sk.to_box(None)?.into_string())
        .with_context(|| format!("writing {}", secret.display()))?;
    std::fs::write(public_path, kp.pk.to_box()?.into_string())
        .with_context(|| format!("writing {}", public_path.display()))?;
    println!(
        "Secret key: {} (back it up somewhere safe; never commit it)",
        secret.display()
    );
    println!(
        "Public key: {} (commit this; the app is built with it)",
        public_path.display()
    );
    Ok(())
}

fn load_secret(path: &Path) -> anyhow::Result<SecretKey> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "no signing key at {}. Create one with `just release-keygen`.",
            path.display()
        )
    })?;
    let sk_box = || SecretKeyBox::from_string(&text).context("reading the signing key");
    match SecretKey::from_unencrypted_box(sk_box()?) {
        Ok(sk) => Ok(sk),
        // Encrypted: minisign asks for the password.
        Err(_) => SecretKey::from_box(sk_box()?, None).context("unlocking the signing key"),
    }
}

fn sign(file: &Path, keys: &Keys) -> anyhow::Result<()> {
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .context("the file needs a plain name")?;
    let data = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let sk = load_secret(&keys.secret_key)?;
    let signature = minisign::sign(
        None,
        &sk,
        Cursor::new(&data),
        Some(&format!("file:{name}")),
        Some("signature from the Peeroxide release key"),
    )
    .context("signing")?
    .into_string();
    let sig_path = PathBuf::from(format!("{}.minisig", file.display()));
    std::fs::write(&sig_path, &signature)
        .with_context(|| format!("writing {}", sig_path.display()))?;

    // Same check the app makes, with the public key it is built with: catches a secret key that
    // doesn't match the committed public key before anyone downloads the release.
    let public = std::fs::read_to_string(&keys.public_key)
        .with_context(|| format!("reading {}", keys.public_key.display()))?;
    PublicKeyBox::from_string(&public)
        .and_then(|b| b.into_public_key())
        .with_context(|| {
            format!(
                "{} holds no public key; run `just release-keygen`",
                keys.public_key.display()
            )
        })?;
    peeroxide_update::verify_file(file, &signature, name, &public).with_context(|| {
        format!(
            "the signature doesn't verify with {}. Is the right key in use?",
            keys.public_key.display()
        )
    })?;
    println!("Signed {name} -> {}", sig_path.display());
    Ok(())
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Keygen {
            force,
            no_password,
            keys,
        } => keygen(&keys, force, no_password),
        Command::Sign { file, keys } => sign(&file, &keys),
    }
}
