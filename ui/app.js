/* Kimi Quota Bar — card renderer.
   The card polls the native side via invoke("get_quota") every 60 s. */

function usageColor(ratio) {
  if (ratio == null) return "#64748b";
  if (ratio >= 0.8) return "#f87171";
  if (ratio >= 0.5) return "#fbbf24";
  return "#34d399";
}

function setRing(id, ratio) {
  const bar = document.getElementById("ring-" + id);
  const pct = document.getElementById("pct-" + id);
  if (!bar || !pct) return;
  const r = Number(bar.getAttribute("r")) || 20;
  const c = 2 * Math.PI * r;
  bar.style.strokeDasharray = c;
  if (ratio == null) {
    bar.style.strokeDashoffset = c;
    pct.textContent = "–";
    // 球面弧末亮点（观赏模式）同步熄灭
    if (id === "ball") setOrbTip(null);
    return;
  }
  const clamped = Math.max(0, Math.min(1, ratio));
  bar.style.strokeDashoffset = c * (1 - clamped);
  // 球体进度环统一用极光渐变（CSS url(#orbGrad)），不按用量变色；
  // 卡片三环仍按用量着色。
  if (id !== "ball") bar.style.stroke = usageColor(clamped);
  // 观赏模式弧末亮点跟随球面进度（关闭时 CSS 隐藏，更新无副作用）
  if (id === "ball") setOrbTip(clamped);
  pct.textContent = (clamped * 100).toFixed(2) + "%";
}

function setRowDisabled(id, disabled) {
  const row = document.getElementById("ring-" + id).closest(".row");
  row.classList.toggle("disabled", disabled);
}

function fmtReset(iso, mode) {
  if (!iso) return "重置时间未知";
  const t = new Date(iso);
  if (isNaN(t)) return "重置时间未知";
  const diffMs = t.getTime() - Date.now();
  if (diffMs <= 0) return "即将重置…";
  const mins = Math.floor(diffMs / 60000);
  const hours = Math.floor(mins / 60);
  const days = Math.floor(hours / 24);
  if (mode === "date" || days > 6) {
    return `${t.getMonth() + 1}/${String(t.getDate()).padStart(2, "0")} 重置`;
  }
  if (days >= 1) return `${days} 天 ${hours % 24} 小时后重置`;
  if (hours >= 1) return `${hours} 小时 ${mins % 60} 分后重置`;
  return `${mins} 分钟后重置`;
}

function fmtUpdated(unixSecs) {
  const d = new Date(unixSecs * 1000);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

function render(payload) {
  lastPayload = payload; // 供悬浮球口径切换后立即重绘（declared with the metric block）
  const dot = document.getElementById("statusDot");
  const rows = document.getElementById("rows");
  const errorBox = document.getElementById("errorBox");
  const footer = document.getElementById("footer");

  if (!payload || payload.ok !== true) {
    dot.classList.add("err");
    rows.classList.add("hidden");
    errorBox.classList.remove("hidden");
    document.getElementById("errorMsg").textContent =
      (payload && payload.error) || "未知错误";
    footer.textContent = "点击重试或稍候自动重试";
    return;
  }

  dot.classList.remove("err");
  errorBox.classList.add("hidden");
  rows.classList.remove("hidden");

  const five = payload.fiveHour || {};
  const seven = payload.sevenDay || {};
  const fiveOff = five.enabled === false;
  const sevenOff = seven.enabled === false;

  setRing("total", payload.total && payload.total.usedRatio);
  // 球面进度按当前口径：总量（默认）或 5 小时窗口（未启用则置空）
  setRing("ball", orbMetric === "5h"
    ? (fiveOff ? null : five.usedRatio)
    : payload.total && payload.total.usedRatio);
  setRing("5h", fiveOff ? null : five.usedRatio);
  setRing("7d", sevenOff ? null : seven.usedRatio);
  setRowDisabled("5h", fiveOff);
  setRowDisabled("7d", sevenOff);

  document.getElementById("reset-total").textContent =
    fmtReset(payload.total && payload.total.resetTime, "date");
  document.getElementById("reset-5h").textContent = fiveOff
    ? "未启用"
    : fmtReset(five.resetTime, "rel");
  document.getElementById("reset-7d").textContent = sevenOff
    ? "未启用"
    : fmtReset(seven.resetTime, "rel");

  document.getElementById("updatedAt").textContent = payload.fetchedAt
    ? fmtUpdated(payload.fetchedAt)
    : "--";

  const code = payload.total && payload.total.codeRatio;
  footer.textContent =
    code != null
      ? `其中 Kimi Code 占 ${(code * 100).toFixed(2)}% · 每 60 秒自动刷新`
      : "每 60 秒自动刷新";
}

async function poll() {
  try {
    const payload = await window.__TAURI__.core.invoke("get_quota");
    render(payload);
  } catch (e) {
    render({ ok: false, error: "内部调用失败: " + String(e) });
  }
}
window.__kqbPoll = poll;

function manualRefresh() {
  const btn = document.getElementById("refreshBtn");
  btn.classList.add("spin");
  setTimeout(() => btn.classList.remove("spin"), 1200);
  poll();
}

// ---------- login panel ----------

const CARD_H = 224;
const LOGIN_H = 400;
const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);

