//! `daygrid-server` 命令行：初始化、启动、邀请、成员管理与备份。
use daygrid_server::{
    app::{self, AppState},
    db::ServerDb,
};
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::exit,
};

const USAGE: &str = "\
daygrid-server — Daygrid 自托管协同服务端

用法：
  daygrid-server init   --workspace <名称> --owner <你的名字> [--data-dir <目录>]
  daygrid-server serve  [--listen 127.0.0.1:7878] [--data-dir <目录>]
  daygrid-server invite --workspace <名称> [--role editor|viewer] [--hours 72] [--uses 1] [--data-dir <目录>]
  daygrid-server members --workspace <名称> [--data-dir <目录>]
  daygrid-server role   --workspace <名称> --user <名字或ID> --role editor|viewer [--data-dir <目录>]
  daygrid-server revoke --workspace <名称> --user <名字或ID> [--data-dir <目录>]
  daygrid-server backup --out <文件> [--data-dir <目录>]

数据目录默认 $DAYGRID_SERVER_DATA，否则 ./daygrid-server-data。
服务端只说 HTTP；公网使用请放在 TLS 反向代理（Caddy / Nginx）之后。默认只监听本机。";

fn fail(message: &str) -> ! {
    eprintln!("错误：{message}");
    exit(1)
}

fn parse(args: &[String]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut it = args.iter();
    while let Some(key) = it.next() {
        let Some(name) = key.strip_prefix("--") else {
            fail(&format!("无法识别的参数：{key}\n\n{USAGE}"));
        };
        let Some(value) = it.next() else {
            fail(&format!("参数 --{name} 缺少值。"));
        };
        map.insert(name.to_string(), value.clone());
    }
    map
}

fn open_db(opts: &HashMap<String, String>) -> (ServerDb, PathBuf) {
    let dir = opts
        .get("data-dir")
        .cloned()
        .or_else(|| std::env::var("DAYGRID_SERVER_DATA").ok())
        .unwrap_or_else(|| "daygrid-server-data".into());
    let dir = Path::new(&dir);
    if let Err(e) = fs::create_dir_all(dir) {
        fail(&format!("无法创建数据目录：{e}"));
    }
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    let path = dir.join("server.db");
    let db = ServerDb::open(&path).unwrap_or_else(|e| fail(&e.message));
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    (db, path)
}

fn workspace(db: &ServerDb, opts: &HashMap<String, String>) -> daygrid::sync::WorkspaceInfo {
    let name = opts
        .get("workspace")
        .unwrap_or_else(|| fail("缺少 --workspace。"));
    match db.workspace_by_name(name) {
        Ok(Some(w)) => w,
        Ok(None) => fail(&format!("没有名为「{name}」的团队日历。")),
        Err(e) => fail(&e.message),
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("DAYGRID_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        println!("{USAGE}");
        return;
    };
    if matches!(cmd, "-h" | "--help" | "help") {
        println!("{USAGE}");
        return;
    }
    let opts = parse(&args[1..]);
    let (mut db, db_path) = open_db(&opts);
    match cmd {
        "init" => {
            let ws = opts
                .get("workspace")
                .unwrap_or_else(|| fail("缺少 --workspace。"));
            let owner = opts.get("owner").unwrap_or_else(|| fail("缺少 --owner。"));
            let (ws_id, _, token) = db
                .init_workspace(ws, owner)
                .unwrap_or_else(|e| fail(&e.message));
            println!("已创建团队日历「{ws}」（ID {ws_id}），owner：{owner}");
            println!();
            println!("你的访问令牌（只显示这一次，请妥善保存；丢失只能重新初始化）：");
            println!("  {token}");
            println!();
            println!(
                "下一步：daygrid-server serve，然后在日格「设置 → 协作」里填入服务器地址和这个令牌。"
            );
        }
        "invite" => {
            let ws = workspace(&db, &opts);
            let owner = db.owner_id(&ws.id).unwrap_or_else(|e| fail(&e.message));
            let role = opts.get("role").map(String::as_str).unwrap_or("editor");
            let hours = opts.get("hours").and_then(|v| v.parse().ok()).unwrap_or(72);
            let uses = opts.get("uses").and_then(|v| v.parse().ok()).unwrap_or(1);
            let v = db
                .create_invite(&ws.id, &owner, role, hours, uses, daygrid::sync::now_ms())
                .unwrap_or_else(|e| fail(&e.message));
            println!("邀请码（{role}，{hours} 小时内有效，可用 {uses} 次）：");
            println!("  {}", v["code"].as_str().unwrap_or_default());
        }
        "members" => {
            let ws = workspace(&db, &opts);
            for m in db.members(&ws.id).unwrap_or_else(|e| fail(&e.message)) {
                println!(
                    "{:<8} {:<20} {}  加入于 {}",
                    m.role, m.name, m.id, m.joined_at
                );
            }
        }
        "role" => {
            let ws = workspace(&db, &opts);
            let user = opts.get("user").unwrap_or_else(|| fail("缺少 --user。"));
            let role = opts.get("role").unwrap_or_else(|| fail("缺少 --role。"));
            db.set_role(&ws.id, user, role)
                .unwrap_or_else(|e| fail(&e.message));
            println!("已把「{user}」的角色改为 {role}。对方的客户端在下一次同步时生效。");
        }
        "revoke" => {
            let ws = workspace(&db, &opts);
            let user = opts.get("user").unwrap_or_else(|| fail("缺少 --user。"));
            db.remove_member(&ws.id, None, user)
                .unwrap_or_else(|e| fail(&e.message));
            println!("已移除成员「{user}」。其令牌若不再属于任何团队日历，将整体失效。");
        }
        "backup" => {
            let out = opts.get("out").unwrap_or_else(|| fail("缺少 --out。"));
            if Path::new(out).exists() {
                fail("目标文件已存在。");
            }
            db.backup_to(Path::new(out))
                .unwrap_or_else(|e| fail(&e.message));
            let _ = fs::set_permissions(out, fs::Permissions::from_mode(0o600));
            println!("已备份到 {out}");
        }
        "serve" => {
            let listen = opts
                .get("listen")
                .map(String::as_str)
                .unwrap_or("127.0.0.1:7878");
            let listener = tokio::net::TcpListener::bind(listen)
                .await
                .unwrap_or_else(|e| fail(&format!("无法监听 {listen}：{e}")));
            let local_only = listener
                .local_addr()
                .map(|a| a.ip().is_loopback())
                .unwrap_or(true);
            tracing::info!(listen, db = %db_path.display(), "daygrid-server started");
            if !local_only {
                tracing::warn!(
                    "正在监听非回环地址：服务端不提供 TLS，公网使用请放在 TLS 反向代理之后"
                );
            }
            let state = AppState::new(db);
            if let Err(e) = app::serve(listener, state).await {
                fail(&format!("服务异常退出：{e}"));
            }
            tracing::info!("daygrid-server stopped");
        }
        other => fail(&format!("未知命令：{other}\n\n{USAGE}")),
    }
}
