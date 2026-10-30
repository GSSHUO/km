"use strict";

const $ = (id) => document.getElementById(id);
const Q = window.KQBQuota;
const S = window.KQBSettings;
const sandbox = new URLSearchParams(location.search).get("sandbox") === "1";
const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
const clone = S.clone;

let config = null;
let settings = null;
let activeTheme = null;
let payload = null;
let scheduler = null;
let nativeVisible = true;
let ballMode = false;
let currentPanel = "quota";
let editor = null;
let savingAppearance = false;
let qrTimer = null;
let qrTicket = null;
let qrFailCount = 0;
let smsTimer = null;
let toastTimer = null;
let lastFormGeneration = -1;

function toast(message) {
  clearTimeout(toastTimer);
  $("toast").textContent = message;
  $("toast").classList.remove("hidden");
  toastTimer = setTimeout(() => $("toast").classList.add("hidden"), 2600);
}

function show(element) { element.classList.remove("hidden"); }
function hide(element) { element.classList.add("hidden"); }

function setRing(id, ratio) {
  const bar = $(`ring-${id}`);
  if (!bar) return;
  const radius = Number(bar.getAttribute("r"));
  const circumference = 2 * Math.PI * radius;
  const valid = Q.finiteRatio(ratio);
  bar.style.strokeDasharray = String(circumference);
  bar.style.strokeDashoffset = String(valid == null ? circumference : circumference * (1 - valid));
  if (id !== "ball") {
    const band = Q.usageBand(valid);
    bar.style.stroke = `var(--${band})`;
  } else {
    const point = Q.endpoint(valid);
    $("orb-tip").setAttribute("cx", point.x.toFixed(2));
    $("orb-tip").setAttribute("cy", point.y.toFixed(2));
    $("orb-tip").setAttribute("opacity", point.visible ? "1" : "0");
  }
}

function usableRatio(stat) {
  if (!stat || stat.enabled === false || stat.valueStatus && stat.valueStatus !== "valid") return null;
  return Q.finiteRatio(stat.usedRatio);
}

function render(nextPayload) {
  if (nextPayload !== undefined) payload = nextPayload;
  const data = payload && payload.ok === true ? payload : null;
  const stale = Boolean(data && (data.stale || Q.isExpired(data)));
  for (const metric of ["total", "5h", "7d"]) {
    const { stat, label } = Q.metricInfo(data, metric);
    const disabled = !stat || stat.enabled === false;
    const ratio = usableRatio(stat);
    setRing(metric, ratio);
    $(`pct-${metric}`).textContent = Q.percent(ratio);
    const row = $(`row-${metric}`);
    row.disabled = disabled;
    row.classList.toggle("disabled", disabled);
    row.classList.toggle("selected", settings && settings.orb.metric === metric);
    row.setAttribute("aria-label", `${label}${settings && settings.orb.metric === metric ? "，当前浮珠读数" : "，点击设为浮珠读数"}${disabled ? "，未启用" : ""}`);
    let reset = "—";
    if (stat) {
      if (stat.enabled === false) reset = "未启用";
      else if (stat.valueStatus && stat.valueStatus !== "valid") reset = stat.valueError || "数据异常";
      else reset = Q.formatReset(stat.resetTime, metric === "total" ? "date" : "relative");
    } else if (data) reset = "数据未知";
    $(`reset-${metric}`).textContent = reset;
    row.title = `${label} · ${reset}`;
  }

  const codeRatio = data && data.total && data.total.codeStatus !== "invalid" ? Q.finiteRatio(data.total.codeRatio) : null;
  $("codeLine").textContent = `Kimi Code · ${codeRatio == null ? "—" : Q.percent(codeRatio)}`;
  const dot = $("statusDot");
  dot.className = "dot";
  let status = "正在获取额度";
  if (data) {
    const time = Q.formatUpdated(data.fetchedAt);
    status = stale ? `上次更新 ${time} · 暂未刷新` : `更新于 ${time}`;
    if (data.error) dot.classList.add("stale");
  } else if (payload && payload.error) {
    status = "暂未获取额度";
    dot.classList.add("err");
  }
  $("statusLine").textContent = status;

  const showError = Boolean(payload && payload.error && (!data || payload.errorCategory === "auth"));
  $("errorBox").classList.toggle("hidden", !showError);
  if (showError) {
    $("errorMsg").textContent = payload.error;
    $("errorMsg").title = payload.error;
    $("loginFromErrorBtn").classList.toggle("hidden", payload.errorCategory !== "auth" && !/登录/.test(payload.error));
  }

  const orbRatio = Q.orbRatio(data, settings ? settings.orb.metric : "total");
  setRing("ball", orbRatio);
  const selected = Q.metricInfo(data, settings ? settings.orb.metric : "total");
  const orbText = orbRatio == null ? "—" : Q.percent(orbRatio);
  $("pct-ball").textContent = orbText;
  const stateMark = $("orbState");
  stateMark.className = "orb-state";
  if (stale) stateMark.classList.add("stale");
  if (payload && payload.errorCategory === "auth") stateMark.classList.add("err");
  const updated = data ? Q.formatUpdated(data.fetchedAt) : "暂无数据";
  $("ball").title = `${selected.label}已用 ${orbText} · ${stale ? "上次数据 " : "更新于 "}${updated} · 点击展开`;
  $("ball").setAttribute("aria-label", $("ball").title);
}

