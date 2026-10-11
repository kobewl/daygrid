import { readFileSync, copyFileSync, mkdirSync, writeFileSync } from "node:fs";

const tag = process.argv[2];
if (!/^v\d+\.\d+\.\d+$/.test(tag ?? ""))
  throw new Error("Expected release tag vX.Y.Z");
const config = JSON.parse(readFileSync("apps/desktop/tauri.conf.json", "utf8"));
if (config.version !== tag.slice(1))
  throw new Error("Updater version does not match release tag");
const archive = "target/release/bundle/macos/Daygrid.app.tar.gz";
const signature = readFileSync(`${archive}.sig`, "utf8").trim();
const material = (value) =>
  Buffer.from(
    Buffer.from(value, "base64").toString("utf8").split("\n")[1] ?? "",
    "base64",
  );
const pub = material(config.plugins.updater.pubkey);
const sig = material(signature);
if (
  pub.length !== 42 ||
  sig.length !== 74 ||
  !pub.subarray(2, 10).equals(sig.subarray(2, 10))
) {
  throw new Error("Updater signing key does not match embedded public key");
}
const name = "Daygrid_aarch64.app.tar.gz";
mkdirSync("dist", { recursive: true });
copyFileSync(archive, `dist/${name}`);
copyFileSync(`${archive}.sig`, `dist/${name}.sig`);
const notes = readFileSync("CHANGELOG.md", "utf8")
  .split(`## [${config.version}]`)[1]
  ?.split("\n## [")[0]
  ?.split("\n")
  .slice(1)
  .join("\n")
  .trim();
if (!notes) throw new Error("Missing updater release notes");
const manifest = {
  version: config.version,
  notes,
  pub_date: new Date().toISOString(),
  platforms: {
    "darwin-aarch64": {
      signature,
      url: `https://github.com/kobewl/daygrid/releases/download/${tag}/${name}`,
    },
  },
};
writeFileSync("dist/latest.json", JSON.stringify(manifest, null, 2) + "\n");
console.log(`Prepared signed updater manifest for ${tag}`);