let qrTimer = null;
let qrTicket = null;
let qrFailCount = 0;
let smsTimer = null;

function $(id) { return document.getElementById(id); }
function showEl(el) { el.classList.remove("hidden"); }
function hideEl(el) { el.classList.add("hidden"); }

async function openLogin() {
  showEl($("loginPanel"));
  await invoke("set_card_height", { height: LOGIN_H });
  startQrLogin();
}

async function closeLogin() {
  stopQrPolling();
  hideEl($("loginPanel"));
  await invoke("set_card_height", { height: CARD_H });
  refreshSessionUI();
  poll(); // refresh card (picks up a brand-new session right away)
}

function switchLoginTab(which) {
  $("tabQr").classList.toggle("active", which === "qr");
  $("tabSms").classList.toggle("active", which === "sms");
  $("qrPane").classList.toggle("hidden", which !== "qr");
  $("smsPane").classList.toggle("hidden", which !== "sms");
}

// --- QR login ---

function stopQrPolling() {
  if (qrTimer) { clearInterval(qrTimer); qrTimer = null; }
  qrTicket = null;
}

function setQrStatus(text) { $("qrStatus").textContent = text; }

function drawQr(matrix, size) {
  const canvas = $("qrCanvas");
  const ctx = canvas.getContext("2d");
  const quiet = 2;
  const total = size + quiet * 2;
  const scale = canvas.width / total;
  ctx.fillStyle = "#ffffff";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.fillStyle = "#16121f";
  const lines = matrix.trim().split("\n");
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      if (lines[y] && lines[y][x] === "1") {
        ctx.fillRect((x + quiet) * scale, (y + quiet) * scale, scale, scale);
      }
    }
  }
}

async function startQrLogin() {
  stopQrPolling();
  qrFailCount = 0;
  hideEl($("qrRefreshBtn"));
  setQrStatus("正在生成二维码…");
  let r;
  try {
    r = await invoke("login_qr_create");
  } catch (e) {
    r = { ok: false, error: String(e) };
  }
  if (!r || r.ok !== true) {
    setQrStatus("生成失败：" + ((r && r.error) || "未知错误"));
    showEl($("qrRefreshBtn"));
    return;
  }
  qrTicket = r.code;
  drawQr(r.matrix, r.size);
  setQrStatus("等待扫码…");
  qrTimer = setInterval(pollQr, 2000);
}