function applyConfig(next) {
  config = next;
  settings = clone(next.settings);
  activeTheme = S.resolveTheme(next.presets, settings);
  S.applyTheme(activeTheme);
  KQBMotion.setState({ theme: activeTheme, preset: settings.motion.preset, ball: ballMode, visible: nativeVisible });
  document.querySelectorAll("[data-orb-metric]").forEach((button) => button.classList.toggle("active", button.dataset.orbMetric === settings.orb.metric));
  render();
}

async function persistSettings(next) {
  if (sandbox) {
    applyConfig({ ...config, settings: clone(next), revision: config.revision + 1 });
    return true;
  }
  const result = await invoke("save_settings", { settings: next, expectedRevision: config.revision });
  if (!result || result.ok !== true) {
    toast(result && result.error || "设置未保存");
    return false;
  }
  applyConfig({ ...config, settings: result.settings, revision: result.revision, needsMigration: false });
  return true;
}

async function selectMetric(metric) {
  if (!Q.METRICS[metric]) return;
  const next = clone(settings);
  next.orb.metric = metric;
  settings.orb.metric = metric;
  render();
  hide($("moreMenu"));
  await persistSettings(next);
}

function setRefreshing(on) {
  $("refreshBtn").classList.toggle("spin", on);
  $("refreshBtn").disabled = on;
}

async function onQuotaResult(result, manual) {
  setRefreshing(false);
  if (result && result.staleResponse) return;
  if (result) render(result);
  if (manual) toast(result && (result.ok || result.stale) ? "额度已更新" : `刷新失败：${result && result.error || "未知错误"}`);
}

async function refresh(manual) {
  if (sandbox) return;
  setRefreshing(true);
  try { await scheduler.refresh(Boolean(manual)); }
  finally { if (!scheduler.pending) setRefreshing(false); }
}

function updateVisibility() {
  const visible = nativeVisible && !document.hidden;
  KQBMotion.setState({ visible });
  if (scheduler) scheduler.setVisible(visible);
}

window.__kqbBallMode = (on) => {
  ballMode = Boolean(on);
  document.body.classList.toggle("ball-mode", ballMode);
  if (ballMode) {
    stopQrPolling();
    ["loginPanel", "profilePanel", "appearancePanel", "diagnosticsPanel", "confirmMask", "moreMenu"].forEach((id) => hide($(id)));
    currentPanel = "quota";
    editor = null;
  }
  KQBMotion.setState({ ball: ballMode });
};

function closePanels() {
  ["loginPanel", "profilePanel", "appearancePanel", "diagnosticsPanel"].forEach((id) => hide($(id)));
  hide($("confirmMask"));
  hide($("moreMenu"));
  stopQrPolling();
}

async function openPanel(name) {
  closePanels();
  show($("quotaView"));
  const panel = $(`${name}Panel`);
  show(panel);
  hide($("quotaView"));
  currentPanel = name;
  if (!sandbox) await invoke("set_panel_layout", { layout: name });
  if (name === "login") startQrLogin();
}

async function closePanel() {
  closePanels();
  show($("quotaView"));
  currentPanel = "quota";
  editor = null;
  if (!sandbox) await invoke("set_panel_layout", { layout: "quota" });
  refreshSessionUI();
  render();
}

function toggleMore() {
  const opening = $("moreMenu").classList.contains("hidden");
  $("moreMenu").classList.toggle("hidden", !opening);
  $("moreBtn").setAttribute("aria-expanded", String(opening));
  if (!opening) hide($("metricChoices"));
}

async function refreshSessionUI() {
  if (sandbox) return { ownSession: true };
  let session = { ownSession: false };
  try { session = await invoke("session_status"); } catch (_) {}
  const loggedIn = Boolean(session && (session.ownSession || session.followingDesktop && !session.loggedOut));
  $("accountBtn").classList.toggle("active", loggedIn);
  return session;
}
window.__kqbSession = refreshSessionUI;

async function onAccountClick() {
  const session = await refreshSessionUI();
  const loggedIn = session && (session.ownSession || session.followingDesktop && !session.loggedOut);
  if (loggedIn) await openProfile(); else await openLogin();
}

