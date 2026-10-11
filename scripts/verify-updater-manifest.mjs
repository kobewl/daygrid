import { readFileSync, statSync } from "node:fs";
import { join } from "node:path";

const [tag, directory] = process.argv.slice(2);
if (!/^v\d+\.\d+\.\d+$/.test(tag ?? "") || !directory) {
  throw new Error("Expected vX.Y.Z and downloaded assets directory");
}
const manifest = JSON.parse(
  readFileSync(join(directory, "latest.json"), "utf8"),
);
const name = "Daygrid_aarch64.app.tar.gz";
const platform = manifest.platforms?.["darwin-aarch64"];
const signature = readFileSync(join(directory, `${name}.sig`), "utf8").trim();
if (
  manifest.version !== tag.slice(1) ||
  platform?.signature?.trim() !== signature ||
  platform?.url !==
    `https://github.com/kobewl/daygrid/releases/download/${tag}/${name}` ||
  !manifest.notes?.trim() ||
  !Number.isFinite(Date.parse(manifest.pub_date)) ||
  statSync(join(directory, name)).size === 0
) {
  throw new Error("Published updater manifest does not match release assets");
}
console.log(`PASS: published updater manifest matches ${tag}`);
