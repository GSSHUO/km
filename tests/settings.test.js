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
    assert.equal(preset.material.cardOpacity, preset.id === "abyss" ? 0.12 : preset.id === "sakura" ? 0.94 : 0.2);
    assert.equal(preset.material.orbOpacity, preset.id === "sakura" ? 0.94 : 0.3);
  }
});

test("opacity customization accepts 0.05 and rejects lower values", () => {
  assert.equal(S.validateOverrides({ cardOpacity: 0.05, orbOpacity: 0.05 }), true);
  assert.throws(() => S.validateOverrides({ cardOpacity: 0.049 }), /0.05–1.0/);
  assert.throws(() => S.validateOverrides({ orbOpacity: 0.01 }), /0.05–1.0/);
});

test("fresh defaults use abyss and lively while saved choices remain", () => {
  const settings = S.defaults();
  assert.equal(settings.theme.selectedId, "abyss");
  assert.equal(settings.motion.preset, "lively");
  assert.deepEqual(settings.theme.overrides, {});
  settings.theme.selectedId = "rainbow";
  settings.orb.metric = "7d";
  settings.motion.preset = "stardust";
  assert.equal(S.resolveTheme(presets, settings).label, "柔彩棱镜");
  assert.equal(settings.motion.preset, "stardust");
});

test("editor saves overrides per preset and supports undo redo", () => {
  const editor = new S.ThemeEditor(config());
  editor.setOverride("accent", "#102030");
  assert.equal(editor.currentOverrides().accent, "#102030");
  assert.equal(presets[0].palette.accent, "#8EA5FF");
  editor.undo();
  assert.equal(editor.currentOverrides().accent, undefined);
  editor.redo();
  assert.equal(editor.resolved().palette.accent, "#102030");
  editor.selectPreset("polar");
  assert.equal(editor.resolved().palette.accent, "#8BD6FF");
  editor.selectPreset("abyss");
  assert.equal(editor.resolved().palette.accent, "#102030");
  assert.deepEqual(Object.keys(editor.draft.theme.overrides), ["abyss"]);
});

test("legacy custom records are cleared and selected preset falls back to its base", () => {
  const legacy = config();
  legacy.settings.theme = {
    selectedId: "custom-old",
    customThemes: [{ id: "custom-old", name: "旧主题", basePresetId: "polar", overrides: { accent: "#102030" } }],
  };
  legacy.settings.motion.preset = "stardust";
  const editor = new S.ThemeEditor(legacy);
  assert.equal(editor.draft.theme.selectedId, "polar");
  assert.deepEqual(editor.draft.theme.overrides, {});
  assert.equal(editor.draft.theme.customThemes, undefined);
  assert.equal(editor.draft.motion.preset, "stardust");
});

test("export contains no account or window fields", () => {
  const editor = new S.ThemeEditor(config());
  editor.setOverride("accent", "#102030");
  const text = editor.exportText();
  const parsed = JSON.parse(text);
  assert.deepEqual(Object.keys(parsed).sort(), ["basePresetId", "name", "overrides", "schemaVersion"].sort());
  assert.equal(parsed.basePresetId, "abyss");
  assert.equal(parsed.overrides.accent, "#102030");
});

test("import updates its built-in base without creating a custom theme", () => {
  const editor = new S.ThemeEditor(config());
  editor.importText('{"schemaVersion":1,"name":"导入","basePresetId":"polar","overrides":{"accent":"#102030"}}');
  assert.equal(editor.draft.theme.selectedId, "polar");
  assert.equal(editor.resolved().palette.accent, "#102030");
  assert.equal(editor.draft.theme.customThemes, undefined);
  assert.deepEqual(Object.keys(editor.draft.theme.overrides), ["polar"]);
});

test("unsafe or unsupported theme imports are rejected", () => {
  assert.throws(() => S.parseThemeText('{"schemaVersion":2}'));
  assert.throws(() => S.parseThemeText('{"schemaVersion":1,"name":"x","basePresetId":"abyss","overrides":{"accent":"url(javascript:alert(1))"}}'));
  assert.throws(() => S.parseThemeText('{"schemaVersion":1,"name":"x","basePresetId":"abyss","token":"secret","overrides":{}}'));
});