async function openLogin() { await openPanel("login"); }

function stopQrPolling() {
  if (qrTimer) clearInterval(qrTimer);
  qrTimer = null;
  qrTicket = null;
}

function switchLoginTab(which) {
  $("tabQr").classList.toggle("active", which === "qr");
  $("tabSms").classList.toggle("active", which === "sms");
  $("qrPane").classList.toggle("hidden", which !== "qr");
  $("smsPane").classList.toggle("hidden", which !== "sms");
  if (which === "qr" && currentPanel === "login") startQrLogin(); else stopQrPolling();
}

function drawQr(matrix, size) {
  const canvas = $("qrCanvas");
  const context = canvas.getContext("2d");
  const quiet = 2;
  const scale = canvas.width / (size + quiet * 2);
  context.fillStyle = "#fff"; context.fillRect(0, 0, canvas.width, canvas.height);
  context.fillStyle = "#16121f";
  const lines = matrix.trim().split("\n");
  for (let y = 0; y < size; y += 1) for (let x = 0; x < size; x += 1) {
    if (lines[y] && lines[y][x] === "1") context.fillRect((x + quiet) * scale, (y + quiet) * scale, scale, scale);
  }
}

async function startQrLogin() {
  if (sandbox || currentPanel !== "login" || $("qrPane").classList.contains("hidden")) return;
  stopQrPolling();
  qrFailCount = 0;
  hide($("qrRefreshBtn"));
  $("qrStatus").textContent = "正在生成二维码…";
  let result;
  try { result = await invoke("login_qr_create"); } catch (error) { result = { ok: false, error: String(error) }; }
  if (!result || result.ok !== true) {
    $("qrStatus").textContent = `生成失败：${result && result.error || "未知错误"}`;
    show($("qrRefreshBtn"));
    return;
  }
  qrTicket = result.code;
  drawQr(result.matrix, result.size);
  $("qrStatus").textContent = "等待扫码…";
  qrTimer = setInterval(pollQr, 2000);
}

async function pollQr() {
  if (!qrTicket || currentPanel !== "login") return;
  let result;
  try { result = await invoke("login_qr_poll", { code: qrTicket }); } catch (error) { result = { ok: false, error: String(error) }; }
  if (!result || result.ok !== true) {
    qrFailCount += 1;
    if (qrFailCount >= 5) $("qrStatus").textContent = `连接异常：${result && result.error || "未知错误"}（重试中…）`;
    return;
  }
  qrFailCount = 0;
  if (result.status === "STATUS_SCANNED") $("qrStatus").textContent = "已扫码，请在手机上确认登录…";
  else if (result.status === "STATUS_EXPIRED") { stopQrPolling(); $("qrStatus").textContent = "二维码已过期"; show($("qrRefreshBtn")); }
  else if (result.status === "STATUS_SUCCESS") {
    stopQrPolling();
    $("qrStatus").textContent = "登录成功！";
    await refreshSessionUI();
    setTimeout(closePanel, 500);
  }
}

function setSmsError(text) { $("smsError").textContent = text || ""; }

async function sendSmsCode() {
  const countryCode = $("smsCC").value.trim().replace(/\D/g, "") || "86";
  const number = $("smsPhone").value.trim();
  if (!/^\d{5,15}$/.test(number)) return setSmsError("请输入正确的手机号");
  $("smsSendBtn").disabled = true;
  setSmsError("");
  let result;
  try { result = await invoke("login_sms_send", { countryCode, number }); } catch (error) { result = { ok: false, error: String(error) }; }
  if (result && result.ok) startSmsCountdown(60);
  else {
    $("smsSendBtn").disabled = false;
    const error = result && result.error || "发送失败";
    setSmsError(/CAPTCHA/i.test(error) ? "服务端要求人机验证，请使用下方官方网页登录" : error);
  }
}

function startSmsCountdown(seconds) {
  clearInterval(smsTimer);
  let left = seconds;
  $("smsSendBtn").textContent = `${left}s 后重发`;
  smsTimer = setInterval(() => {
    left -= 1;
    if (left <= 0) {
      clearInterval(smsTimer); smsTimer = null;
      $("smsSendBtn").disabled = false; $("smsSendBtn").textContent = "发送验证码";
    } else $("smsSendBtn").textContent = `${left}s 后重发`;
  }, 1000);
}

async function submitSmsLogin() {
  const countryCode = $("smsCC").value.trim().replace(/\D/g, "") || "86";
  const number = $("smsPhone").value.trim();
  const verifyCode = $("smsCode").value.trim();
  if (!/^\d{4,8}$/.test(verifyCode)) return setSmsError("请输入短信验证码");
  $("smsLoginBtn").disabled = true;
  let result;
  try { result = await invoke("login_sms_verify", { countryCode, number, verifyCode }); } catch (error) { result = { ok: false, error: String(error) }; }
  $("smsLoginBtn").disabled = false;
  if (result && result.ok) { await refreshSessionUI(); await closePanel(); refresh(true); }
  else setSmsError(result && result.error || "登录失败");
}

