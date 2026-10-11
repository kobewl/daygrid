// 版本号单一真值检查。
//
//   node scripts/check-version.mjs            # 只检查各处版本是否一致
//   node scripts/check-version.mjs v0.2.0     # 另外要求等于 tag，且 CHANGELOG 里有该版本的说明
//
// 为什么要有它：版本号分散在 Cargo 工作区、Tauri 配置和三个 package.json 里，
// 漏改任何一处都不会编译失败，却会让「应用显示的版本」「服务握手的版本」「Release 标签」
// 三者对不上。这类错误发出去以后很难悄悄补救，所以在 CI 和发布流程里都挡一道。
import { readFileSync } from "node:fs";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const json = (path) => JSON.parse(read(path));

const sources = {
  "Cargo.toml [workspace.package]": read("Cargo.toml").match(
    /\[workspace\.package\][^[]*?^version\s*=\s*"([^"]+)"/ms,
  )?.[1],
  "apps/desktop/tauri.conf.json": json("apps/desktop/tauri.conf.json").version,
  "package.json": json("package.json").version,
  "apps/desktop/package.json": json("apps/desktop/package.json").version,
  "packages/ui/package.json": json("packages/ui/package.json").version,
};
// Cargo.lock 里本项目两个 crate 的版本：与 Cargo.toml 不一致时 `--locked` 的构建会失败，
// 提前给出更清楚的提示。
const lock = read("Cargo.lock");
for (const crate of ["daygrid", "daygrid-desktop"]) {
  const m = lock.match(new RegExp(`name = "${crate}"\\nversion = "([^"]+)"`));
  sources[`Cargo.lock (${crate})`] = m?.[1];
}

const expected = sources["Cargo.toml [workspace.package]"];
const width = Math.max(...Object.keys(sources).map((k) => k.length));
let failed = false;
for (const [name, value] of Object.entries(sources)) {
  const ok = value === expected;
  if (!ok) failed = true;
  console.log(`${ok ? "ok  " : "FAIL"} ${name.padEnd(width)}  ${value ?? "(未找到)"}`);
}

const tag = process.argv[2];
if (tag) {
  const wanted = tag.replace(/^v/, "");
  if (wanted !== expected) {
    failed = true;
    console.log(`FAIL tag ${tag} 与应用版本 ${expected} 不一致`);
  } else {
    console.log(`ok   tag ${tag}`);
  }
  const changelog = read("CHANGELOG.md");
  if (!new RegExp(`^## \\[${wanted.replaceAll(".", "\\.")}\\]`, "m").test(changelog)) {
    failed = true;
    console.log(`FAIL CHANGELOG.md 里没有 "## [${wanted}]" 一节`);
  } else {
    console.log(`ok   CHANGELOG.md 含 ${wanted}`);
  }
}

if (failed) {
  console.error("\n版本号检查失败：请让上面所有位置保持一致后再提交或发版。");
  process.exit(1);
}
console.log(`\n版本一致：${expected}`);
