const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const S = require("../ui/settings.js");
const presets = JSON.parse(fs.readFileSync(require.resolve("../ui/themes.json"), "utf8"));
const config = () => ({ presets, settings: S.defaults(), revision: 0 });

test("eight presets have unique ids and complete palettes", () => {
  assert.equal(presets.length, 8);
  assert.equal(new Set(presets.map((p) => p.id)).size, 8);
  for (const preset of presets) {
    for (const key of ["background", "surface", "text", "secondary", "accent", "glow"]) assert.ok(S.isHex(preset.palette[key]));
    assert.ok(preset.palette.gradient.length >= 2 && preset.palette.gradient.length <= 5);
    assert.equal(preset.palette.particles.length, 2);
  }
});

test("legacy ids migrate without creating custom themes", () => {
  const settings = S.defaults();
  settings.theme.selectedId = "rainbow";
  settings.orb.metric = "7d";
  settings.motion.preset = "stardust";
  assert.equal(settings.theme.customThemes.length, 0);
  assert.equal(S.resolveTheme(presets, settings).label, "柔彩棱镜");
});

test("custom editor preserves presets and supports undo redo", () => {
  const editor = new S.ThemeEditor(config());
  editor.setOverride("accent", "#102030");
  assert.equal(editor.custom().overrides.accent, "#102030");
  assert.equal(presets[0].palette.accent, "#8EA5FF");
  editor.undo();
  assert.equal(editor.custom().overrides.accent, undefined);
  editor.redo();
  assert.equal(editor.resolved().palette.accent, "#102030");
});

test("export contains no account or window fields", () => {
  const editor = new S.ThemeEditor(config());
  editor.createCustom("我的主题");
  const text = editor.exportText();
  const parsed = JSON.parse(text);
  assert.deepEqual(Object.keys(parsed).sort(), ["basePresetId", "name", "overrides", "schemaVersion"].sort());
});

test("unsafe or unsupported theme imports are rejected", () => {
  assert.throws(() => S.parseThemeText('{"schemaVersion":2}'));
  assert.throws(() => S.parseThemeText('{"schemaVersion":1,"name":"x","basePresetId":"abyss","overrides":{"accent":"url(javascript:alert(1))"}}'));
  assert.throws(() => S.parseThemeText('{"schemaVersion":1,"name":"x","basePresetId":"abyss","token":"secret","overrides":{}}'));
});
