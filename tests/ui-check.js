const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const root = path.resolve(__dirname, "..");
const read = (file) => fs.readFileSync(path.join(root, file), "utf8");
const html = read("ui/index.html");
const css = read("ui/style.css");
const config = JSON.parse(read("src-tauri/tauri.conf.json"));
const themes = JSON.parse(read("ui/themes.json"));

assert.equal(config.app.windows[0].width, 280);
assert.equal(config.app.windows[0].height, 252);
assert.equal(config.app.windows[0].backgroundColor, "#00000000");
assert.equal(themes.length, 8);
assert.ok(html.includes('viewBox="0 0 72 72"'));
assert.ok(html.includes('id="orbGradient"'));
assert.ok(!css.includes(".orb-particles"));
assert.ok(!/https?:\/\//.test(html));
assert.ok(!html.includes("onclick="));

for (const match of html.matchAll(/<script src="([^"]+)"/g)) {
  assert.ok(fs.existsSync(path.join(root, "ui", match[1])), `missing script ${match[1]}`);
}
for (const id of ["row-total", "row-5h", "row-7d", "appearancePanel", "diagnosticsPanel", "loginPanel", "profilePanel", "confirmMask"]) {
  assert.ok(html.includes(`id="${id}"`), `missing ${id}`);
}
for (const script of ["ui/app.js", "ui/settings.js", "ui/motion.js", "ui/quota-state.js"]) {
  assert.ok(!read(script).includes("localStorage.setItem"), `${script} must not be a second settings store`);
}
console.log("UI static contract checks passed: 280×252, 60px orb, 8 themes, local assets, no duplicate particle layer.");