async function pollQr() {
  if (!qrTicket) return;
  let r;
  try {
    r = await invoke("login_qr_poll", { code: qrTicket });
  } catch (e) {
    r = { ok: false, error: String(e) };
  }
  if (!r || r.ok !== true) {
    // Keep polling, but a persistent failure (e.g. session save error)
    // must surface instead of leaving the panel stuck on "等待扫码…".
    qrFailCount += 1;
    if (qrFailCount >= 5) {
      setQrStatus("连接异常：" + ((r && r.error) || "未知错误") + "（重试中…）");
    }
    return;
  }
  qrFailCount = 0;
  if (r.status === "STATUS_SCANNED") {
    setQrStatus("已扫码，请在手机上确认登录…");
  } else if (r.status === "STATUS_EXPIRED") {
    stopQrPolling();
    setQrStatus("二维码已过期");
    showEl($("qrRefreshBtn"));
  } else if (r.status === "STATUS_SUCCESS") {
    stopQrPolling();
    setQrStatus("登录成功！");
    await refreshSessionUI();
    setTimeout(closeLogin, 600);
  }
}

// --- SMS login ---

function setSmsError(text) { $("smsError").textContent = text || ""; }

function mapSmsSendError(err) {
  if (/CAPTCHA/i.test(err)) {
    return "服务端要求人机验证，请点下方「官方网页登录」完成短信验证";
  }
  return err;
}

async function sendSmsCode() {
  const cc = $("smsCC").value.trim().replace(/\D/g, "") || "86";
  const phone = $("smsPhone").value.trim();
  if (!/^\d{5,15}$/.test(phone)) {
    setSmsError("请输入正确的手机号");
    return;
  }
  $("smsSendBtn").disabled = true;
  setSmsError("");
  let r;
  try {
    r = await invoke("login_sms_send", { countryCode: cc, number: phone });
  } catch (e) {
    r = { ok: false, error: String(e) };
  }
  if (r && r.ok === true) {
    startSmsCountdown(60);
  } else {
    $("smsSendBtn").disabled = false;
    setSmsError(mapSmsSendError((r && r.error) || "发送失败"));
  }
}

function startSmsCountdown(secs) {
  if (smsTimer) clearInterval(smsTimer);
  let left = secs;
  const btn = $("smsSendBtn");
  btn.disabled = true;
  btn.textContent = `${left}s 后重发`;
  smsTimer = setInterval(() => {
    left -= 1;
    if (left <= 0) {
      clearInterval(smsTimer);
      smsTimer = null;
      btn.disabled = false;
      btn.textContent = "发送验证码";
    } else {
      btn.textContent = `${left}s 后重发`;
    }
  }, 1000);
}

async function submitSmsLogin() {
  const cc = $("smsCC").value.trim().replace(/\D/g, "") || "86";
  const phone = $("smsPhone").value.trim();
  const code = $("smsCode").value.trim();
  if (!/^\d{4,8}$/.test(code)) {
    setSmsError("请输入短信验证码");
    return;
  }
  $("smsLoginBtn").disabled = true;
  setSmsError("");
  let r;
  try {
    r = await invoke("login_sms_verify", { countryCode: cc, number: phone, verifyCode: code });
  } catch (e) {
    r = { ok: false, error: String(e) };
  }
  $("smsLoginBtn").disabled = false;
  if (r && r.ok === true) {
    await refreshSessionUI();
    closeLogin();
  } else {
    setSmsError((r && r.error) || "登录失败");
  }
}

// --- account state ---

async function refreshSessionUI() {
  let s = { ownSession: false };
  try {
    s = await invoke("session_status");
  } catch (e) { /* keep default */ }
  const loggedIn = !!(s && (s.ownSession || (s.followingDesktop && !s.loggedOut)));
  $("accountBtn").classList.toggle("active", loggedIn);
  return s;
}

// --- personal center ---

const PROFILE_H = 320;

const LEVEL_NAMES = {
  LEVEL_FREE: "免费版",
  LEVEL_BASIC: "基础会员",
  LEVEL_INTERMEDIATE: "中级会员",
  LEVEL_ADVANCED: "高级会员",
};
const SOURCE_NAMES = { qr: "扫码登录", sms: "手机验证码登录", web: "网页登录" };

function maskUid(uid) {
  if (!uid) return "未知用户";
  return uid.length > 12 ? uid.slice(0, 6) + "…" + uid.slice(-4) : uid;
}