const LEVEL_NAMES = { LEVEL_FREE: "免费版", LEVEL_BASIC: "基础会员", LEVEL_INTERMEDIATE: "中级会员", LEVEL_ADVANCED: "高级会员" };
const SOURCE_NAMES = { qr: "扫码登录", sms: "手机验证码登录", web: "网页登录" };

function formatDate(iso) {
  const date = new Date(iso);
  if (!iso || Number.isNaN(date.getTime())) return "—";
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}

async function openProfile() {
  await openPanel("profile");
  $("profileUid").textContent = "加载中…";
  let result;
  try { result = await invoke("get_profile"); } catch (error) { result = { error: String(error) }; }
  const uid = result && result.userId ? String(result.userId) : "";
  $("profileAvatar").textContent = (uid[0] || "K").toUpperCase();
  $("profileUid").textContent = uid.length > 12 ? `${uid.slice(0, 6)}…${uid.slice(-4)}` : uid || "未知用户";
  $("profileSource").textContent = result && result.ownSession ? SOURCE_NAMES[result.source] || "本卡片登录" : "跟随 Kimi 桌面客户端";
  const membership = result && result.membership;
  if (membership) {
    const level = LEVEL_NAMES[membership.level] || membership.level || "";
    $("profilePlan").textContent = `${membership.plan || "—"}${level ? ` · ${level}` : ""}`;
    $("profileStatus").textContent = membership.active || membership.status === "SUBSCRIPTION_STATUS_ACTIVE" ? "生效中" : membership.status || "—";
    $("profileExpire").textContent = formatDate(membership.periodEnd);
  } else {
    $("profilePlan").textContent = "—"; $("profileStatus").textContent = "—"; $("profileExpire").textContent = "—";
  }
}

function updatePreview() {
  if (!editor) return;
  const theme = editor.resolved();
  const preview = $("themePreview");
  S.applyTheme(theme, preview);
  preview.style.setProperty("--preview-bg", theme.palette.background);
  preview.style.setProperty("--preview-surface", theme.palette.surface);
  preview.style.setProperty("--preview-text", theme.palette.text);
  preview.style.setProperty("--preview-accent", theme.palette.accent);
  $("customThemeName").value = editor.custom() ? editor.custom().name : theme.label;
  $("undoBtn").disabled = editor.index <= 0;
  $("redoBtn").disabled = editor.index >= editor.history.length - 1;
}

function renderPresetLists() {
  const presetGrid = $("presetGrid");
  presetGrid.replaceChildren();
  editor.presets.forEach((preset) => {
    const button = document.createElement("button");
    button.className = `preset-item${editor.draft.theme.selectedId === preset.id ? " active" : ""}`;
    const swatch = document.createElement("span"); swatch.className = "swatch";
    swatch.style.setProperty("--swatch-a", preset.palette.gradient[0]);
    swatch.style.setProperty("--swatch-b", preset.palette.gradient[preset.palette.gradient.length - 1]);
    const label = document.createElement("span"); label.textContent = preset.label;
    button.append(swatch, label);
    button.addEventListener("click", () => { editor.selectPreset(preset.id); renderAppearance(); });
    presetGrid.appendChild(button);
  });
  const customList = $("customList");
  customList.replaceChildren();
  if (!editor.draft.theme.customThemes.length) {
    const empty = document.createElement("div"); empty.className = "empty-state"; empty.textContent = "尚未保存自定义主题"; customList.appendChild(empty);
  }
  editor.draft.theme.customThemes.forEach((custom) => {
    const button = document.createElement("button"); button.className = `custom-item${editor.draft.theme.selectedId === custom.id ? " active" : ""}`;
    const base = editor.presets.find((item) => item.id === custom.basePresetId);
    const swatch = document.createElement("span"); swatch.className = "swatch";
    swatch.style.setProperty("--swatch-a", custom.overrides.accent || base.palette.accent);
    swatch.style.setProperty("--swatch-b", custom.overrides.background || base.palette.background);
    const label = document.createElement("span"); label.textContent = custom.name;
    button.append(swatch, label);
    button.addEventListener("click", () => { editor.draft.theme.selectedId = custom.id; editor.checkpoint(); renderAppearance(); });
    customList.appendChild(button);
  });
}

