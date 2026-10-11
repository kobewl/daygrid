//! 把 Daygrid 的 MCP 桥接写入常见 AI 客户端的配置。
//!
//! 安全边界（这是在改**别的应用**的配置文件，所以保守）：
//! - 只在用户点击时才写，且只增删 `mcpServers.daygrid` 这一个键，其余内容原样保留；
//! - 首次修改前备份原文件为 `<文件名>.daygrid.bak`（已存在则不覆盖，保证永远留有最初版本）；
//! - 配置文件不是合法 JSON 时拒绝改动，让用户自己处理；
//! - 原子写入（临时文件 + rename），不会留下写一半的文件。
//!
//! 路径与格式依据各客户端官方文档：
//! - Cursor：`~/.cursor/mcp.json`，顶层 `mcpServers`（cursor.com/docs/context/mcp）。
//! - Claude Desktop：macOS `~/Library/Application Support/Claude/claude_desktop_config.json`，
//!   顶层 `mcpServers`（modelcontextprotocol.io 本地服务器指南）。
//! - Claude Code：`claude mcp add --transport stdio --scope user <name> -- <command> [args...]`
//!   （code.claude.com/docs/en/mcp）。
//! - Codex：`codex mcp add <name> -- <command> [args...]`，配置在 `~/.codex/config.toml`
//!   （developers.openai.com/codex/mcp）。
//!
//! Claude Code 与 Codex 的配置格式较复杂且客户端自带 CLI，这里只生成可复制的命令，不直接改文件。
use crate::{Error, Result, domain};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// 写入各客户端配置时使用的服务器名。
pub const ENTRY_NAME: &str = "daygrid";

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// 直接编辑 JSON 配置文件。
    File,
    /// 生成命令，由用户在终端执行。
    Command,
}

struct Client {
    id: &'static str,
    name: &'static str,
    kind: Kind,
    /// 相对用户主目录的路径：File 类是配置文件，Command 类仅用于检测是否已配置。
    path: &'static str,
}

const CLIENTS: [Client; 4] = [
    Client {
        id: "cursor",
        name: "Cursor",
        kind: Kind::File,
        path: ".cursor/mcp.json",
    },
    Client {
        id: "claude_desktop",
        name: "Claude Desktop",
        kind: Kind::File,
        path: "Library/Application Support/Claude/claude_desktop_config.json",
    },
    Client {
        id: "claude_code",
        name: "Claude Code",
        kind: Kind::Command,
        path: ".claude.json",
    },
    Client {
        id: "codex",
        name: "Codex",
        kind: Kind::Command,
        path: ".codex/config.toml",
    },
];

fn client(id: &str) -> Result<&'static Client> {
    CLIENTS
        .iter()
        .find(|c| c.id == id)
        .ok_or_else(|| Error::new("VALIDATION_ERROR", "未知的客户端。"))
}

fn io_err(what: &str) -> impl Fn(std::io::Error) -> Error + '_ {
    move |e| {
        tracing::warn!(error = %e, what, "client config io failed");
        Error::new(
            "SERVICE_UNAVAILABLE",
            format!("{what}失败，请检查文件权限。"),
        )
    }
}