function fmtDate(iso) {
  if (!iso) return "--";
  const t = new Date(iso);
  if (isNaN(t)) return "--";
  return `${t.getFullYear()}-${String(t.getMonth() + 1).padStart(2, "0")}-${String(t.getDate()).padStart(2, "0")}`;
}

async function openProfile() {
  showEl($("profilePanel"));
  await invoke("set_card_height", { height: PROFILE_H });
  $("profileUid").textContent = "加载中…";
  $("profileSource").textContent = "";
  let s;
  try {
    s = await invoke("get_profile");
  } catch (e) {
    s = { ok: false, error: String(e) };
  }
  const uid = s && s.userId;
  $("profileAvatar").textContent = (uid ? String(uid)[0] : "K").toUpperCase();
  $("profileUid").textContent = maskUid(uid);
  $("profileSource").textContent = s && s.ownSession
    ? (SOURCE_NAMES[s.source] || "本卡片登录")
    : "跟随 Kimi 桌面客户端";
  const m = s && s.membership;
  if (m && (m.plan || m.level)) {
    const level = LEVEL_NAMES[m.level] || m.level || "";
    $("profilePlan").textContent = (m.plan || "--") + (level ? " · " + level : "");
    $("profileStatus").textContent =
      m.active === true || m.status === "SUBSCRIPTION_STATUS_ACTIVE" ? "生效中" : (m.status || "--");
    $("profileExpire").textContent = fmtDate(m.periodEnd);
  } else {
    $("profilePlan").textContent = "--";
    $("profileStatus").textContent = "--";
    $("profileExpire").textContent = "--";
  }
}

async function closeProfile() {
  hideEl($("confirmMask"));
  hideEl($("profilePanel"));
  await invoke("set_card_height", { height: CARD_H });
  refreshSessionUI();
}

async function onAccountClick() {
  const s = await refreshSessionUI();
  const loggedIn = s && (s.ownSession || (s.followingDesktop && !s.loggedOut));
  if (loggedIn) {
    openProfile();
  } else {
    openLogin();
  }
}

// --- wiring ---

document.getElementById("refreshBtn").addEventListener("click", manualRefresh);
document.getElementById("retryBtn").addEventListener("click", manualRefresh);
document.getElementById("loginFromErrorBtn").addEventListener("click", openLogin);
document.getElementById("accountBtn").addEventListener("click", onAccountClick);
document.getElementById("loginCloseBtn").addEventListener("click", closeLogin);
document.getElementById("tabQr").addEventListener("click", () => switchLoginTab("qr"));
document.getElementById("tabSms").addEventListener("click", () => switchLoginTab("sms"));
document.getElementById("qrRefreshBtn").addEventListener("click", startQrLogin);
document.getElementById("smsSendBtn").addEventListener("click", sendSmsCode);
document.getElementById("smsLoginBtn").addEventListener("click", submitSmsLogin);
document.getElementById("webLoginBtn").addEventListener("click", () => invoke("open_web_login"));
document.getElementById("hideCardBtn").addEventListener("click", () => invoke("hide_card"));
document.getElementById("profileCloseBtn").addEventListener("click", closeProfile);
document.getElementById("logoutBtn").addEventListener("click", async () => {
  // 二次确认：文案区分自有会话与跟随桌面客户端两种登录态。
  const s = await refreshSessionUI();
  $("confirmText").textContent = s && s.ownSession
    ? "退出后卡片将显示未登录状态。"
    : "仅退出本卡片的登录，Kimi 桌面客户端不受影响。";
  showEl($("confirmMask"));
});
document.getElementById("confirmCancelBtn").addEventListener("click", () => hideEl($("confirmMask")));
document.getElementById("confirmOkBtn").addEventListener("click", async () => {
  hideEl($("confirmMask"));
  await invoke("logout");
  await refreshSessionUI();
  closeProfile();
  poll();
});
window.__kqbSession = refreshSessionUI;

// Drag the window from anywhere on the card except interactive controls.
document.querySelector(".card").addEventListener("mousedown", (e) => {
  if (e.button !== 0 || e.target.closest("button, input, canvas, .login-panel, .profile-panel, .confirm-mask")) return;
  window.__TAURI__.core.invoke("start_drag");
});