const FIELD_GROUPS = [
  { id: "overall", label: "整体", colors: [["accent", "强调色"]], ranges: [["cardOpacity", "卡片透明度", .75, 1, .01], ["orbOpacity", "浮珠透明度", .75, 1, .01]], appearance: true },
  { id: "orb", label: "浮珠", colors: [["orbBackground", "浮珠底色"], ["track", "轨道"], ["orbText", "数字"], ["glow", "光晕色"]], ranges: [["orbGlow", "光晕强度", 0, .5, .01]], gradient: true, particles: true },
  { id: "card", label: "卡片", colors: [["background", "背景"], ["surface", "Surface"], ["border", "边线"], ["text", "主文字"], ["secondary", "副文字"], ["cardTrack", "额度环轨道"]] },
  { id: "controls", label: "控件", colors: [["accent", "按钮强调"], ["buttonText", "按钮字色"], ["selected", "选中底色"], ["focus", "焦点色"]] },
  { id: "status", label: "状态", colors: [["statusNormal", "正常"], ["statusWarning", "注意"], ["statusCritical", "接近上限"], ["statusUnknown", "未知"]] },
];

function colorField(key, label, value) {
  const row = document.createElement("div"); row.className = "color-field";
  const color = document.createElement("input"); color.type = "color"; color.value = value; color.setAttribute("aria-label", label);
  const name = document.createElement("label"); name.textContent = label;
  const hex = document.createElement("input"); hex.type = "text"; hex.value = value; hex.maxLength = 7; hex.setAttribute("aria-label", `${label} HEX`);
  const update = (next) => {
    if (!S.isHex(next)) return;
    editor.setOverride(key, next.toUpperCase());
    color.value = next; hex.value = next.toUpperCase(); updatePreview();
  };
  color.addEventListener("input", () => update(color.value));
  hex.addEventListener("change", () => { if (S.isHex(hex.value)) update(hex.value); else { toast("请输入 #RRGGBB 色值"); hex.value = value; } });
  row.append(color, name, hex); return row;
}

function rangeField(key, label, min, max, step, value) {
  const row = document.createElement("div"); row.className = "range-field";
  const name = document.createElement("label"); name.textContent = label;
  const input = document.createElement("input"); Object.assign(input, { type: "range", min, max, step, value });
  const output = document.createElement("span"); output.textContent = Number(value).toFixed(2);
  input.addEventListener("input", () => { const number = Number(input.value); output.textContent = number.toFixed(2); editor.setOverride(key, number); updatePreview(); });
  row.append(name, input, output); return row;
}

function renderThemeFields() {
  const container = $("themeFields"); container.replaceChildren();
  const theme = editor.resolved();
  const custom = editor.custom();
  if (!custom) {
    const note = document.createElement("div"); note.className = "empty-state"; note.textContent = "修改任一字段时会自动从当前预设创建“我的主题”"; container.appendChild(note);
  }
  FIELD_GROUPS.forEach((group) => {
    const section = document.createElement("section"); section.className = "field-group";
    const header = document.createElement("div"); header.className = "field-group-header";
    const title = document.createElement("b"); title.textContent = group.label;
    const reset = document.createElement("button"); reset.textContent = "还原本组";
    const keys = [...group.colors.map(([key]) => key), ...(group.ranges || []).map(([key]) => key)];
    if (group.gradient) keys.push("gradient"); if (group.particles) keys.push("particles"); if (group.appearance) keys.push("appearance");
    reset.addEventListener("click", () => { editor.resetGroup(keys); renderAppearance(); });
    header.append(title, reset); section.appendChild(header);
    if (group.appearance) {
      const label = document.createElement("label"); label.className = "field-label"; label.textContent = "明 / 暗基调";
      const select = document.createElement("select");
      [["dark", "深色"], ["light", "浅色"]].forEach(([value, text]) => { const option = document.createElement("option"); option.value = value; option.textContent = text; select.appendChild(option); });
      select.value = theme.appearance;
      select.addEventListener("change", () => { editor.setOverride("appearance", select.value); renderAppearance(); });
      label.appendChild(select); section.appendChild(label);
    }
    group.colors.forEach(([key, label]) => {
      let value;
      if (key.startsWith("status")) value = theme.status[key.replace("status", "").toLowerCase()];
      else if (key === "orbBackground") value = theme.palette.orbBackground || theme.palette.background;
      else value = theme.palette[key] || theme.palette.text;
      section.appendChild(colorField(key, label, value));
    });
    (group.ranges || []).forEach(([key, label, min, max, step]) => section.appendChild(rangeField(key, label, min, max, step, theme.material[key])));
    if (group.gradient) {
      const title = document.createElement("div"); title.className = "section-title"; title.textContent = "渐变（2–5 色，等距停点）"; section.appendChild(title);
      theme.palette.gradient.forEach((value, index) => {
        const row = document.createElement("div"); row.className = "gradient-row";
        const input = document.createElement("input"); input.type = "color"; input.value = value;
        input.addEventListener("input", () => { const colors = [...editor.resolved().palette.gradient]; colors[index] = input.value.toUpperCase(); editor.setOverride("gradient", colors); updatePreview(); });
        const label = document.createElement("span"); label.textContent = `${Math.round(index * 100 / (theme.palette.gradient.length - 1))}% · ${value}`;
        const remove = document.createElement("button"); remove.textContent = "删除"; remove.disabled = theme.palette.gradient.length <= 2;
        remove.addEventListener("click", () => { const colors = [...editor.resolved().palette.gradient]; colors.splice(index, 1); editor.setOverride("gradient", colors); renderAppearance(); });
        row.append(input, label, remove); section.appendChild(row);
      });
      const add = document.createElement("button"); add.className = "secondary-btn"; add.textContent = "添加渐变色"; add.disabled = theme.palette.gradient.length >= 5;
      add.addEventListener("click", () => { editor.setOverride("gradient", [...editor.resolved().palette.gradient, theme.palette.accent]); renderAppearance(); }); section.appendChild(add);
    }
    if (group.particles) {
      const title = document.createElement("div"); title.className = "section-title"; title.textContent = "星尘粒子（2 色 + 主文字色）"; section.appendChild(title);
      theme.palette.particles.forEach((value, index) => section.appendChild(colorField("particles", `粒子色 ${index + 1}`, value)));
      const rows = [...section.querySelectorAll(".color-field")].slice(-2);
      rows.forEach((row, index) => {
        const color = row.querySelector('input[type="color"]'); const hex = row.querySelector('input[type="text"]');
        color.replaceWith(color.cloneNode(true)); hex.replaceWith(hex.cloneNode(true));
        const freshColor = row.querySelector('input[type="color"]'); const freshHex = row.querySelector('input[type="text"]');
        const update = (next) => { if (!S.isHex(next)) return; const colors = [...editor.resolved().palette.particles]; colors[index] = next.toUpperCase(); editor.setOverride("particles", colors); renderAppearance(); };
        freshColor.addEventListener("input", () => update(freshColor.value)); freshHex.addEventListener("change", () => update(freshHex.value));
      });
    }
    container.appendChild(section);
  });
}

