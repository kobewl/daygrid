//! 发布前和线上附件回读后，用应用内置公钥验证真实更新包。
use base64::{Engine, engine::general_purpose::STANDARD};
use minisign_verify::{PublicKey, Signature};
use std::{env, fs};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: verify-update <tauri.conf.json> <archive> <signature>".into());
    }
    let config: serde_json::Value = serde_json::from_slice(&fs::read(&args[0])?)?;
    let public = config["plugins"]["updater"]["pubkey"]
        .as_str()
        .ok_or("missing updater pubkey")?;
    let public = String::from_utf8(STANDARD.decode(public.trim())?)?;
    let signature = String::from_utf8(STANDARD.decode(fs::read_to_string(&args[2])?.trim())?)?;
    let data = fs::read(&args[1])?;
    let signature = Signature::decode(&signature)?;
    PublicKey::decode(&public)?.verify(&data, &signature, false)?;
    let signed_version = signature
        .trusted_comment()
        .split('\t')
        .find_map(|field| field.strip_prefix("version:"))
        .ok_or("missing signed app version")?;
    if Some(signed_version) != config["version"].as_str() {
        return Err("signed version does not match application version".into());
    }
    println!("PASS: updater archive signature matches embedded public key");
    Ok(())
}