// --- ball mode (docked to the screen edge) ---

// Native side notifies via BOTH event emit and a direct eval of this
// function (eval survives the resize storm where events can get dropped).
window.__kqbBallMode = (on) => {
  document.body.classList.toggle("ball-mode", !!on);
  if (on) {
    // Fold panels WITHOUT closeLogin()/closeProfile(): those resize the
    // window via set_card_height, which would instantly undo ball mode.
    stopQrPolling();
    hideEl($("loginPanel"));
    hideEl($("profilePanel"));
    hideEl($("confirmMask"));
  }
  // 观赏模式粒子引擎只在「球态 + 观赏模式」运行；展开卡片即停帧省电。
  syncFx();
};
window.__TAURI__.event.listen("ball-mode", (e) => window.__kqbBallMode(!!e.payload));

// --- orb themes (悬浮球主题：深渊紫黑 / 翡翠庭园 / 独角兽彩虹) ---

const ORB_THEMES = ["abyss", "emerald", "rainbow"];

// 应用主题：切换 body 上的 theme-* class 并持久化到 localStorage。
// 托盘菜单通过 __kqbSetTheme 直接调用（eval 通道，事件可能丢）。
function applyOrbTheme(name) {
  if (!ORB_THEMES.includes(name)) name = "abyss";
  ORB_THEMES.forEach((t) => document.body.classList.remove("theme-" + t));
  document.body.classList.add("theme-" + name);
  try { localStorage.setItem("kqb-orb-theme", name); } catch (e) { /* private mode */ }
  // 观赏模式环内星尘随主题换色。注意：启动时本函数先于 showcase 模块
  // 的 let fxState 初始化被调用（TDZ），必须 try/catch 兜住，否则整个
  // 脚本会在此中断、额度完全不渲染。
  try { refreshFxColors(); } catch (e) { /* showcase 模块尚未初始化 */ }
  return name;
}
window.__kqbSetTheme = applyOrbTheme;

// 启动时恢复上次主题（默认深渊紫黑）。
let savedOrbTheme = null;
try { savedOrbTheme = localStorage.getItem("kqb-orb-theme"); } catch (e) { /* ignore */ }
const activeOrbTheme = applyOrbTheme(savedOrbTheme);

// --- orb metric (悬浮球显示口径：总量 / 5 小时窗口) ---

const ORB_METRICS = ["total", "5h"];
let lastPayload = null; // 最近一次 render 的原始数据，切口径时立即重绘球面

function applyOrbMetric(metric) {
  if (!ORB_METRICS.includes(metric)) metric = "total";
  orbMetric = metric;
  try { localStorage.setItem("kqb-orb-metric", metric); } catch (e) { /* ignore */ }
  document.getElementById("ball").title =
    (metric === "5h" ? "5 小时窗口用量" : "月度总量用量") + " — 点击展开";
  if (lastPayload) render(lastPayload);
}
let orbMetric = "total";
try { orbMetric = localStorage.getItem("kqb-orb-metric") || "total"; } catch (e) { /* ignore */ }
window.__kqbSetMetric = applyOrbMetric;

// --- orb showcase mode（观赏模式：可选增强渲染层，默认关闭） ---
// 关闭时 body 上无 showcase-mode class，新增层全部 display:none，
// UI 与旧版逐像素一致。开启后新增（参数 = 设计案例 v2.1 定稿值）：
//   1) 环内星尘画布：低密度 8 粒上升光尘 + 1 颗环内游星（带拖尾）
//   2) 最外圈 1 颗游星（CSS 动画，贴球体外缘反向慢转）
//   3) 进度弧末端小亮点（白色辉光点，位置跟随球面进度）
//   4) 环光晕收敛版（1.5px 贴身 + 4px 低透明扩散）
// 性能约束：粒子引擎只在「球态 + 观赏模式」运行；DPR 钳制 ≤ 2；
// 30fps 节流；页面隐藏自动停帧；prefers-reduced-motion 下只画静态一帧；
// 粒子「外晕+亮核」预烘焙为离屏精灵，每帧仅 drawImage 位图拷贝，
// 主题色/精灵全部缓存于 fxState，热路径零 DOM 查询。