function renderAppearance() {
  if (!editor) return;
  renderPresetLists(); renderThemeFields(); updatePreview();
  document.querySelectorAll('input[name="motion"]').forEach((input) => { input.checked = input.value === editor.draft.motion.preset; });
  $("materialMode").value = editor.draft.material.mode;
}

async function openAppearance() {
  editor = new S.ThemeEditor(config);
  await openPanel("appearance");
  renderAppearance();
}

async function closeAppearance() {
  if (savingAppearance) return;
  editor = null;
  await closePanel();
}

async function saveAppearance() {
  if (!editor || savingAppearance) return;
  savingAppearance = true;
  const ok = await persistSettings(editor.draft);
  savingAppearance = false;
  if (ok) { toast("外观已保存"); editor = null; await closePanel(); }
}

async function openDiagnostics() {
  await openPanel("diagnostics");
  KQBMotion.resetStats();
  let result;
  try { result = await invoke("get_diagnostics"); } catch (error) { result = { error: String(error) }; }
  result.frontendMotion = KQBMotion.getStats();
  $("diagnosticDetails").textContent = JSON.stringify(result, null, 2);
  const summary = $("diagnosticSummary"); summary.replaceChildren();
  const values = [
    `应用版本 ${result.appVersion || "未知"}`,
    `材质 ${result.window && result.window.material && result.window.material.mode || "未知"}`,
    `尺寸 ${result.window && result.window.logicalSize ? `${result.window.logicalSize.width}×${result.window.logicalSize.height}` : "未知"} · DPI ${result.window && result.window.scaleFactor || "—"}`,
    `主题 ${result.theme || activeTheme.label} · 动效 ${result.motion || settings.motion.preset}`,
    `最后成功 ${result.quota && result.quota.lastSuccessAt ? Q.formatUpdated(result.quota.lastSuccessAt, true) : "—"}`,
  ];
  values.forEach((value) => { const line = document.createElement("div"); line.textContent = value; summary.appendChild(line); });
}

