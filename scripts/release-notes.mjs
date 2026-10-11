// 从 CHANGELOG.md 取出某个版本的说明，加上「未签名」提示，写到 stdout，
// 供发布工作流作为 GitHub Release 正文。
//
//   node scripts/release-notes.mjs v0.2.0 > notes.md
import { readFileSync } from "node:fs";

const tag = process.argv[2];
if (!tag) {
  console.error("用法：node scripts/release-notes.mjs <tag>");
  process.exit(2);
}
const version = tag.replace(/^v/, "");
const changelog = readFileSync(new URL("../CHANGELOG.md", import.meta.url), "utf8");

const lines = changelog.split("\n");
const start = lines.findIndex((l) => l.startsWith(`## [${version}]`));
if (start < 0) {
  console.error(`CHANGELOG.md 里没有 "## [${version}]" 一节`);
  process.exit(1);
}
let end = lines.findIndex((l, i) => i > start && l.startsWith("## ["));
if (end < 0) end = lines.length;
const body = lines
  .slice(start + 1, end)
  .join("\n")
  .trim();
if (!body) {
  console.error(`"## [${version}]" 一节是空的`);
  process.exit(1);
}

process.stdout.write(`> **未做 Apple 开发者签名与公证的个人发布包（仅 ad-hoc 签名，Apple Silicon）。**
> 下载 zip 解压后把 \`Daygrid.app\` 拖进「应用程序」。首次打开如果被 macOS 拦住，请到
> 「系统设置 → 隐私与安全性」允许；具体入口随 macOS 版本不同，请以你系统里的界面为准。
> 校验：下载同名的 \`.sha256\` 文件，在同一目录执行 \`shasum -a 256 -c Daygrid_${version}_aarch64.zip.sha256\`。

${body}
`);