const SHOWCASE_KEY = "kqb-orb-showcase";
const FX_PALETTE = {
  abyss:   { motes: ["#86efac", "#c4b5fd", "#ffffff", "#a78bfa", "#4ade80"], orbit: "#a7f3d0" },
  emerald: { motes: ["#d9f99d", "#86efac", "#ffffff", "#7dd3fc", "#fde68a"], orbit: "#fde68a" },
  rainbow: { motes: ["#fda4af", "#fde68a", "#86efac", "#7dd3fc", "#c4b5fd"], orbit: "#fda4af" },
};
const FX_MOTES = 8; // 低密度档（用户定稿）
const FX_REDUCED = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

let showcaseOn = false;
try { showcaseOn = localStorage.getItem(SHOWCASE_KEY) === "1"; } catch (e) { /* ignore */ }

function currentFxTheme() {
  for (const t of ORB_THEMES) {
    if (document.body.classList.contains("theme-" + t)) return FX_PALETTE[t];
  }
  return FX_PALETTE.abyss;
}

let fxState = null; // { ctx, R, dpr, palette, sprites, headSprite, orbitSprite, motes, orbiter, raf, last }

// 粒子精灵预烘焙：把「外晕(alpha 0.3) + 亮核」按每尺寸单位 K 像素画进
// 离屏画布一次，运行时每帧只需 drawImage（路径 arc/fill 是逐帧最贵的调用，
// 烘焙后 20 次路径绘制降为 12 次位图拷贝，观感与原绘制逐像素等价）。
const FX_K = 12; // 每粒子尺寸单位烘焙的像素数（最大粒 ~2.5 → 源图 ~60px，清晰）
function fxBakeSprite(color, withHalo, dpr) {
  const box = Math.ceil(FX_K * 2.4 * 2);
  const cv = document.createElement("canvas");
  cv.width = box * dpr;
  cv.height = box * dpr;
  const c = cv.getContext("2d");
  c.scale(dpr, dpr);
  if (withHalo) {
    c.globalAlpha = 0.3;
    c.fillStyle = color;
    c.beginPath(); c.arc(box / 2, box / 2, FX_K * 2.4, 0, 7); c.fill();
  }
  c.globalAlpha = 1;
  c.fillStyle = color;
  c.beginPath(); c.arc(box / 2, box / 2, FX_K, 0, 7); c.fill();
  return { cv, box };
}
// 按当前主题重建整套精灵（启动 + 换主题各一次，数量极少可忽略）
function fxBakeAll(dpr) {
  const palette = currentFxTheme();
  const sprites = {};
  for (const color of palette.motes) sprites[color] = fxBakeSprite(color, true, dpr);
  return {
    palette, sprites,
    headSprite: fxBakeSprite("#ffffff", false, dpr),
    orbitSprite: fxBakeSprite(palette.orbit, false, dpr),
  };
}

// 上升光尘：圆盘内均匀生成，上浮 + 左右微摆 + 闪烁，出顶后从底部回收
function fxSpawnMote(anywhere) {
  const R = fxState.R;
  const ang = Math.random() * Math.PI * 2;
  const rr = Math.sqrt(Math.random()) * 0.92;
  const pal = fxState.palette.motes;
  return {
    x: R + Math.cos(ang) * rr * R,
    y: anywhere ? R + Math.sin(ang) * rr * R : R * 1.9,
    vy: 10 + Math.random() * 16, // px/s 上浮
    sway: 2 + Math.random() * 4,
    swaySpd: 0.6 + Math.random() * 1.2,
    sz: 1.1 + Math.random() * 1.4,
    tw: Math.random() * Math.PI * 2,
    twSpd: 1.5 + Math.random() * 2.5,
    color: pal[(Math.random() * pal.length) | 0],
  };
}