function bindAppearance() {
  document.querySelectorAll("[data-appearance-tab]").forEach((button) => button.addEventListener("click", () => {
    document.querySelectorAll("[data-appearance-tab]").forEach((item) => item.classList.toggle("active", item === button));
    for (const name of ["Theme", "Parts", "Motion"]) $(`appearance${name}Tab`).classList.toggle("hidden", button.dataset.appearanceTab !== name.toLowerCase());
  }));
  $("appearanceBackBtn").addEventListener("click", closeAppearance);
  $("cancelAppearanceBtn").addEventListener("click", closeAppearance);
  $("saveAppearanceBtn").addEventListener("click", saveAppearance);
  $("saveAsThemeBtn").addEventListener("click", () => { try { editor.createCustom($("customThemeName").value); renderAppearance(); } catch (error) { toast(error.message); } });
  $("customThemeName").addEventListener("change", () => { editor.rename($("customThemeName").value); renderAppearance(); });
  $("undoBtn").addEventListener("click", () => { editor.undo(); renderAppearance(); });
  $("redoBtn").addEventListener("click", () => { editor.redo(); renderAppearance(); });
  $("resetBaseBtn").addEventListener("click", () => { editor.resetBase(); renderAppearance(); });
  $("exportThemeBtn").addEventListener("click", async () => {
    const text = editor.exportText(); $("themeText").value = text;
    if (!sandbox) { const result = await invoke("copy_theme_text", { text }); toast(result.ok ? "主题文本已复制" : result.error); }
  });
  $("importThemeBtn").addEventListener("click", () => { try { editor.importText($("themeText").value); renderAppearance(); toast("主题已导入草稿，保存后生效"); } catch (error) { toast(error.message); } });
  document.querySelectorAll('input[name="motion"]').forEach((input) => input.addEventListener("change", () => { editor.draft.motion.preset = input.value; editor.checkpoint(); updatePreview(); }));
  $("materialMode").addEventListener("change", () => { editor.draft.material.mode = $("materialMode").value; editor.checkpoint(); });
}

function bindEvents() {
  $("refreshBtn").addEventListener("click", () => refresh(true));
  $("retryBtn").addEventListener("click", () => refresh(true));
  $("accountBtn").addEventListener("click", onAccountClick);
  $("loginFromErrorBtn").addEventListener("click", openLogin);
  $("moreBtn").addEventListener("click", (event) => { event.stopPropagation(); toggleMore(); });
  $("hideCardBtn").addEventListener("click", () => invoke("hide_card"));
  $("appearanceBtn").addEventListener("click", openAppearance);
  $("diagnosticsBtn").addEventListener("click", openDiagnostics);
  $("diagnosticsBackBtn").addEventListener("click", closePanel);
  $("orbDisplayBtn").addEventListener("click", () => {
    const opening = $("metricChoices").classList.contains("hidden");
    $("metricChoices").classList.toggle("hidden", !opening);
    $("orbDisplayBtn").setAttribute("aria-expanded", String(opening));
  });
  document.querySelectorAll("[data-orb-metric]").forEach((button) => button.addEventListener("click", () => selectMetric(button.dataset.orbMetric)));
  document.querySelectorAll(".quota-row").forEach((row) => row.addEventListener("click", () => selectMetric(row.dataset.metric)));
  $("copySummaryBtn").addEventListener("click", async () => { hide($("moreMenu")); const result = await invoke("copy_quota_summary"); toast(result.ok ? "额度摘要已复制" : result.error); });
  $("copyDiagnosticsBtn").addEventListener("click", async () => { const result = await invoke("copy_diagnostics"); toast(result.ok ? "脱敏诊断已复制" : result.error); });
  $("dockBtn").addEventListener("click", () => invoke("dock_ball"));
  $("loginCloseBtn").addEventListener("click", closePanel);
  $("profileCloseBtn").addEventListener("click", closePanel);
  $("tabQr").addEventListener("click", () => switchLoginTab("qr"));
  $("tabSms").addEventListener("click", () => switchLoginTab("sms"));
  $("qrRefreshBtn").addEventListener("click", startQrLogin);
  $("smsSendBtn").addEventListener("click", sendSmsCode);
  $("smsLoginBtn").addEventListener("click", submitSmsLogin);
  $("webLoginBtn").addEventListener("click", () => invoke("open_web_login"));
  $("logoutBtn").addEventListener("click", async () => {
    const session = await refreshSessionUI();
    $("confirmText").textContent = session && session.ownSession ? "退出后卡片将显示未登录状态。" : "仅退出本卡片的登录，Kimi 桌面客户端不受影响。";
    show($("confirmMask")); $("confirmCancelBtn").focus();
  });
  $("confirmCancelBtn").addEventListener("click", () => hide($("confirmMask")));
  $("confirmOkBtn").addEventListener("click", async () => {
    hide($("confirmMask")); await invoke("logout"); payload = null; render(); await closePanel(); refresh(true);
  });
  document.addEventListener("click", (event) => { if (!event.target.closest("#moreMenu, #moreBtn")) hide($("moreMenu")); });
  document.querySelectorAll(".drag-region").forEach((region) => region.addEventListener("mousedown", (event) => {
    if (event.button === 0 && !event.target.closest("button, input, .quota-row")) invoke("start_drag");
  }));
  document.querySelector(".title").addEventListener("dblclick", () => invoke("dock_ball"));
  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    if (!$("confirmMask").classList.contains("hidden")) hide($("confirmMask"));
    else if (!$("moreMenu").classList.contains("hidden")) hide($("moreMenu"));
    else if (currentPanel === "appearance") closeAppearance();
    else if (currentPanel !== "quota") closePanel();
    else if (!ballMode) invoke("hide_card");
  });
  document.addEventListener("visibilitychange", updateVisibility);
  bindAppearance();
}