/// 给 shell 用的最小引用：只在必要时加单引号。
fn quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-:@=+,".contains(c))
    {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

pub fn manual_command(client_id: &str, connection_id: &str, exe: &Path) -> Option<String> {
    let exe = quote(&exe.to_string_lossy());
    match client_id {
        "claude_code" => Some(format!(
            "claude mcp add --transport stdio --scope user {ENTRY_NAME} -- {exe} --connection {connection_id}"
        )),
        "codex" => Some(format!(
            "codex mcp add {ENTRY_NAME} -- {exe} --connection {connection_id}"
        )),
        _ => None,
    }
}

fn entry(connection_id: &str, exe: &Path) -> Value {
    json!({"command":exe.to_string_lossy(),"args":["--connection",connection_id]})
}

/// 读取 JSON 配置；文件不存在或为空视为空对象；不是合法 JSON 则拒绝。
fn read_config(path: &Path) -> Result<(Value, bool)> {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok((json!({}), true)),
        Ok(text) => {
            let v: Value = serde_json::from_str(&text).map_err(|_| {
                Error::new(
                    "VALIDATION_ERROR",
                    "配置文件不是有效的 JSON，已保持原样。请先手动修复，或改用复制配置。",
                )
            })?;
            if v.is_object() {
                Ok((v, true))
            } else {
                Err(Error::new(
                    "VALIDATION_ERROR",
                    "配置文件顶层不是对象，已保持原样。",
                ))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((json!({}), false)),
        Err(e) => Err(io_err("读取配置文件")(e)),
    }
}

fn write_config(path: &Path, value: &Value, existed: bool) -> Result<Option<String>> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(io_err("创建配置目录"))?;
    let mut backup = None;
    if existed {
        let bak = PathBuf::from(format!("{}.daygrid.bak", path.display()));
        if !bak.exists() {
            fs::copy(path, &bak).map_err(io_err("备份配置文件"))?;
            backup = Some(bak.display().to_string());
        }
    }
    let mode = fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o600);
    let tmp = parent.join(format!(
        ".{}.daygrid.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let mut text = serde_json::to_string_pretty(value).unwrap();
    text.push('\n');
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(&tmp)
        .map_err(io_err("写入配置文件"))?;
    f.write_all(text.as_bytes())
        .and_then(|_| f.sync_all())
        .map_err(io_err("写入配置文件"))?;
    fs::rename(&tmp, path).map_err(io_err("替换配置文件"))?;
    Ok(backup)
}

pub fn status(home: &Path, connection_id: &str, exe: &Path) -> Vec<Value> {
    CLIENTS
        .iter()
        .map(|c| {
            let path = home.join(c.path);
            let (installed, other) = match c.kind {
                Kind::File => match read_config(&path) {
                    Ok((v, _)) => match v["mcpServers"][ENTRY_NAME]["args"].as_array() {
                        Some(args) => {
                            let ours = args.iter().any(|a| a == connection_id);
                            (ours, !ours)
                        }
                        None => (false, false),
                    },
                    Err(_) => (false, false),
                },
                // 命令类客户端的配置格式不归我们管，只做文本层面的存在性判断。
                Kind::Command => {
                    let text = fs::read_to_string(&path).unwrap_or_default();
                    let has = text.contains(ENTRY_NAME) && text.contains("daygrid-mcp");
                    let ours = has && text.contains(connection_id);
                    (ours, has && !ours)
                }
            };
            let detected = path.parent().is_some_and(Path::exists);
            json!({
                "id":c.id,"name":c.name,
                "kind":if c.kind == Kind::File {"file"} else {"command"},
                "config_path":path.display().to_string(),
                "detected":detected,
                "installed":installed,
                "configured_for_other_connection":other,
                "manual_command":manual_command(c.id, connection_id, exe),
            })
        })
        .collect()
}

pub fn install(home: &Path, client_id: &str, connection_id: &str, exe: &Path) -> Result<Value> {
    domain::uuid(connection_id)?;
    let c = client(client_id)?;
    if c.kind != Kind::File {
        return Err(Error::new(
            "VALIDATION_ERROR",
            "该客户端请复制命令后在终端执行。",
        ));
    }
    let path = home.join(c.path);
    let (mut config, existed) = read_config(&path)?;
    let servers = config
        .as_object_mut()
        .unwrap()
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    let servers = servers.as_object_mut().ok_or_else(|| {
        Error::new(
            "VALIDATION_ERROR",
            "配置里的 mcpServers 不是对象，已保持原样。",
        )
    })?;
    let replaced = servers.contains_key(ENTRY_NAME);
    servers.insert(ENTRY_NAME.into(), entry(connection_id, exe));
    let backup = write_config(&path, &config, existed)?;
    tracing::info!(client = c.id, replaced, "mcp config written");
    Ok(json!({
        "client":c.id,"name":c.name,"config_path":path.display().to_string(),
        "backup":backup,"replaced_existing":replaced,
        "restart_hint":format!("请完全退出并重新打开 {}，新的 MCP 服务器才会加载。", c.name)
    }))
}

pub fn uninstall(home: &Path, client_id: &str) -> Result<Value> {
    let c = client(client_id)?;
    if c.kind != Kind::File {
        return Err(Error::new(
            "VALIDATION_ERROR",
            "该客户端请使用其自带命令移除（例如 `codex mcp remove daygrid`）。",
        ));
    }
    let path = home.join(c.path);
    let (mut config, existed) = read_config(&path)?;
    let removed = config
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .is_some_and(|s| s.remove(ENTRY_NAME).is_some());
    if removed {
        write_config(&path, &config, existed)?;
        tracing::info!(client = c.id, "mcp config entry removed");
    }
    Ok(json!({"client":c.id,"removed":removed}))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "11111111-2222-4333-8444-555555555555";
    fn exe() -> PathBuf {
        "/Applications/Daygrid.app/Contents/MacOS/daygrid-mcp".into()
    }

    #[test]
    fn merges_only_our_entry_and_backs_up_once() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".cursor/mcp.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"mcpServers":{"zeta":{"command":"z"},"alpha":{"command":"a"}},"other":1}"#,
        )
        .unwrap();
        let first = install(home.path(), "cursor", ID, &exe()).unwrap();
        assert!(first["backup"].is_string());
        let v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["other"], 1);
        assert_eq!(v["mcpServers"]["zeta"]["command"], "z");
        assert_eq!(
            v["mcpServers"]["daygrid"]["args"],
            json!(["--connection", ID])
        );
        // 其他条目保持原有顺序：zeta 仍在 alpha 之前。
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.find("zeta").unwrap() < text.find("alpha").unwrap());
        // 备份是最初版本，二次安装不会覆盖它。
        let second = install(home.path(), "cursor", ID, &exe()).unwrap();
        assert!(second["backup"].is_null());
        assert_eq!(second["replaced_existing"], true);
        let bak = fs::read_to_string(format!("{}.daygrid.bak", path.display())).unwrap();
        assert!(!bak.contains("daygrid"));
        assert_eq!(status(home.path(), ID, &exe())[0]["installed"], true);
        assert_eq!(uninstall(home.path(), "cursor").unwrap()["removed"], true);
        let v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(v["mcpServers"].get("daygrid").is_none());
        assert_eq!(v["mcpServers"]["alpha"]["command"], "a");
    }

    #[test]
    fn refuses_to_touch_invalid_or_unexpected_files() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".cursor/mcp.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        for bad in ["{ not json", "[]", r#"{"mcpServers":[]}"#] {
            fs::write(&path, bad).unwrap();
            assert_eq!(
                install(home.path(), "cursor", ID, &exe()).unwrap_err().code,
                "VALIDATION_ERROR"
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), bad);
        }
        assert!(install(home.path(), "codex", ID, &exe()).is_err());
        assert!(install(home.path(), "cursor", "not-a-uuid", &exe()).is_err());
    }

    #[test]
    fn creates_missing_config_and_quotes_commands() {
        let home = tempfile::tempdir().unwrap();
        install(home.path(), "claude_desktop", ID, &exe()).unwrap();
        assert!(
            home.path()
                .join("Library/Application Support/Claude/claude_desktop_config.json")
                .exists()
        );
        let cmd =
            manual_command("codex", ID, Path::new("/Applications/My App/daygrid-mcp")).unwrap();
        assert!(cmd.starts_with(
            "codex mcp add daygrid -- '/Applications/My App/daygrid-mcp' --connection"
        ));
        assert!(
            manual_command("claude_code", ID, &exe())
                .unwrap()
                .contains("--scope user")
        );
        assert!(manual_command("cursor", ID, &exe()).is_none());
    }
}