function fxDraw(t, dt) {
  const { ctx, R, sprites } = fxState;
  const box = R * 2;
  ctx.clearRect(0, 0, box, box);
  ctx.globalCompositeOperation = "lighter"; // additive：亮而不脏
  for (const p of fxState.motes) {
    p.y -= p.vy * dt;
    if (p.y < -4) Object.assign(p, fxSpawnMote(false));
    const x = p.x + Math.sin(t * p.swaySpd + p.tw) * p.sway;
    ctx.globalAlpha = 0.25 + 0.6 * (0.5 + 0.5 * Math.sin(p.tw + t * p.twSpd));
    const spr = sprites[p.color];
    const w = (spr.box * p.sz) / FX_K;
    ctx.drawImage(spr.cv, x - w / 2, p.y - w / 2, w, w);
  }
  // 环内游星 1 颗：贴内缘游走 + 渐隐拖尾
  const o = fxState.orbiter;
  o.a += o.spd * dt;
  const pulse = 0.55 + 0.45 * Math.sin(o.tw + t * 2);
  for (let k = 3; k >= 0; k--) {
    const a2 = o.a - o.spd * k * 0.06;
    const x = R + Math.cos(a2) * o.r * R;
    const y = R + Math.sin(a2) * o.r * R;
    ctx.globalAlpha = Math.max((1 - k / 4) * pulse, 0) * (k === 0 ? 1 : 0.35);
    const spr = k === 0 ? fxState.headSprite : fxState.orbitSprite;
    const w = (spr.box * o.sz * (k === 0 ? 1 : 0.8)) / FX_K;
    ctx.drawImage(spr.cv, x - w / 2, y - w / 2, w, w);
  }
  ctx.globalAlpha = 1;
  ctx.globalCompositeOperation = "source-over";
}

function fxLoop(ts) {
  if (!fxState) return;
  if (!fxState.last) fxState.last = ts;
  const dtAll = (ts - fxState.last) / 1000;
  // 30fps 节流：粒子运动缓慢，30fps 与高刷观感一致，绘制量减半省 CPU
  if (dtAll < 1 / 30) {
    fxState.raf = requestAnimationFrame(fxLoop);
    return;
  }
  const dt = Math.min(dtAll, 0.05);
  fxState.last = ts;
  fxDraw(ts / 1000, dt);
  fxState.raf = requestAnimationFrame(fxLoop);
}

function startOrbFx() {
  if (fxState) return;
  const canvas = document.querySelector(".orb-fx");
  if (!canvas) return;
  // 球径以 :root 的 --orb-size 为准（与 Rust 侧 BALL_SIZE 对应）
  const size =
    parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--orb-size")) || 60;
  const box = Math.max(20, Math.round(size * 0.68)); // inset 16% → 画布为球的 68%
  const dpr = Math.min(window.devicePixelRatio || 1, 2); // DPR 钳制，避免 Retina 过采样
  canvas.width = box * dpr;
  canvas.height = box * dpr;
  canvas.style.width = box + "px";
  canvas.style.height = box + "px";
  const ctx = canvas.getContext("2d");
  ctx.scale(dpr, dpr);
  fxState = {
    ctx, R: box / 2, dpr, raf: 0, last: 0,
    ...fxBakeAll(dpr), // palette + sprites + headSprite + orbitSprite
    motes: Array.from({ length: FX_MOTES }, () => null),
    orbiter: {
      a: Math.random() * Math.PI * 2,
      r: 0.52, spd: 0.5, sz: 1.4,
      tw: Math.random() * Math.PI * 2,
    },
  };
  fxState.motes = fxState.motes.map(() => fxSpawnMote(true));
  if (FX_REDUCED) { fxDraw(0, 0); return; } // 降级：静态一帧，不进循环
  fxState.raf = requestAnimationFrame(fxLoop);
}

function stopOrbFx() {
  if (!fxState) return;
  cancelAnimationFrame(fxState.raf);
  const { ctx, R } = fxState;
  ctx.clearRect(0, 0, R * 2, R * 2);
  fxState = null;
}