function bindBallGestures() {
  let down = null;
  let dragging = false;
  $("ball").addEventListener("mousedown", (event) => { if (event.button === 0) { down = { x: event.clientX, y: event.clientY }; dragging = false; } });
  $("ball").addEventListener("mousemove", (event) => {
    if (down && !dragging && Math.hypot(event.clientX - down.x, event.clientY - down.y) > 6) { dragging = true; invoke("start_drag"); }
  });
  window.addEventListener("mouseup", () => { down = null; });
  $("ball").addEventListener("click", () => { if (!dragging) invoke("expand_card"); dragging = false; });
  $("ball").addEventListener("keydown", (event) => {
    if (["Enter", " "].includes(event.key)) { event.preventDefault(); invoke("expand_card"); }
  });
}

async function listenNative() {
  if (sandbox) return;
  await window.__TAURI__.event.listen("settings-changed", (event) => {
    if (currentPanel === "appearance" && !savingAppearance) {
      editor = null; closePanel(); toast("设置已在其他入口变化，未保存草稿已放弃");
    }
    applyConfig(event.payload);
  });
  const handleForm = (data) => {
    if (typeof data.visible === "boolean") { nativeVisible = data.visible; updateVisibility(); }
    if (data.generation != null && data.generation === lastFormGeneration) return;
    if (data.generation != null) lastFormGeneration = data.generation;
    window.__kqbBallMode(data.form === "ball");
  };
  await window.__TAURI__.event.listen("app-visibility", (event) => handleForm(event.payload));
  await window.__TAURI__.event.listen("form-changed", (event) => handleForm(event.payload));
  await window.__TAURI__.event.listen("session-changed", () => { payload = null; render(); refreshSessionUI(); refresh(false); });
}

async function init() {
  bindEvents(); bindBallGestures();
  if (sandbox) {
    document.body.classList.add("sandbox");
    const presets = await fetch("themes.json").then((response) => response.json());
    config = { presets, settings: S.defaults(), revision: 0 };
    applyConfig(config);
    render(null);
  } else {
    config = await invoke("get_ui_config");
    if (config.needsMigration) {
      let legacy = {};
      try {
        legacy = {
          theme: localStorage.getItem("kqb-orb-theme"),
          metric: localStorage.getItem("kqb-orb-metric"),
          showcase: localStorage.getItem("kqb-orb-showcase") === "1",
        };
      } catch (_) {}
      await invoke("migrate_settings", legacy);
      config = await invoke("get_ui_config");
    }
    applyConfig(config);
    if (config.loadError) toast(config.loadError);
    await listenNative();
    refreshSessionUI();
    scheduler = new Q.Scheduler(() => invoke("get_quota"), onQuotaResult, () => nativeVisible && !document.hidden);
    scheduler.start();
  }
  setInterval(() => render(), 60000);
}

window.__kqbRender = render;
window.__kqbPoll = refresh;
window.__kqbSetTheme = async (id) => { const next = clone(settings); next.theme.selectedId = id; return persistSettings(next); };
window.__kqbSetMetric = selectMetric;
window.__kqbSetShowcase = async (on) => { const next = clone(settings); next.motion.preset = on ? "stardust" : "lively"; return persistSettings(next); };
window.__kqbDebug = {
  getState: () => ({ config, settings, payload, ballMode, currentPanel, motion: KQBMotion.getStats() }),
  openAppearance,
  openDiagnostics,
  selectMetric,
  applyConfig,
};

window.addEventListener("message", (event) => {
  if (!sandbox || !event.data || event.data.type !== "kqb-fixture") return;
  if (event.data.theme) {
    const next = clone(settings); next.theme.selectedId = event.data.theme; applyConfig({ ...config, settings: next });
  }
  if (event.data.motion) KQBMotion.setState({ preset: event.data.motion });
  render(event.data.payload);
});
window.addEventListener("error", (event) => { console.error(event.error || event.message); if (sandbox) $("statusLine").textContent = `演示错误：${event.message}`; });
init().catch((error) => { console.error(error); $("statusLine").textContent = `初始化失败：${error.message || error}`; toast("初始化失败，请重新打开应用"); });
