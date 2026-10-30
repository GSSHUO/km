(function (root) {
  "use strict";

  const clone = (value) => JSON.parse(JSON.stringify(value));
  const isHex = (value) => typeof value === "string" && /^#[0-9a-fA-F]{6}$/.test(value);

  function defaults() {
    return {
      schemaVersion: 1,
      theme: { selectedId: "abyss", customThemes: [] },
      orb: { metric: "total", decimals: 2 },
      motion: { preset: "lively" },
      material: { mode: "auto" },
      layout: { density: "compact" },
    };
  }

  function resolveTheme(presets, settings) {
    const custom = settings.theme.customThemes.find((item) => item.id === settings.theme.selectedId);
    const baseId = custom ? custom.basePresetId : settings.theme.selectedId;
    const base = presets.find((item) => item.id === baseId) || presets[0];
    const theme = clone(base);
    if (custom) {
      theme.id = custom.id;
      theme.label = custom.name;
      theme.basePresetId = custom.basePresetId;
      const overrides = custom.overrides || {};
      Object.entries(overrides).forEach(([key, value]) => {
        if (key === "appearance") theme.appearance = value;
        else if (["cardOpacity", "orbOpacity", "orbGlow"].includes(key)) theme.material[key] = value;
        else if (/^status(Normal|Warning|Critical|Unknown)$/.test(key)) {
          theme.status[key.replace("status", "").toLowerCase()] = value;
        } else theme.palette[key] = clone(value);
      });
    }
    return theme;
  }

  function rgba(hex, alpha) {
    if (!isHex(hex)) return `rgba(0,0,0,${alpha})`;
    const value = parseInt(hex.slice(1), 16);
    return `rgba(${value >> 16},${(value >> 8) & 255},${value & 255},${alpha})`;
  }

  function cssVars(theme) {
    const palette = theme.palette;
    const material = theme.material;
    return {
      "--background": palette.background,
      "--surface": palette.surface,
      "--text": palette.text,
      "--secondary": palette.secondary,
      "--accent": palette.accent,
      "--button-text": palette.buttonText || palette.background,
      "--glow": palette.glow,
      "--border": palette.border || rgba(palette.text, 0.14),
      "--track": palette.track || palette.cardTrack || rgba(palette.text, 0.12),
      "--selected": palette.selected || rgba(palette.accent, 0.12),
      "--focus": palette.focus || palette.accent,
      "--normal": theme.status.normal,
      "--warning": theme.status.warning,
      "--critical": theme.status.critical,
      "--unknown": theme.status.unknown,
      "--card-opacity": material.cardOpacity,
      "--orb-opacity": material.orbOpacity,
      "--orb-glow": material.orbGlow,
      "--card-bg": rgba(palette.background, material.cardOpacity),
      "--orb-bg": rgba(palette.orbBackground || palette.background, material.orbOpacity),
      "--orb-glow-color": rgba(palette.glow, material.orbGlow),
    };
  }

  function applyTheme(theme, target) {
    const element = target || document.body;
    Object.entries(cssVars(theme)).forEach(([key, value]) => element.style.setProperty(key, value));
    if (!target) {
      document.body.className = document.body.className.replace(/theme-[\w-]+/g, "").trim();
      document.body.classList.add(`theme-${theme.id}`, `appearance-${theme.appearance}`);
      const gradient = document.getElementById("orbGradient");
      if (gradient) {
        gradient.replaceChildren();
        const colors = theme.palette.gradient;
        colors.forEach((color, index) => {
          const stop = document.createElementNS("http://www.w3.org/2000/svg", "stop");
          stop.setAttribute("offset", `${index * 100 / (colors.length - 1)}%`);
          stop.setAttribute("stop-color", color);
          gradient.appendChild(stop);
        });
      }
      const pct = document.getElementById("pct-ball");
      if (pct && theme.palette.orbText) pct.style.color = theme.palette.orbText;
    }
    return theme;
  }

  const COLOR_KEYS = new Set([
    "background", "surface", "text", "secondary", "accent", "buttonText", "glow", "orbBackground", "orbText",
    "track", "border", "cardTrack", "selected", "focus", "statusNormal", "statusWarning", "statusCritical", "statusUnknown",
  ]);

  function validateOverrides(overrides) {
    if (!overrides || typeof overrides !== "object" || Array.isArray(overrides)) throw new Error("主题覆盖值必须是对象");
    Object.entries(overrides).forEach(([key, value]) => {
      if (COLOR_KEYS.has(key)) {
        if (!isHex(value)) throw new Error(`${key} 必须是 #RRGGBB 色值`);
      } else if (key === "gradient") {
        if (!Array.isArray(value) || value.length < 2 || value.length > 5 || !value.every(isHex)) throw new Error("渐变色需为 2–5 个合法色值");
      } else if (key === "particles") {
        if (!Array.isArray(value) || value.length !== 2 || !value.every(isHex)) throw new Error("粒子色需为 2 个合法色值");
      } else if (["cardOpacity", "orbOpacity"].includes(key)) {
        if (typeof value !== "number" || value < 0.75 || value > 1) throw new Error("透明度必须在 0.75–1.0 之间");
      } else if (key === "orbGlow") {
        if (typeof value !== "number" || value < 0 || value > 0.5) throw new Error("光晕必须在 0–0.5 之间");
      } else if (key === "appearance") {
        if (!["light", "dark"].includes(value)) throw new Error("明暗基调无效");
      } else throw new Error(`不支持的主题字段：${key}`);
    });
    return true;
  }

  function parseThemeText(text) {
    if (typeof text !== "string" || text.length > 16384) throw new Error("主题文本为空或超过 16KiB");
    const value = JSON.parse(text);
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("主题文本必须是对象");
    if (value.schemaVersion !== 1) throw new Error("主题文本版本不受支持");
    if (Object.keys(value).some((key) => !["schemaVersion", "name", "basePresetId", "overrides"].includes(key))) throw new Error("主题文本包含未允许字段");
    if (typeof value.name !== "string" || !value.name.trim() || [...value.name].length > 24) throw new Error("主题名称需为 1–24 个字符");
    if (typeof value.basePresetId !== "string") throw new Error("缺少基底预设");
    validateOverrides(value.overrides || {});
    return { name: value.name, basePresetId: value.basePresetId, overrides: clone(value.overrides || {}) };
  }

  class ThemeEditor {
    constructor(config) {
      this.presets = clone(config.presets);
      this.draft = clone(config.settings);
      this.history = [clone(this.draft)];
      this.index = 0;
    }

    resolved() { return resolveTheme(this.presets, this.draft); }
    custom() { return this.draft.theme.customThemes.find((item) => item.id === this.draft.theme.selectedId); }

    checkpoint() {
      this.history = this.history.slice(0, this.index + 1);
      this.history.push(clone(this.draft));
      if (this.history.length > 20) this.history.shift();
      this.index = this.history.length - 1;
    }

    selectPreset(id) {
      if (!this.presets.some((item) => item.id === id)) return;
      this.draft.theme.selectedId = id;
      this.checkpoint();
    }

    createCustom(name) {
      if (this.draft.theme.customThemes.length >= 8) throw new Error("最多保存 8 个自定义主题，请先整理已有项");
      const current = this.custom();
      const basePresetId = current ? current.basePresetId : this.draft.theme.selectedId;
      const id = `custom-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`;
      this.draft.theme.customThemes.push({
        id,
        name: (name || "我的主题").trim().slice(0, 24) || "我的主题",
        basePresetId,
        overrides: current ? clone(current.overrides) : {},
      });
      this.draft.theme.selectedId = id;
      this.checkpoint();
      return id;
    }

    setOverride(key, value) {
      if (!this.custom()) this.createCustom("我的主题");
      this.custom().overrides[key] = clone(value);
      validateOverrides(this.custom().overrides);
      this.checkpoint();
    }

    rename(name) {
      if (!this.custom()) this.createCustom(name);
      else this.custom().name = name;
      this.checkpoint();
    }

    resetField(key) {
      if (this.custom()) {
        delete this.custom().overrides[key];
        this.checkpoint();
      }
    }

    resetGroup(keys) {
      if (this.custom()) {
        keys.forEach((key) => delete this.custom().overrides[key]);
        this.checkpoint();
      }
    }

    resetBase() {
      if (this.custom()) {
        this.custom().overrides = {};
        this.checkpoint();
      }
    }

    undo() {
      if (this.index > 0) this.index -= 1;
      this.draft = clone(this.history[this.index]);
    }

    redo() {
      if (this.index < this.history.length - 1) this.index += 1;
      this.draft = clone(this.history[this.index]);
    }

    exportText() {
      const custom = this.custom();
      const theme = this.resolved();
      return JSON.stringify({
        schemaVersion: 1,
        name: custom ? custom.name : theme.label,
        basePresetId: custom ? custom.basePresetId : theme.id,
        overrides: custom ? custom.overrides : {},
      }, null, 2);
    }

    importText(text) {
      if (this.draft.theme.customThemes.length >= 8) throw new Error("最多保存 8 个自定义主题");
      const parsed = parseThemeText(text);
      if (!this.presets.some((item) => item.id === parsed.basePresetId)) throw new Error("基底预设不存在");
      const id = `custom-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`;
      this.draft.theme.customThemes.push({ id, ...parsed });
      this.draft.theme.selectedId = id;
      this.checkpoint();
      return id;
    }
  }

  const api = { clone, isHex, defaults, resolveTheme, rgba, cssVars, applyTheme, validateOverrides, parseThemeText, ThemeEditor };
  root.KQBSettings = api;
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(typeof window !== "undefined" ? window : globalThis);
