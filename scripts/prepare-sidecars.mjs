import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
const release = process.argv.includes("--release");
const target = execFileSync("rustc", ["-vV"], { encoding: "utf8" }).match(
  /^host: (.+)$/m,
)[1];
if (!target.includes("apple-darwin") && !target.includes("linux"))
  throw new Error(
    "This preview requires macOS or Linux; desktop packaging is verified on macOS only.",
  );
execFileSync(
  "cargo",
  ["build", "-p", "daygrid", "--bins", ...(release ? ["--release"] : [])],
  { stdio: "inherit" },
);
mkdirSync("apps/desktop/binaries", { recursive: true });
for (const name of ["daygrid-service", "daygrid-mcp"])
  copyFileSync(
    `target/${release ? "release" : "debug"}/${name}`,
    `apps/desktop/binaries/${name}-${target}`,
  );