// 粒子引擎的总开关：球态 + 观赏模式才运行，其余情况一律停帧清屏。
function syncFx() {
  if (showcaseOn && document.body.classList.contains("ball-mode")) startOrbFx();
  else stopOrbFx();
}

// 主题切换时环内星尘换色（applyOrbTheme 调用）：重建精灵并重刷粒子颜色
function refreshFxColors() {
  if (!fxState) return;
  Object.assign(fxState, fxBakeAll(fxState.dpr));
  const pal = fxState.palette.motes;
  for (const p of fxState.motes) p.color = pal[(Math.random() * pal.length) | 0];
  if (FX_REDUCED) fxDraw(0, 0); // 静态降级模式下换主题也要重绘那一帧
}

// 托盘菜单直推的开关入口；状态持久化，重启后由 sync_menus 回写勾选态。
function applyShowcase(on) {
  showcaseOn = !!on;
  document.body.classList.toggle("showcase-mode", showcaseOn);
  try { localStorage.setItem(SHOWCASE_KEY, showcaseOn ? "1" : "0"); } catch (e) { /* ignore */ }
  syncFx();
  return showcaseOn;
}
window.__kqbSetShowcase = applyShowcase;

// 启动即恢复上次的开关状态（默认关闭 = 与旧版一致）。
applyShowcase(showcaseOn);

// 页面隐藏自动停帧；恢复可见且引擎仍在运行（球态 + 观赏模式）时继续。
document.addEventListener("visibilitychange", () => {
  if (!fxState) return;
  if (document.hidden) {
    cancelAnimationFrame(fxState.raf);
    fxState.last = 0;
  } else if (!FX_REDUCED) {
    fxState.raf = requestAnimationFrame(fxLoop);
  }
});

// 观赏模式弧末亮点：跟随球面进度角位置（关闭时 CSS 隐藏，仅改属性）。
function setOrbTip(ratio) {
  const tip = document.getElementById("orb-tip");
  if (!tip) return;
  if (ratio == null) {
    tip.setAttribute("opacity", "0");
    return;
  }
  const clamped = Math.max(0, Math.min(1, ratio));
  const ang = ((-90 + 360 * clamped) * Math.PI) / 180;
  tip.setAttribute("cx", (36 + 30 * Math.cos(ang)).toFixed(2));
  tip.setAttribute("cy", (36 + 30 * Math.sin(ang)).toFixed(2));
  tip.setAttribute("opacity", "1");
}

// 启动时把前端持久化的主题 + 口径回写托盘勾选态（重启后菜单同步）。
invoke("sync_menus", { theme: activeOrbTheme, metric: orbMetric, showcase: showcaseOn }).catch(() => {});

// Ball gestures: a plain click expands the card; a real drag moves the ball
// (dropping it away from the edge also expands, handled natively on Moved).
const ballEl = document.getElementById("ball");
let ballDownAt = null;
let ballDragging = false;
ballEl.addEventListener("mousedown", (e) => {
  if (e.button !== 0) return;
  ballDownAt = { x: e.clientX, y: e.clientY };
  ballDragging = false;
});
ballEl.addEventListener("mousemove", (e) => {
  if (!ballDownAt || ballDragging) return;
  if (Math.hypot(e.clientX - ballDownAt.x, e.clientY - ballDownAt.y) > 6) {
    ballDragging = true;
    invoke("start_drag");
  }
});
window.addEventListener("mouseup", () => {
  if (ballDownAt && !ballDragging) invoke("expand_card");
  ballDownAt = null;
  ballDragging = false;
});

// Surface any JS error on the card itself (debug aid).
window.addEventListener("error", (e) => {
  const footer = document.getElementById("footer");
  footer.textContent = "JS: " + e.message;
  footer.style.opacity = "0.9";
});

// 双击标题「Kimi 额度」：展开态下自动吸附到最近的屏幕边缘缩成球。
document.querySelector(".title").addEventListener("dblclick", () => {
  invoke("dock_ball").catch(() => {});
});

poll();
setInterval(poll, 60000);
refreshSessionUI();
